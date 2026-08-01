//! Simulated-clock acceptance tests for the scheduler engine.

use super::*;
use crate::occurrence::{Occurrence, OccurrenceKind};
use crate::store::{MisfirePolicy, NewTimer, Store, TimerPatch, TimerUpdate};
use chrono::{Duration as ChronoDuration, NaiveTime, TimeZone, Utc};
use std::time::Duration;

fn open_tmp() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("timers.db");
    let store = Store::open(&path).expect("open");
    (dir, store)
}

/// Fixed epoch so next_fire math is independent of real Utc::now().
fn epoch() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2030, 6, 1, 12, 0, 0).unwrap()
}

fn interval_timer(
    store: &mut Store,
    name: &str,
    every_secs: u64,
    anchor: chrono::DateTime<Utc>,
    last_fired: Option<chrono::DateTime<Utc>>,
    misfire: MisfirePolicy,
) -> crate::store::Timer {
    let occ = Occurrence::new(OccurrenceKind::Interval { every_secs, anchor }, "UTC").unwrap();
    let mut new = NewTimer::new(name, occ);
    new.last_fired = last_fired;
    new.misfire = misfire;
    store.create_timer(new).unwrap()
}

fn daily_timer(
    store: &mut Store,
    name: &str,
    at: NaiveTime,
    last_fired: Option<chrono::DateTime<Utc>>,
    misfire: MisfirePolicy,
) -> crate::store::Timer {
    let occ = Occurrence::new(OccurrenceKind::Daily { at }, "UTC").unwrap();
    let mut new = NewTimer::new(name, occ);
    new.last_fired = last_fired;
    new.misfire = misfire;
    store.create_timer(new).unwrap()
}

#[test]
fn suspend_resume_oversleep_recovered_interval_skip() {
    let (_dir, mut store) = open_tmp();
    let t0 = epoch();
    let clock = SimulatedClock::new(t0);

    // 10 s interval; last fired at t0 so next = t0+10.
    let timer = interval_timer(&mut store, "hf", 10, t0, Some(t0), MisfirePolicy::Skip);
    assert_eq!(
        timer.next_fire_utc.unwrap(),
        t0 + ChronoDuration::seconds(10)
    );

    let mut sched = Scheduler::new(
        store,
        clock.clone(),
        RecordingAction::new(),
        SchedulerConfig::default().with_jump_threshold(Duration::from_secs(3)),
    );
    sched.boot().unwrap();

    // Normal advance to first fire.
    clock.advance(Duration::from_secs(10));
    let r = sched.tick().unwrap();
    assert_eq!(r.fires.len(), 1);
    assert_eq!(r.fires[0].timer_id, timer.id);

    // Suspend: wall jumps +1 h, mono barely moves (already advanced in sleep path
    // we simulate by wall-only jump after a mono tick baseline update).
    // After the previous tick, last_wall/mono are synced. Jump wall only.
    clock.advance_wall_only(Duration::from_secs(3600));
    let r = sched.tick().unwrap();
    assert!(r.clock_jump, "suspend must look like a clock jump");
    // Skip policy + grace = one period (10 s): 1 h >> 10 s ⇒ no recovery fire.
    assert!(
        r.fires.is_empty(),
        "interval skip must not fire missed backlog, got {}",
        r.fires.len()
    );

    let next = sched
        .store()
        .get_timer(timer.id)
        .unwrap()
        .unwrap()
        .next_fire_utc
        .unwrap();
    let now = clock.wall_now();
    assert!(
        next > now,
        "next_fire {next} must be strictly after now {now}"
    );

    // Next interval after resume should still fire.
    let wait = (next - now).to_std().unwrap();
    clock.advance(wait);
    let r = sched.tick().unwrap();
    assert_eq!(r.fires.len(), 1, "post-recovery interval fires once");
}

#[test]
fn weekend_gap_daily_coalesce_fires_once() {
    let (_dir, mut store) = open_tmp();
    // Daily at 09:00 UTC. Start Thursday 08:00 so next is Thursday 09:00, then
    // jump the wall clock over the weekend. Default calendar grace is 1 h:
    // Thu–Sun are out of grace at Monday 10:00, but Monday 09:00 is inside
    // grace and must fire once (coalesce), not be dropped with the oldest miss.
    let at = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
    let thursday_morning = Utc.with_ymd_and_hms(2030, 6, 6, 8, 0, 0).unwrap();
    let clock = SimulatedClock::new(thursday_morning);

    // last_fired = Wednesday 09:00 ⇒ next = Thursday 09:00
    let last = Utc.with_ymd_and_hms(2030, 6, 5, 9, 0, 0).unwrap();
    let timer = daily_timer(
        &mut store,
        "daily",
        at,
        Some(last),
        MisfirePolicy::default_calendar(),
    );
    let expected_thu = Utc.with_ymd_and_hms(2030, 6, 6, 9, 0, 0).unwrap();
    assert_eq!(timer.next_fire_utc.unwrap(), expected_thu);

    let mut sched = Scheduler::new(
        store,
        clock.clone(),
        RecordingAction::new(),
        SchedulerConfig::default(),
    );
    sched.boot().unwrap();

    // Laptop closed all weekend: wall → Monday 10:00, mono frozen relative to last tick.
    let monday = Utc.with_ymd_and_hms(2030, 6, 10, 10, 0, 0).unwrap();
    let monday_nine = Utc.with_ymd_and_hms(2030, 6, 10, 9, 0, 0).unwrap();
    clock.set_wall(monday);
    let r = sched.tick().unwrap();
    assert!(r.clock_jump);
    assert_eq!(
        r.fires.len(),
        1,
        "coalesce must deliver exactly one recovery fire, got {}",
        r.fires.len()
    );
    assert_eq!(r.fires[0].timer_id, timer.id);
    assert_eq!(
        r.fires[0].scheduled_for, monday_nine,
        "must fire the in-grace Monday 09:00 slot, not the oldest Thursday miss"
    );
    match &r.fires[0].kind {
        FireKind::Coalesced { missed_count } => assert!(*missed_count >= 2),
        FireKind::Late { .. } | FireKind::OnTime => {}
        other => panic!("unexpected kind: {other:?}"),
    }

    let r2 = sched.tick().unwrap();
    assert!(r2.fires.is_empty());

    let t = sched.store().get_timer(timer.id).unwrap().unwrap();
    assert!(t.next_fire_utc.unwrap() > monday);
}

