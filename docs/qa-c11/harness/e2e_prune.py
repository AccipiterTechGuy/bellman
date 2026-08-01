#!/usr/bin/env python3
"""C11 §2 — pruner / retention against the real running app.

Learned the hard way on the first attempt: `AppConfig::sanitized()` clamps
`log_rotation_max_bytes` up to a 1 MiB floor and `log_retention_budget_bytes`
up to 4 MiB, so a config asking for a 6 KB rotation is silently ignored (that
floor is undocumented — see VALIDATION.md). This run uses the smallest values
the product will actually accept and pre-seeds enough real log volume that
both the rotation trigger and the byte budget fire inside one publish cycle.

Seeded, all valid documents on disk before the app starts:
  * `events.current.jsonl` just over the 1 MiB rotation threshold
  * six gzipped archives of ~1 MiB each, mtimes staggered oldest → newest,
    two of them older than the 30-day retention window
Then one 1 s interval timer supplies the live traffic that makes the
publisher drain, rotate and retain.
"""
import gzip
import json
import os
import sys
from pathlib import Path
import time
import uuid
from datetime import datetime, timedelta, timezone

# The harness lives next to the evidence it produced; importing the shared
# helper from beside this file keeps every script runnable from anywhere.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from e2e_lib import Run, say  # noqa: E402

TZ = "Europe/Helsinki"
ROTATE_AT = 1024 * 1024            # the sanitizer's floor
BUDGET = 4 * 1024 * 1024           # the sanitizer's floor
ARCHIVES = 6


def event_line(i):
    """A real bellman-event/1 line; random ids keep it from over-compressing."""
    return json.dumps({
        "schema": "bellman-event/1",
        "logged_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ"),
        "kind": "fired",
        "event_id": str(uuid.uuid4()),
        "timer_id": str(uuid.uuid4()),
        "run_id": str(uuid.uuid4()),
        "timer_name": f"seeded-{i}",
        "scheduled_for": "2026-06-01T00:00:00Z",
    })


def fill(path, target_bytes, gz=False):
    opener = (lambda p: gzip.open(p, "wt")) if gz else (lambda p: open(p, "w"))
    i = 0
    with opener(path) as fh:
        while os.path.getsize(path) if os.path.exists(path) else 0:
            break
        while True:
            fh.write(event_line(i) + "\n")
            i += 1
            if i % 500 == 0:
                fh.flush()
                if os.path.getsize(path) >= target_bytes:
                    break
    return os.path.getsize(path)


def main():
    run = Run("prune2", display=":96").fresh()
    run.seed_config(log_rotation_max_bytes=ROTATE_AT,
                    log_retention_budget_bytes=BUDGET,
                    retention_days=30)
    logs = run.appdata / "logs"
    arch = logs / "archive"
    arch.mkdir(parents=True, exist_ok=True)

    now = time.time()
    seeded = []
    for n in range(ARCHIVES):
        # Weeks far enough back that the names never collide with today's.
        p = arch / f"events-2026-W{10 + n:02d}.jsonl.gz"
        size = fill(p, 1024 * 1024, gz=True)
        # Oldest two are outside the 30-day retention window.
        age_days = 45 - n * 6
        os.utime(p, (now - age_days * 86400, now - age_days * 86400))
        seeded.append({"name": p.name, "bytes": size, "age_days": age_days})
        say(f"seeded archive {p.name} {size} bytes, {age_days} days old")

    cur = logs / "events.current.jsonl"
    cur_size = fill(cur, ROTATE_AT + 4096)
    say(f"seeded {cur.name} {cur_size} bytes (rotation threshold {ROTATE_AT})")

    before_total = cur_size + sum(a["bytes"] for a in seeded)
    say(f"total retained before: {before_total} bytes, budget {BUDGET}")

    run.start_app()
    run.submit({"app_name": "noise-app", "timer_name": "prune-noise",
                "tz": TZ, "occurrence": {"kind": "interval", "every_secs": 1}})
    say("submitted a 1 s interval timer to make the publisher drain")

    rotated = None
    end = time.monotonic() + 240
    while time.monotonic() < end:
        arcs = sorted(arch.glob("*"))
        names = {a.name for a in arcs}
        new = names - {a["name"] for a in seeded}
        total = (cur.stat().st_size if cur.exists() else 0) + \
            sum(a.stat().st_size for a in arcs)
        if new and total <= BUDGET:
            rotated = sorted(new)
            break
        time.sleep(3)

    arcs = sorted(arch.glob("*"))
    checks = []
    for a in arcs:
        head = a.open("rb").read(2)
        ok = head == b"\x1f\x8b"
        lines = None
        if ok:
            try:
                lines = sum(1 for _ in gzip.open(a, "rt"))
            except OSError as e:
                lines = f"decompress failed: {e}"
        checks.append({"name": a.name, "bytes": a.stat().st_size,
                       "gzip_magic": ok, "lines": lines,
                       "mtime_age_days": round((time.time() - a.stat().st_mtime) / 86400, 1)})

    cur_now = cur.stat().st_size if cur.exists() else 0
    total = cur_now + sum(a.stat().st_size for a in arcs)
    surviving = {c["name"] for c in checks}
    dropped = [s for s in seeded if s["name"] not in surviving]
    aged_out = [s for s in dropped if s["age_days"] > 30]

    out = {
        "config": json.loads((run.appdata / "config.json").read_text()),
        "rotation_threshold_bytes": ROTATE_AT,
        "retained_budget_bytes": BUDGET,
        "seeded_archives": seeded,
        "seeded_current_bytes": cur_size,
        "total_before": before_total,
        "new_archives_from_rotation": rotated,
        "archives_after": checks,
        "current_bytes_after": cur_now,
        "total_after": total,
        "budget_holds": total <= BUDGET,
        "current_below_threshold": cur_now <= ROTATE_AT,
        "dropped_by_retention": dropped,
        "dropped_because_older_than_30_days": aged_out,
        "rotation_events": [e for e in run.log_lines() if e.get("kind") == "pruned"][-6:],
    }
    p = run.root / "prune_evidence.json"
    p.write_text(json.dumps(out, indent=2, default=str))
    say("rotation produced:", rotated)
    say(f"current={cur_now} archives={len(arcs)} total={total} budget={BUDGET} "
        f"holds={total <= BUDGET}")
    say("dropped by retention:", [d["name"] for d in dropped])
    say("evidence ->", p)
    run.stop()
    ok = bool(rotated) and total <= BUDGET and all(c["gzip_magic"] for c in checks)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
