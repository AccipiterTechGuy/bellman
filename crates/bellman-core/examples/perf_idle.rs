//! Idle-footprint harness for P5 acceptance.
//!
//! Boots a real scheduler with one 1 s interval timer, a [`Dispatcher`] that
//! appends to a real JSONL event log, and samples RSS + CPU over a timed window.
//!
//! ```text
//! # Full acceptance window (10 min):
//! cargo run -p bellman-core --example perf_idle --release -- --secs 600
//!
//! # Quick smoke (60 s):
//! cargo run -p bellman-core --example perf_idle --release -- --secs 60
//! ```
//!
//! Writes evidence under `--data-dir` (default: a temp dir printed at start):
//! - `logs/events.current.jsonl` — fire evidence
//! - `perf_idle_report.json` — measured numbers + method

use bellman_core::actions::{Dispatcher, DispatcherConfig, ExecutorConfig};
use bellman_core::events::{read_events, RunState};
use bellman_core::occurrence::{Occurrence, OccurrenceKind};
use bellman_core::scheduler::{Scheduler, SchedulerConfig, SystemClock};
use bellman_core::store::{Action, MisfirePolicy, NewTimer, OpenOptions, Store};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut secs: u64 = 600;
    let mut data_dir: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--secs" => {
                secs = args
                    .next()
                    .ok_or("--secs needs a value")?
                    .parse()
                    .map_err(|e| format!("--secs: {e}"))?;
            }
            "--data-dir" => {
                data_dir = Some(PathBuf::from(args.next().ok_or("--data-dir needs a value")?));
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: perf_idle [--secs N] [--data-dir DIR]\n  default secs=600 (10 min)"
                );
                return Ok(());
            }
            other => return Err(format!("unknown arg: {other}").into()),
        }
    }

    let data_dir = match data_dir {
        Some(p) => {
            std::fs::create_dir_all(&p)?;
            p
        }
        None => {
            let p = std::env::temp_dir().join(format!("bellman-perf-idle-{}", std::process::id()));
            std::fs::create_dir_all(&p)?;
            p
        }
    };

    println!("perf_idle data_dir={}", data_dir.display());
    println!("window_secs={secs}");

    let db = data_dir.join("timers.db");
    let mut store = Store::open_with(
        &db,
        OpenOptions {
            refuse_network_fs: false,
            ..OpenOptions::default()
        },
    )?;

    let now = Utc::now();
    let occ = Occurrence::new(
        OccurrenceKind::Interval {
            every_secs: 1,
            anchor: now,
        },
        "UTC",
    )?;
    let mut t = NewTimer::new("perf-1s", occ);
    t.last_fired = Some(now);
    t.misfire = MisfirePolicy::Skip;
    t.action = Action::None;
    let timer = store.create_timer(t)?;
    println!(
        "timer id={} next={:?}",
        timer.id,
        timer.next_fire_utc.map(|d| d.to_rfc3339())
    );

    let logs_dir = data_dir.join("logs");
    let dispatcher = Dispatcher::spawn(DispatcherConfig {
        db_path: db.clone(),
        data_dir: Some(data_dir.clone()),
        max_concurrent_actions: 4,
        notify_sink: std::sync::Arc::new(bellman_core::actions::StubNotifySink),
        executor: ExecutorConfig {
            skip_retry_sleep: true,
            ..ExecutorConfig::default()
        },
        tick: Duration::from_millis(200),
    })?;

    let cfg = SchedulerConfig::default()
        .with_max_sleep(Duration::from_millis(200))
        .with_data_dir(data_dir.clone());
    let mut sched = Scheduler::new(store, SystemClock::new(), dispatcher, cfg);
    sched.boot()?;

    let pid = std::process::id();
    let rss_start_kb = read_vm_rss_kb(pid)?;
    let (utime0, stime0) = read_proc_times(pid)?;
    let wall0 = Instant::now();

    let mut rss_samples: Vec<u64> = vec![rss_start_kb];
    let sample_every = Duration::from_secs(secs.clamp(5, 30));
    let end = wall0 + Duration::from_secs(secs);
    let mut next_sample = wall0 + sample_every;

    println!(
        "running… pid={pid} rss_start_kb={rss_start_kb} sample_every={sample_every:?}"
    );

    // Drive the scheduler for the whole window.
    while Instant::now() < end {
        let remain = end.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            break;
        }
        let slice = remain.min(Duration::from_millis(500));
        let _ = sched.run_for(slice)?;
        let now_i = Instant::now();
        if now_i >= next_sample {
            if let Ok(kb) = read_vm_rss_kb(pid) {
                rss_samples.push(kb);
                println!(
                    "sample t={:.1}s rss_kb={kb}",
                    now_i.duration_since(wall0).as_secs_f64()
                );
            }
            next_sample = now_i + sample_every;
        }
    }

    let wall_elapsed = wall0.elapsed();
    let (utime1, stime1) = read_proc_times(pid)?;
    let rss_end_kb = read_vm_rss_kb(pid).unwrap_or(rss_start_kb);
    rss_samples.push(rss_end_kb);

    // Count fired events in the JSONL (primary wake evidence). Events are
    // enqueued into the outbox (R11) — drain before reading.
    {
        let drain_store = Store::open_with(&db, OpenOptions {
            refuse_network_fs: false,
            ..OpenOptions::default()
        })?;
        bellman_core::events::EventPublisher::drain_best_effort(&data_dir, &drain_store);
    }
    let log_path = logs_dir.join("events.current.jsonl");
    let (recs, stats) = if log_path.exists() {
        read_events(&log_path)?
    } else {
        (vec![], Default::default())
    };
    // Fire rate = `fired` (+ late) only. `wake_delivered` is a second line per
    // fire and must not double-count wakeups/min.
    let fired = recs
        .iter()
        .filter(|r| matches!(r.kind, RunState::Fired | RunState::FiredLate))
        .count();
    let wake_delivered = recs
        .iter()
        .filter(|r| r.kind == RunState::WakeDelivered)
        .count();
    let elapsed_min = wall_elapsed.as_secs_f64() / 60.0;
    let wakeups_per_min = if elapsed_min > 0.0 {
        fired as f64 / elapsed_min
    } else {
        0.0
    };

    // CPU% ≈ (user+sys jiffies) / (wall * Hz) * 100
    let hz = proc_clk_tck();
    let cpu_jiffies = (utime1 + stime1).saturating_sub(utime0 + stime0);
    let wall_jiffies = wall_elapsed.as_secs_f64() * hz as f64;
    let cpu_pct = if wall_jiffies > 0.0 {
        (cpu_jiffies as f64 / wall_jiffies) * 100.0
    } else {
        0.0
    };

    let rss_min = *rss_samples.iter().min().unwrap_or(&rss_start_kb);
    let rss_max = *rss_samples.iter().max().unwrap_or(&rss_end_kb);
    let rss_median = {
        let mut v = rss_samples.clone();
        v.sort_unstable();
        v[v.len() / 2]
    };

    let report = serde_json::json!({
        "method": "engine-only SystemClock + SCH1 Dispatcher + R11 outbox/publisher; one 1s interval Action::None",
        "pid": pid,
        "data_dir": data_dir.display().to_string(),
        "window_secs_requested": secs,
        "wall_elapsed_secs": wall_elapsed.as_secs_f64(),
        "rss_start_kb": rss_start_kb,
        "rss_end_kb": rss_end_kb,
        "rss_min_kb": rss_min,
        "rss_max_kb": rss_max,
        "rss_median_kb": rss_median,
        "rss_samples_kb": rss_samples,
        "cpu_jiffies": cpu_jiffies,
        "cpu_pct_over_window": cpu_pct,
        "clk_tck": hz,
        "event_log_path": log_path.display().to_string(),
        "event_lines_total": recs.len(),
        "event_lines_skipped": stats.skipped,
        "fired_count": fired,
        "wake_delivered_count": wake_delivered,
        "wakeups_per_min": wakeups_per_min,
        "host": uname_s(),
        "measured_at": Utc::now().to_rfc3339(),
    });

    let report_path = data_dir.join("perf_idle_report.json");
    std::fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;

    // Also copy evidence into the repo docs tree when BELLMAN_PERF_OUT is set.
    if let Ok(out) = std::env::var("BELLMAN_PERF_OUT") {
        let out = PathBuf::from(out);
        std::fs::create_dir_all(&out)?;
        std::fs::copy(&report_path, out.join("perf_idle_report.json"))?;
        if log_path.exists() {
            std::fs::copy(&log_path, out.join("events.current.jsonl"))?;
        }
        println!("copied evidence to {}", out.display());
    }

    println!("--- RESULT ---");
    println!("rss_median_kib={rss_median}  rss_min_kib={rss_min}  rss_max_kib={rss_max}");
    println!("cpu_pct={cpu_pct:.4}  (jiffies={cpu_jiffies}, hz={hz})");
    println!("fires={fired}  wall_min={elapsed_min:.3}  wakeups_per_min={wakeups_per_min:.2}");
    println!("event_log={}", log_path.display());
    println!("report={}", report_path.display());
    println!("{}", serde_json::to_string_pretty(&report)?);

    Ok(())
}