#[test]
fn interval_skips_missed_beyond_grace() {
    let (_dir, mut store) = open_tmp();
    let t0 = epoch();
    let clock = SimulatedClock::new(t0);
    let timer = interval_timer(&mut store, "iv", 60, t0, Some(t0), MisfirePolicy::Skip);

    let mut sched = Scheduler::new(
        store,
        clock.clone(),
        RecordingAction::new(),
        SchedulerConfig::default(),
    );
    sched.boot().unwrap();

    // Jump wall by 10 minutes (10 missed periods); mono frozen ⇒ jump + skip.
    clock.advance_wall_only(Duration::from_secs(600));
    let r = sched.tick().unwrap();
    assert!(r.clock_jump);
    assert!(r.fires.is_empty(), "skip must drop the backlog");
    assert_eq!(sched.action().len(), 0);

    let next = sched
        .store()
        .get_timer(timer.id)
        .unwrap()
        .unwrap()
        .next_fire_utc
        .unwrap();
    assert!(next > clock.wall_now());
}

#[test]
fn backward_jump_refires_nothing() {
    let (_dir, mut store) = open_tmp();
    let t0 = epoch();
    let clock = SimulatedClock::new(t0);

    // One-shot at t0+30s; seed last_fired = t0 so next is independent of real now.
    let at = (t0 + ChronoDuration::seconds(30)).naive_utc();
    let mut new = NewTimer::new(
        "once",
        Occurrence::new(OccurrenceKind::Once { at }, "UTC").unwrap(),
    );
    new.last_fired = Some(t0);
    new.misfire = MisfirePolicy::Coalesce { grace_secs: 3600 };
    let timer = store.create_timer(new).unwrap();
    assert_eq!(
        timer.next_fire_utc.unwrap(),
        t0 + ChronoDuration::seconds(30)
    );

    let mut sched = Scheduler::new(
        store,
        clock.clone(),
        RecordingAction::new(),
        SchedulerConfig::default(),
    );
    sched.boot().unwrap();

    // Fire the one-shot.
    clock.advance(Duration::from_secs(30));
    let r = sched.tick().unwrap();
    assert_eq!(r.fires.len(), 1);

    // Wall jumps backward past the fire time.
    clock.jump_wall_backward(Duration::from_secs(60));
    let r = sched.tick().unwrap();
    assert!(r.clock_jump);
    assert!(
        r.fires.is_empty(),
        "backward jump must not re-fire completed one-shot"
    );

    let t = sched.store().get_timer(timer.id).unwrap().unwrap();
    assert!(t.next_fire_utc.is_none(), "one-shot exhausted");
    assert_eq!(t.last_fired, Some(t0 + ChronoDuration::seconds(30)));
}

#[test]
fn horizon_refill_on_edit() {
    let (_dir, mut store) = open_tmp();
    let t0 = epoch();
    let clock = SimulatedClock::new(t0);

    // Daily far in the future relative to short horizon — start with no near fire.
    let at = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
    // last_fired today 09:00 at t0=12:00 ⇒ next = tomorrow 09:00.
    let last = Utc.with_ymd_and_hms(2030, 6, 1, 9, 0, 0).unwrap();
    let timer = daily_timer(
        &mut store,
        "daily",
        at,
        Some(last),
        MisfirePolicy::default_calendar(),
    );
    let tomorrow_9 = Utc.with_ymd_and_hms(2030, 6, 2, 9, 0, 0).unwrap();
    assert_eq!(timer.next_fire_utc.unwrap(), tomorrow_9);

    // Horizon of 1 hour: tomorrow is outside.
    let mut sched = Scheduler::new(
        store,
        clock.clone(),
        RecordingAction::new(),
        SchedulerConfig::default().with_horizon(Duration::from_secs(3600)),
    );
    sched.boot().unwrap();
    assert_eq!(sched.heap_len(), 0, "tomorrow is outside 1h horizon");

    // Edit: reschedule to fire in 5 minutes via a new once occurrence... or
    // patch last_fired so next daily is soon. Easier: switch to 2s interval.
    let handle = sched.control_handle();
    {
        let store = sched.store_mut();
        let cur = store.get_timer(timer.id).unwrap().unwrap();
        let occ = Occurrence::new(
            OccurrenceKind::Interval {
                every_secs: 2,
                anchor: t0,
            },
            "UTC",
        )
        .unwrap();
        store
            .update_timer(TimerUpdate {
                id: timer.id,
                expected_revision: cur.revision,
                patch: TimerPatch {
                    occurrence: Some(occ),
                    last_fired: Some(Some(t0)),
                    misfire: Some(MisfirePolicy::Skip),
                    ..Default::default()
                },
            })
            .unwrap();
    }
    handle.refill();
    let r = sched.tick().unwrap();
    assert!(r.refilled);
    assert!(
        sched.heap_len() >= 1,
        "refill after edit must load the new near fire"
    );
    let (nf, id) = sched.peek_next().unwrap();
    assert_eq!(id, timer.id);
    assert_eq!(nf, t0 + ChronoDuration::seconds(2));
}

