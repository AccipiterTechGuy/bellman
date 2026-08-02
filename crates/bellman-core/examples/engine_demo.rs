//! Live demo of the near-horizon scheduler engine.
//!
//! Registers a 2 s interval timer and a daily timer, runs the loop long enough
//! for several interval fires, prints next-fires, and exits clean.
//!
//! ```text
//! cargo run -p bellman-core --example engine_demo
//! ```

use bellman_core::occurrence::{Occurrence, OccurrenceKind};
use bellman_core::scheduler::{FireAction, FireContext, Scheduler, SchedulerConfig, SystemClock};
use bellman_core::store::{MisfirePolicy, NewTimer, Store};
use chrono::{Duration as ChronoDuration, NaiveTime, Utc};
use std::path::PathBuf;
use std::time::Duration;

struct PrintAction;

impl FireAction for PrintAction {
    fn on_fire(&mut self, ctx: &FireContext<'_>) -> Result<(), String> {
        println!(
            "FIRE  name={:?} id={} scheduled_for={} run_id={} kind={:?}",
            ctx.timer.name,
            ctx.timer.id,
            ctx.scheduled_for.to_rfc3339(),
            ctx.run_id,
            ctx.kind
        );
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("bellman-engine-demo-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let db: PathBuf = dir.join("timers.db");
    println!("demo store: {}", db.display());

    let mut store = Store::open(&db)?;
    let now = Utc::now();

    // 2 s high-frequency interval, anchored at now so first fire is ~2 s out.
    let interval_occ = Occurrence::new(
        OccurrenceKind::Interval {
            every_secs: 2,
            anchor: now,
        },
        "UTC",
    )?;
    let mut interval = NewTimer::new("demo-interval-2s", interval_occ);
    interval.last_fired = Some(now);
    interval.misfire = MisfirePolicy::Skip;
    let interval = store.create_timer(interval)?;
    println!(
        "registered interval id={} next_fire={:?}",
        interval.id,
        interval.next_fire_utc.map(|t| t.to_rfc3339())
    );

    // Daily at 03:00 UTC — stays on the horizon without firing in the short demo window.
    let daily_at = NaiveTime::from_hms_opt(3, 0, 0).unwrap();
    let daily_occ = Occurrence::new(OccurrenceKind::Daily { at: daily_at }, "UTC")?;
    let mut daily = NewTimer::new("demo-daily-03:00", daily_occ);
    daily.last_fired = Some(now - ChronoDuration::hours(1));
    daily.misfire = MisfirePolicy::default_calendar();
    let daily = store.create_timer(daily)?;
    println!(
        "registered daily    id={} next_fire={:?}",
        daily.id,
        daily.next_fire_utc.map(|t| t.to_rfc3339())
    );

    let mut sched = Scheduler::new(
        store,
        SystemClock::new(),
        PrintAction,
        SchedulerConfig::default().with_max_sleep(Duration::from_millis(200)),
    );
    sched.boot()?;

    println!("heap size after boot: {}", sched.heap_len());
    if let Some((nf, id)) = sched.peek_next() {
        println!("heap head: {} @ {}", id, nf.to_rfc3339());
    }

    // Run ~5 s of wall time — expect about 2 interval fires (at t+2 and t+4).
    println!("running loop for ~5s …");
    let result = sched.run_for(Duration::from_secs(5))?;
    println!(
        "done. fires={} clock_jumps={} refilled={}",
        result.fires.len(),
        result.clock_jump,
        result.refilled
    );

    println!("next-fires after run:");
    for t in sched.store().list_timers()? {
        println!(
            "  {}  next={:?}  last={:?}",
            t.name,
            t.next_fire_utc.map(|x| x.to_rfc3339()),
            t.last_fired.map(|x| x.to_rfc3339())
        );
    }

    // Clean up demo db directory (best-effort).
    drop(sched);
    let _ = std::fs::remove_dir_all(&dir);
    println!("exit clean");
    Ok(())
}