fn read_vm_rss_kb(pid: u32) -> Result<u64, Box<dyn std::error::Error>> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest
                .split_whitespace()
                .next()
                .ok_or("VmRSS parse")?
                .parse()?;
            return Ok(kb);
        }
    }
    Err("VmRSS not found".into())
}

/// (utime, stime) in clock ticks from `/proc/pid/stat` fields 14 and 15.
fn read_proc_times(pid: u32) -> Result<(u64, u64), Box<dyn std::error::Error>> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    // comm can contain spaces/parens — split after last ')' then fields are 3…
    let after = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest)
        .ok_or("stat parse")?;
    let fields: Vec<&str> = after.split_whitespace().collect();
    // After ')': field 3 is state, so utime is index 11 (fields 14-3=11), stime 12.
    let utime: u64 = fields.get(11).ok_or("utime")?.parse()?;
    let stime: u64 = fields.get(12).ok_or("stime")?.parse()?;
    Ok((utime, stime))
}

fn proc_clk_tck() -> u64 {
    // Avoid a libc crate dep: getconf is portable enough on Linux.
    let v: i64 = std::process::Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse()
                .ok()
        })
        .unwrap_or(100);
    if v > 0 {
        v as u64
    } else {
        100
    }
}

fn uname_s() -> String {
    std::process::Command::new("uname")
        .arg("-a")
        .output()
        .ok()
        .map_or_else(
            || "unknown".into(),
            |o| String::from_utf8_lossy(&o.stdout).trim().to_string(),
        )
}

// Silence unused import if Path only used in docs.
#[allow(dead_code)]
fn _path_ty(_: &Path) {}