#[test]
fn grace_boundary_coalesce_honored() {
    let (_dir, mut store) = open_tmp();
    let t0 = epoch();
    let at = NaiveTime::from_hms_opt(12, 0, 0).unwrap();
    // last_fired yesterday 12:00 ⇒ next = t0 (today 12:00).
    let yesterday = t0 - ChronoDuration::days(1);
    let timer = daily_timer(
        &mut store,
        "grace",
        at,
        Some(yesterday),
        MisfirePolicy::Coalesce { grace_secs: 3600 },
    );
    assert_eq!(timer.next_fire_utc.unwrap(), t0);

    // Case A: lateness == grace (1 h) ⇒ fire.
    let clock = SimulatedClock::new(t0 + ChronoDuration::seconds(3600));
    let mut sched = Scheduler::new(
        store,
        clock.clone(),
        RecordingAction::new(),
        SchedulerConfig::default(),
    );
    // boot sees overdue within grace.
    let r = {
        // boot runs misfire_pass which delivers.
        sched.boot().unwrap();
        // boot already misfire-passed; collect via action.
        sched.action().len()
    };
    assert_eq!(r, 1, "lateness == grace must fire");

    // Case B: lateness == grace + 1 ⇒ skip.
    let (_dir2, mut store2) = open_tmp();
    let timer2 = daily_timer(
        &mut store2,
        "grace2",
        at,
        Some(yesterday),
        MisfirePolicy::Coalesce { grace_secs: 3600 },
    );
    let clock2 = SimulatedClock::new(t0 + ChronoDuration::seconds(3601));
    let mut sched2 = Scheduler::new(
        store2,
        clock2.clone(),
        RecordingAction::new(),
        SchedulerConfig::default(),
    );
    sched2.boot().unwrap();
    assert_eq!(sched2.action().len(), 0, "lateness > grace must skip");
    let next = sched2
        .store()
        .get_timer(timer2.id)
        .unwrap()
        .unwrap()
        .next_fire_utc
        .unwrap();
    assert!(next > clock2.wall_now());
}

#[test]
fn catch_up_respects_max_cap() {
    let (_dir, mut store) = open_tmp();
    let t0 = epoch();
    let clock = SimulatedClock::new(t0);
    let timer = interval_timer(
        &mut store,
        "cu",
        10,
        t0,
        Some(t0),
        MisfirePolicy::CatchUp {
            grace_secs: 3600,
            max_catch_up: 3,
        },
    );
    assert_eq!(
        timer.next_fire_utc.unwrap(),
        t0 + ChronoDuration::seconds(10)
    );

    let mut sched = Scheduler::new(
        store,
        clock.clone(),
        RecordingAction::new(),
        SchedulerConfig::default(),
    );
    sched.boot().unwrap();

    // 100 s later ⇒ 10 missed; cap 3.
    clock.advance_wall_only(Duration::from_secs(100));
    let r = sched.tick().unwrap();
    assert!(r.clock_jump);
    assert_eq!(r.fires.len(), 3, "catch_up must honor max_catch_up");
    for (i, f) in r.fires.iter().enumerate() {
        assert_eq!(f.kind, FireKind::CatchUp { index: i as u32 });
    }
}

#[test]
fn claim_before_work_writes_run_row() {
    let (_dir, mut store) = open_tmp();
    let t0 = epoch();
    let clock = SimulatedClock::new(t0);
    let timer = interval_timer(&mut store, "claim", 5, t0, Some(t0), MisfirePolicy::Skip);

    let mut sched = Scheduler::new(
        store,
        clock.clone(),
        RecordingAction::new(),
        SchedulerConfig::default(),
    );
    sched.boot().unwrap();
    clock.advance(Duration::from_secs(5));
    let r = sched.tick().unwrap();
    assert_eq!(r.fires.len(), 1);

    let run = sched
        .store()
        .get_run(r.fires[0].run_id)
        .unwrap()
        .expect("run row");
    assert_eq!(run.timer_id, timer.id);
    assert_eq!(run.status, crate::store::ClaimStatus::Finished);
    assert_eq!(run.outcome, Some(crate::store::RunOutcome::WakeDelivered));
}

#[test]
fn high_frequency_stays_on_heap_outside_short_horizon() {
    let (_dir, mut store) = open_tmp();
    let t0 = epoch();
    let clock = SimulatedClock::new(t0);
    // 2-minute interval (< 5 min HF threshold); next = t0+120.
    let timer = interval_timer(&mut store, "hf", 120, t0, Some(t0), MisfirePolicy::Skip);
    // Horizon only 30 s — next at +120 is outside timers_due_by, but HF must load.
    let mut sched = Scheduler::new(
        store,
        clock,
        RecordingAction::new(),
        SchedulerConfig::default().with_horizon(Duration::from_secs(30)),
    );
    sched.boot().unwrap();
    assert_eq!(sched.heap_len(), 1);
    assert_eq!(sched.peek_next().unwrap().1, timer.id);
}

#[test]
fn chunked_sleep_capped_at_max_sleep() {
    let (_dir, mut store) = open_tmp();
    let t0 = epoch();
    let clock = SimulatedClock::new(t0);
    let _ = interval_timer(&mut store, "far", 600, t0, Some(t0), MisfirePolicy::Skip);
    let mut sched = Scheduler::new(
        store,
        clock,
        RecordingAction::new(),
        SchedulerConfig::default()
            .with_max_sleep(Duration::from_secs(30))
            .with_horizon(Duration::from_secs(3600)),
    );
    sched.boot().unwrap();
    // next fire in 600 s, max_sleep 30 ⇒ sleep 30.
    assert_eq!(sched.next_sleep(), Duration::from_secs(30));
}

#[test]
fn run_for_interval_fires_multiple_times() {
    let (_dir, mut store) = open_tmp();
    let t0 = epoch();
    let clock = SimulatedClock::new(t0);
    let timer = interval_timer(&mut store, "iv", 2, t0, Some(t0), MisfirePolicy::Skip);
    let daily_at = NaiveTime::from_hms_opt(3, 0, 0).unwrap();
    let _daily = daily_timer(
        &mut store,
        "daily",
        daily_at,
        Some(t0 - ChronoDuration::hours(1)),
        MisfirePolicy::default_calendar(),
    );

    let mut sched = Scheduler::new(
        store,
        clock.clone(),
        RecordingAction::new(),
        SchedulerConfig::default().with_max_sleep(Duration::from_millis(200)),
    );
    sched.boot().unwrap();
    let r = sched.run_for(Duration::from_secs(5)).unwrap();
    // fires at t0+2 and t0+4 (t0+6 is at the boundary — may or may not)
    assert!(
        r.fires.len() >= 2,
        "expected >=2 interval fires in 5s, got {} ({:?})",
        r.fires.len(),
        r.fires.iter().map(|f| f.scheduled_for).collect::<Vec<_>>()
    );
    assert!(r.fires.iter().all(|f| f.timer_id == timer.id));
}

#[test]
fn mark_fired_advances_next_fire() {
    let (_dir, mut store) = open_tmp();
    let t0 = epoch();
    let clock = SimulatedClock::new(t0);
    let timer = interval_timer(&mut store, "iv", 2, t0, Some(t0), MisfirePolicy::Skip);
    assert_eq!(
        timer.next_fire_utc.unwrap(),
        t0 + ChronoDuration::seconds(2)
    );

    let mut sched = Scheduler::new(
        store,
        clock.clone(),
        RecordingAction::new(),
        SchedulerConfig::default(),
    );
    sched.boot().unwrap();
    clock.advance(Duration::from_secs(2));
    let r = sched.tick().unwrap();
    assert_eq!(r.fires.len(), 1, "fires: {:?}", r.fires);
    let t = sched.store().get_timer(timer.id).unwrap().unwrap();
    assert_eq!(t.last_fired, Some(t0 + ChronoDuration::seconds(2)));
    assert_eq!(
        t.next_fire_utc,
        Some(t0 + ChronoDuration::seconds(4)),
        "next must advance past last_fired; last={:?} next={:?}",
        t.last_fired,
        t.next_fire_utc
    );
}

#[test]
fn system_clock_interval_advances_and_refires() {
    let (_dir, mut store) = open_tmp();
    let now = Utc::now();
    let occ = Occurrence::new(
        OccurrenceKind::Interval {
            every_secs: 1,
            anchor: now,
        },
        "UTC",
    )
    .unwrap();
    let mut new = NewTimer::new("sys", occ);
    new.last_fired = Some(now);
    new.misfire = MisfirePolicy::Skip;
    let timer = store.create_timer(new).unwrap();
    let first_next = timer.next_fire_utc.unwrap();
    assert!(first_next > now);

    let mut sched = Scheduler::new(
        store,
        SystemClock::new(),
        RecordingAction::new(),
        SchedulerConfig::default().with_max_sleep(Duration::from_millis(50)),
    );
    sched.boot().unwrap();
    let r = sched.run_for(Duration::from_millis(2500)).unwrap();
    assert!(
        r.fires.len() >= 2,
        "expected >=2 fires in 2.5s on 1s interval, got {} last={:?} next={:?}",
        r.fires.len(),
        sched
            .store()
            .get_timer(timer.id)
            .unwrap()
            .unwrap()
            .last_fired,
        sched
            .store()
            .get_timer(timer.id)
            .unwrap()
            .unwrap()
            .next_fire_utc,
    );
    let t = sched.store().get_timer(timer.id).unwrap().unwrap();
    assert!(t.next_fire_utc.unwrap() > t.last_fired.unwrap());
}

/// Regression: nanos-precision timestamps must round-trip so next_fire advances
/// after a fire whose `scheduled_for` carried sub-millisecond components.
#[test]
fn store_update_after_fire_advances_subsecond_interval() {
    let (_dir, mut store) = open_tmp();
    let now = Utc::now();
    let occ = Occurrence::new(
        OccurrenceKind::Interval {
            every_secs: 2,
            anchor: now,
        },
        "UTC",
    )
    .unwrap();
    let mut new = NewTimer::new("x", occ);
    new.last_fired = Some(now);
    let timer = store.create_timer(new).unwrap();
    let scheduled = timer.next_fire_utc.unwrap();

    let mut occ = timer.occurrence.clone();
    occ.record_run();
    let updated = store
        .update_timer(TimerUpdate {
            id: timer.id,
            expected_revision: timer.revision,
            patch: TimerPatch {
                last_fired: Some(Some(scheduled)),
                occurrence: Some(occ),
                ..Default::default()
            },
        })
        .unwrap();
    assert_eq!(updated.last_fired, Some(scheduled));
    assert!(
        updated.next_fire_utc.unwrap() > scheduled,
        "next {:?} must be > scheduled {:?}",
        updated.next_fire_utc,
        scheduled
    );
}

#[test]
fn crash_after_claim_recovers_pending_action() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("timers.db");
    let t0 = epoch();
    let scheduled = t0 + ChronoDuration::seconds(10);

    let timer_id = {
        let mut store = Store::open(&path).unwrap();
        let timer = interval_timer(&mut store, "crash", 10, t0, Some(t0), MisfirePolicy::Skip);
        assert_eq!(timer.next_fire_utc.unwrap(), scheduled);
        // Crash boundary: claim written, action never ran, process dies.
        let claim = store.claim_run(timer.id, scheduled).unwrap();
        assert_eq!(claim.status, crate::store::ClaimStatus::Pending);
        timer.id
    };

    // Reopen + boot must recover the pending claim via the action callback.
    let store = Store::open(&path).unwrap();
    assert_eq!(store.pending_claims().unwrap().len(), 1);

    let clock = SimulatedClock::new(scheduled + ChronoDuration::seconds(1));
    let mut sched = Scheduler::new(
        store,
        clock,
        RecordingAction::new(),
        SchedulerConfig::default(),
    );
    let boot_fires = sched.boot().unwrap();
    assert_eq!(
        boot_fires.len(),
        1,
        "pending claim must recover into one delivered fire"
    );
    assert_eq!(boot_fires[0].timer_id, timer_id);
    assert_eq!(boot_fires[0].scheduled_for, scheduled);
    assert_eq!(sched.action().len(), 1);
    assert!(
        sched.store().pending_claims().unwrap().is_empty(),
        "claim must be completed after recovery"
    );
    let run = sched
        .store()
        .get_run(boot_fires[0].run_id)
        .unwrap()
        .unwrap();
    assert_eq!(run.status, crate::store::ClaimStatus::Finished);
    assert_eq!(run.outcome, Some(crate::store::RunOutcome::WakeDelivered));
}

#[test]
fn catch_up_skips_old_delivers_recent_within_grace() {
    let (_dir, mut store) = open_tmp();
    let t0 = epoch();
    let clock = SimulatedClock::new(t0);
    // 10 s interval; grace 30 s; max 10.
    let timer = interval_timer(
        &mut store,
        "cu-mix",
        10,
        t0,
        Some(t0),
        MisfirePolicy::CatchUp {
            grace_secs: 30,
            max_catch_up: 10,
        },
    );
    assert_eq!(
        timer.next_fire_utc.unwrap(),
        t0 + ChronoDuration::seconds(10)
    );

    let mut sched = Scheduler::new(
        store,
        clock.clone(),
        RecordingAction::new(),
        SchedulerConfig::default(),
    );
    sched.boot().unwrap();

    // Jump 100 s: misses at +10..+100. Only +70,+80,+90,+100 are within 30 s of
    // now=t0+100? late for +70 = 30 s → in grace; +60 late=40 → out.
    // Misses: 10,20,30,40,50,60,70,80,90,100 (10 slots).
    // In grace (late <= 30): 70 (30), 80 (20), 90 (10), 100 (0) → 4 fires.
    clock.advance_wall_only(Duration::from_secs(100));
    let r = sched.tick().unwrap();
    assert!(r.clock_jump);
    assert_eq!(
        r.fires.len(),
        4,
        "catch_up must skip out-of-grace then deliver recent; got {:?}",
        r.fires.iter().map(|f| f.scheduled_for).collect::<Vec<_>>()
    );
    let expected = [
        t0 + ChronoDuration::seconds(70),
        t0 + ChronoDuration::seconds(80),
        t0 + ChronoDuration::seconds(90),
        t0 + ChronoDuration::seconds(100),
    ];
    for (f, exp) in r.fires.iter().zip(expected.iter()) {
        assert_eq!(f.scheduled_for, *exp);
    }
}

#[test]
fn control_refill_wakes_running_loop() {
    use std::thread;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("timers.db");
    let store = Store::open(&path).unwrap();

    let mut sched = Scheduler::new(
        store,
        SystemClock::new(),
        RecordingAction::new(),
        // Empty heap would otherwise sleep a full 30 s between polls.
        SchedulerConfig::default().with_max_sleep(Duration::from_secs(30)),
    );
    sched.boot().unwrap();
    assert_eq!(sched.heap_len(), 0);

    let handle = sched.control_handle();
    let path_bg = path.clone();

    let bg = thread::spawn(move || {
        // Let the main loop enter its long empty-heap sleep.
        thread::sleep(Duration::from_millis(200));
        let mut store = Store::open(&path_bg).unwrap();
        let now = Utc::now();
        let occ = Occurrence::new(
            OccurrenceKind::Interval {
                every_secs: 2,
                anchor: now,
            },
            "UTC",
        )
        .unwrap();
        let mut new = NewTimer::new("wake-me", occ);
        new.last_fired = Some(now);
        new.misfire = MisfirePolicy::Skip;
        store.create_timer(new).unwrap();
        handle.refill();
        // Allow ~one interval fire, then stop the loop.
        thread::sleep(Duration::from_millis(2800));
        handle.shutdown();
    });

    let start = std::time::Instant::now();
    let result = sched.run_until_shutdown().unwrap();
    let elapsed = start.elapsed();
    bg.join().unwrap();

    assert!(
        result.refilled,
        "refill must be observed by the running loop"
    );
    assert!(
        !result.fires.is_empty(),
        "expected at least one fire after refill, got {}",
        result.fires.len()
    );
    // Must not wait the full 30 s empty-heap backstop before noticing the edit.
    assert!(
        elapsed < Duration::from_secs(10),
        "loop slept too long before refill/fire: {elapsed:?}"
    );
}

// --- global pause-all ---------------------------------------------------

#[test]
fn pause_all_keeps_heap_warm_but_blocks_fires() {
    // Build a scheduler that is already paused; an interval timer with a fire
    // time in the past must NOT fire while paused.
    let (_dir, mut store) = open_tmp();
    let t0 = epoch();
    let clock = SimulatedClock::new(t0);
    // 2 s interval, last_fired = t0 so next_fire = t0+2 (the simulated clock
    // returns t0, so the timer is due immediately at the first tick).
    let timer = interval_timer(&mut store, "paused", 2, t0, Some(t0), MisfirePolicy::Skip);
    let mut sched = Scheduler::new_paused(
        store,
        clock,
        RecordingAction::default(),
        SchedulerConfig::default(),
    );
    sched.boot().unwrap();
    assert!(sched.pause_all(), "scheduler must report pause-all set");
    // Advance simulated time past the next fire.
    sched.clock().sleep(Duration::from_secs(5));
    let r = sched.run_for(Duration::from_millis(50)).unwrap();
    assert_eq!(r.fires.len(), 0, "no fire should happen while paused");
    assert_eq!(
        sched.action().events.len(),
        0,
        "RecordingAction stays empty while paused"
    );
    // The timer must still be in the store, with the same next_fire (we did
    // not advance it).
    let still_there = sched
        .store()
        .get_timer(timer.id)
        .unwrap()
        .expect("timer still present");
    assert_eq!(
        still_there.next_fire_utc, timer.next_fire_utc,
        "next_fire must not advance while paused"
    );
}

#[test]
fn unpause_via_control_msg_lets_due_timer_fire() {
    // Start paused; the timer's next_fire_utc is already at t0 (so it would
    // be due the instant unpause is set). Unpause via the control handle and
    // confirm the very next tick delivers a fire.
    let (_dir, mut store) = open_tmp();
    let t0 = epoch();
    let clock = SimulatedClock::new(t0);
    // 60s interval, last_fired at t0, so next_fire = t0+60. Anchor the clock at
    // t0+61 so next_fire is well in the past → guaranteed-due at the unpause.
    let _timer = interval_timer(
        &mut store,
        "flip",
        60,
        t0,
        Some(t0),
        MisfirePolicy::Coalesce { grace_secs: 3600 },
    );
    let mut sched = Scheduler::new_paused(
        store,
        clock.clone(),
        RecordingAction::default(),
        SchedulerConfig::default(),
    );
    sched.boot().unwrap();
    assert!(sched.pause_all());
    // While paused, advance the clock past the fire time and tick — no fire.
    clock.advance(Duration::from_secs(61));
    let r = sched.run_for(Duration::from_millis(20)).unwrap();
    assert_eq!(r.fires.len(), 0, "paused → no fires");
    assert_eq!(sched.action().events.len(), 0);

    // Unpause via the public control handle; next tick should fire.
    sched.control_handle().set_pause_all(false);
    let r = sched.run_for(Duration::from_millis(50)).unwrap();
    assert!(
        !r.fires.is_empty(),
        "after unpause, due timer must fire (got {:?})",
        r.fires
    );
    assert_eq!(sched.action().events.len(), 1);
    assert!(!sched.pause_all());
}

#[test]
fn set_pause_all_now_observable_immediately() {
    // In-place flag update via the scheduler struct; no control message needed.
    let (_dir, mut store) = open_tmp();
    let t0 = epoch();
    let clock = SimulatedClock::new(t0);
    let _timer = interval_timer(&mut store, "now", 2, t0, Some(t0), MisfirePolicy::Skip);
    let mut sched = Scheduler::new(
        store,
        clock,
        RecordingAction::default(),
        SchedulerConfig::default(),
    );
    sched.boot().unwrap();
    assert!(!sched.pause_all());
    sched.set_pause_all_now(true);
    assert!(sched.pause_all());
    // Advance time past due and tick; no fire.
    sched.clock().sleep(Duration::from_secs(5));
    let r = sched.run_for(Duration::from_millis(20)).unwrap();
    assert_eq!(r.fires.len(), 0);
}

#[test]
fn pickup_deadline_fires_from_the_scheduler_heap() {
    // Real clock, tiny deadlines: the pickup countdown must expire via the
    // scheduler's heap entry — no reply watcher is running in this test.
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();
    std::fs::create_dir_all(data_dir.join("slots")).unwrap();
    let db = data_dir.join("timers.db");
    let mut store = Store::open_with(
        &db,
        crate::store::OpenOptions {
            refuse_network_fs: false,
            ..Default::default()
        },
    )
    .unwrap();

    // Owned timer, first fire essentially now, next fire far away.
    let t0 = Utc::now();
    let timer = interval_timer(
        &mut store,
        "owned-hf",
        3600,
        t0 - ChronoDuration::seconds(3600),
        Some(t0 - ChronoDuration::seconds(3600)),
        MisfirePolicy::Skip,
    );
    store.set_timer_owner(timer.id, "app-x").unwrap();
    crate::tree::TimersTree::new(&data_dir)
        .create_for_timer(&timer, Some("app-x"))
        .unwrap();

    let mut cfg = SchedulerConfig::default()
        .with_data_dir(&data_dir)
        .with_max_sleep(Duration::from_millis(40));
    cfg.pickup_grace = Duration::from_millis(150);
    let sched_store = Store::open_with(
        &db,
        crate::store::OpenOptions {
            refuse_network_fs: false,
            ..Default::default()
        },
    )
    .unwrap();
    let mut sched = Scheduler::new(sched_store, SystemClock::new(), RecordingAction::new(), cfg);
    sched.boot().unwrap();
    // ~1s: fire at t≈0, pickup heap entry at +150ms — the scheduler itself
    // must mark no_ack without any watcher poll.
    sched.run_for(Duration::from_millis(900)).unwrap();

    let store2 = Store::open_with(
        &db,
        crate::store::OpenOptions {
            refuse_network_fs: false,
            ..Default::default()
        },
    )
    .unwrap();
    let rows: Vec<_> = store2
        .runs_for_timer(timer.id)
        .unwrap()
        .into_iter()
        .filter_map(|c| store2.get_run_state(c.run_id).unwrap())
        .collect();
    assert_eq!(rows.len(), 1, "one owned run fired");
    assert_eq!(
        rows[0].state, "no_ack",
        "the heap-driven pickup deadline fired inside run_for"
    );

    // status.json mirrors it (projection happened in the deadline path).
    let folder = crate::tree::TimersTree::new(&data_dir)
        .folder_for(timer.id)
        .unwrap();
    let status: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(folder.join(crate::tree::STATUS_FILE_NAME)).unwrap(),
    )
    .unwrap();
    assert_eq!(status["state"], "no_ack");
}

// --- external store writers (SCH2) ---------------------------------------
//
// `bellman slot-submit` claims and applies slot requests ITSELF, on its own
// connection, in its own process. The running scheduler gets no control
// message and no slot request ever passes through its watcher — it must
// notice the foreign commit on its own (PRAGMA data_version + a periodic
// rebuild floor). These tests never send Refill: that is the whole point.

/// SCH2 regression (Path B): a timer committed by another connection — the
/// exact `bellman slot-submit` shape — must fire on a running scheduler with
/// no refill, no GUI edit, no restart. Fails against the pre-fix engine,
/// where the horizon heap was only rebuilt on an explicit Refill.
#[test]
fn external_writer_add_fires_without_refill() {
    use std::thread;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("timers.db");
    let store = Store::open(&path).unwrap();

    let mut sched = Scheduler::new(
        store,
        SystemClock::new(),
        RecordingAction::new(),
        SchedulerConfig::default()
            .with_max_sleep(Duration::from_millis(50))
            // Disable the periodic floor: this test proves the data_version
            // probe alone notices the foreign commit.
            .with_external_rebuild_interval(Duration::from_secs(3600)),
    );
    sched.boot().unwrap();
    assert_eq!(sched.heap_len(), 0);

    let handle = sched.control_handle();
    let path_bg = path.clone();
    let bg = thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        // The "slot-submit process": its own connection, its own commit, and
        // NO control message back to the running scheduler.
        let mut ext = Store::open(&path_bg).unwrap();
        let now = Utc::now();
        let occ = Occurrence::new(
            OccurrenceKind::Interval {
                every_secs: 1,
                anchor: now,
            },
            "UTC",
        )
        .unwrap();
        let mut new = NewTimer::new("external-writer", occ);
        new.last_fired = Some(now);
        new.misfire = MisfirePolicy::Skip;
        ext.create_timer(new).unwrap();
        // Wait past two full intervals, then stop the loop.
        thread::sleep(Duration::from_millis(2500));
        handle.shutdown();
    });

    let result = sched.run_until_shutdown().unwrap();
    bg.join().unwrap();

    assert!(
        result.fires.len() >= 2,
        "externally-created timer must fire on its own interval (>=2 fires past two intervals), got {}",
        result.fires.len()
    );
}

/// SCH2 (Path B, modify): a slot MODIFY applied by another process moves the
/// fire time; the running scheduler must pick up the new time with no restart.
#[test]
fn external_slot_modify_moves_fire_without_restart() {
    use std::thread;

    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("timers.db");
    let slots_root = dir.path().join("slots");
    let store = Store::open(&db).unwrap();

    let handle_holder;
    let mut sched = Scheduler::new(
        store,
        SystemClock::new(),
        RecordingAction::new(),
        SchedulerConfig::default()
            .with_max_sleep(Duration::from_millis(50))
            .with_external_rebuild_interval(Duration::from_secs(3600)),
    );
    sched.boot().unwrap();
    handle_holder = sched.control_handle();
    let handle = handle_holder;

    let db_bg = db.clone();
    let slots_bg = slots_root.clone();
    let bg = thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        // External process: create a timer whose first fire is an hour out.
        let mut ext = Store::open(&db_bg).unwrap();
        let ext_service =
            crate::slots::SlotService::open(&slots_bg, crate::slots::SlotConfig::default())
                .unwrap();
        let now = Utc::now();
        let occ = Occurrence::new(
            OccurrenceKind::Interval {
                every_secs: 3600,
                anchor: now,
            },
            "UTC",
        )
        .unwrap();
        let mut new = NewTimer::new("move-me", occ);
        new.last_fired = Some(now);
        new.misfire = MisfirePolicy::Skip;
        let timer = ext.create_timer(new).unwrap();
        ext.set_timer_owner(timer.id, "app-b").unwrap();

        // Let the scheduler notice the add and load the far-future entry.
        thread::sleep(Duration::from_millis(500));

        // Slot MODIFY (applied by the external process, slot-submit shape):
        // move the schedule to a 1 s interval.
        let mod_req = crate::slots::SlotRequest {
            schema: crate::slots::SCHEMA_V1.to_string(),
            slot_id: String::new(),
            request_id: Some(uuid::Uuid::new_v4().to_string()),
            logged_at: Some(Utc::now()),
            operation: Some(crate::slots::SlotOperation::Modify),
            payload: Some(serde_json::json!({
                "app_name": "app-b",
                "timer_id": timer.id,
                "every_secs": 1,
                "occurrence": { "kind": "interval", "every_secs": 1 },
                "tz": "UTC"
            })),
        };
        ext_service.publish(mod_req).unwrap();
        let n = ext_service.poll(&mut ext).unwrap();
        assert_eq!(n, 1, "modify request must be processed");

        thread::sleep(Duration::from_millis(2500));
        handle.shutdown();
    });

    let start = std::time::Instant::now();
    let result = sched.run_until_shutdown().unwrap();
    let elapsed = start.elapsed();
    bg.join().unwrap();

    assert!(
        !result.fires.is_empty(),
        "slot-modified timer must fire at its NEW time without a restart"
    );
    // The only fire inside this window is the moved one (old schedule was
    // an hour out). If the modify had not taken effect, fires would be empty.
    assert!(
        elapsed < Duration::from_secs(10),
        "moved fire took too long: {elapsed:?}"
    );
}

/// SCH2 (Path B, delete): a timer deleted on another connection must not
/// ghost-fire from a stale heap entry. The timer fires BEFORE the delete —
/// proving it really reached the heap — so "no fires after" is not vacuously
/// true on a broken engine that never loads external timers at all.
#[test]
fn external_delete_leaves_no_ghost_fire() {
    use std::thread;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("timers.db");
    let store = Store::open(&path).unwrap();

    let mut sched = Scheduler::new(
        store,
        SystemClock::new(),
        RecordingAction::new(),
        SchedulerConfig::default()
            .with_max_sleep(Duration::from_millis(50))
            .with_external_rebuild_interval(Duration::from_secs(3600)),
    );
    sched.boot().unwrap();

    let handle = sched.control_handle();
    let path_bg = path.clone();
    let bg = thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        let mut ext = Store::open(&path_bg).unwrap();
        let now = Utc::now();
        let occ = Occurrence::new(
            OccurrenceKind::Interval {
                every_secs: 1,
                anchor: now,
            },
            "UTC",
        )
        .unwrap();
        let mut new = NewTimer::new("doomed", occ);
        new.last_fired = Some(now);
        new.misfire = MisfirePolicy::Skip;
        let timer = ext.create_timer(new).unwrap();
        // Let it fire at least once — proves the entry reached the heap.
        thread::sleep(Duration::from_millis(2500));
        // …then the timer is deleted on the foreign connection.
        ext.delete_timer(timer.id).unwrap();
        let deleted_at = Utc::now();
        // Wait well past several more would-be fire times.
        thread::sleep(Duration::from_millis(3000));
        handle.shutdown();
        deleted_at
    });

    let result = sched.run_until_shutdown().unwrap();
    let deleted_at = bg.join().unwrap();

    let before = result
        .fires
        .iter()
        .filter(|f| f.scheduled_for <= deleted_at)
        .count();
    let after = result
        .fires
        .iter()
        .filter(|f| f.scheduled_for > deleted_at)
        .count();
    assert!(
        before >= 1,
        "the timer must fire BEFORE the delete (proves heap occupancy) — \
         without this, 'no ghost fire' is vacuous on an engine that never loads it"
    );
    assert!(
        after == 0,
        "deleted timer ghost-fired {after} time(s) from a stale heap entry"
    );
}

/// SCH2 exit gate: idle loop iterations must NOT rebuild the horizon (the
/// fix must not become a busy loop). Counted via the store's horizon query
/// counter across an idle period with the floor disabled.
#[test]
fn idle_ticks_do_not_rebuild_horizon() {
    let (_dir, store) = open_tmp();
    let mut sched = Scheduler::new(
        store,
        SystemClock::new(),
        RecordingAction::new(),
        SchedulerConfig::default()
            .with_max_sleep(Duration::from_millis(20))
            .with_external_rebuild_interval(Duration::from_secs(3600)),
    );
    sched.boot().unwrap();
    let q0 = sched.store().horizon_query_count();

    let r = sched.run_for(Duration::from_millis(400)).unwrap();

    assert!(!r.refilled, "idle loop must not refill");
    assert_eq!(
        sched.store().horizon_query_count(),
        q0,
        "idle ticks issued horizon queries (busy loop)"
    );
}

/// SCH2 floor: even with nothing changing, the horizon is rebuilt at least
/// once per `external_rebuild_interval` — the bounded safety net under the
/// data_version probe.
#[test]
fn periodic_floor_rebuilds_even_when_idle() {
    let (_dir, store) = open_tmp();
    let mut sched = Scheduler::new(
        store,
        SystemClock::new(),
        RecordingAction::new(),
        SchedulerConfig::default()
            .with_max_sleep(Duration::from_millis(20))
            .with_external_rebuild_interval(Duration::from_millis(100)),
    );
    sched.boot().unwrap();
    let q0 = sched.store().horizon_query_count();

    let r = sched.run_for(Duration::from_millis(550)).unwrap();

    assert!(r.refilled, "the floor must refill even when idle");
    // Each rebuild is 2 horizon queries (timers_due_by + list_timers); 550 ms
    // over a 100 ms floor is >= 4 rebuilds after boot.
    assert!(
        sched.store().horizon_query_count() >= q0 + 4,
        "floor rebuilds missing: q0={q0} now={}",
        sched.store().horizon_query_count()
    );
}
