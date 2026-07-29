#!/usr/bin/env python3
"""QA P4e — WebKitGTK evidence for fire-neighbour collisions / list triage / calendar create.

Isolated display + tauri-driver / WebKitWebDriver. No global-input-injection, never the operator X session.
Outputs under docs/qa4-screenshots/ (p4e-*) and docs/qa4-evidence/.
"""
from __future__ import annotations

import json
import os
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import qa_webdriver as qa

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "docs" / "qa4-screenshots"
EVIDENCE = ROOT / "docs" / "qa4-evidence"
DATA_DIR = Path(
    os.environ.get(
        "BELLMAN_QA_DATA",
        "/tmp/bellman-qa-session/share/io.bellman.desktop",
    )
)
CLI = qa.CLI_BIN


def dump_store(path: Path):
    db = DATA_DIR / "timers.db"
    if not db.exists():
        path.write_text("[]\n")
        return []
    con = sqlite3.connect(str(db))
    con.row_factory = sqlite3.Row
    rows = [dict(r) for r in con.execute("SELECT * FROM timers ORDER BY name")]
    con.close()
    out = []
    for r in rows:
        item = {}
        for k, v in r.items():
            item[k] = v if isinstance(v, (int, float, type(None))) else str(v)
        out.append(item)
    path.write_text(json.dumps(out, indent=2) + "\n")
    return out


def cli(*args: str) -> str:
    db = DATA_DIR / "timers.db"
    cmd = [CLI, "--db", str(db), *args]
    r = subprocess.run(cmd, capture_output=True, text=True)
    text = (r.stdout or "") + (r.stderr or "")
    if r.returncode != 0:
        print(f"  cli FAIL ({r.returncode}): {' '.join(args)}\n{text[:500]}")
    return text


def set_action_launch(command: str = "/bin/true"):
    d = qa.driver()
    By = qa._by()
    # Click the "launch command" radio via label text
    for lab in d.find_elements(By.CSS_SELECTOR, "label.radio, label"):
        if "launch" in (lab.text or "").lower():
            lab.click()
            time.sleep(0.2)
            break
    try:
        qa.set_input_value("#td-cmd", command)
    except Exception:
        # fallback by placeholder/name
        for css in ("#td-cmd", "input[placeholder*='notify']", "input"):
            try:
                els = d.find_elements(By.CSS_SELECTOR, css)
                for el in els:
                    if el.is_displayed():
                        qa.set_input_value(css if css.startswith("#") else f"#{el.get_attribute('id')}", command)
                        return
            except Exception:
                continue


def create_daily(
    d,
    *,
    name: str,
    time_hhmm: str,
    tz: str = "UTC",
    launch: bool = False,
    snap: str | None = None,
):
    print(f"\n== CREATE daily {name!r} @ {time_hhmm} ==")
    qa.open_new_timer()
    qa.select_kind("daily")
    time.sleep(0.25)
    qa.fill_fields(
        [
            ("Name", name),
            ("Timezone", tz),
            ("Wall-clock", time_hhmm),
        ]
    )
    if launch:
        set_action_launch("/bin/true")
    time.sleep(1.1)
    if snap:
        qa.capture(d, snap, {"name": name, "time": time_hhmm, "launch": launch})
    qa.click_button("Create")
    time.sleep(0.95)


def seed_bulk_cli(n: int = 50):
    print(f"\n== CLI seed {n} timers for density/timing ==")
    for i in range(n):
        minute = (i * 7) % 60
        hour = 11 + (i // 60) % 10
        name = f"bulk-timer-{i:03d}-padding-name-longish"
        cli(
            "add",
            "--name",
            name,
            "--occurrence",
            "daily",
            "--time",
            f"{hour:02d}:{minute:02d}:00",
            "--tz",
            "UTC",
        )
    rows = dump_store(EVIDENCE / "store-after-bulk-seed.json")
    print(f"  store rows after bulk: {len(rows)}")


def measure_neighbours_cli() -> dict:
    out = {"next": {}}
    listing = cli("list", "--json")
    (EVIDENCE / "bellman-list-collision.json").write_text(listing)
    try:
        data = json.loads(listing)
        timers = data if isinstance(data, list) else data.get("timers", data.get("items", []))
    except Exception:
        timers = []
    for t in timers:
        name = t.get("name") or ""
        if name.startswith("qa-collide") or name.startswith("qa-nearby") or name.startswith(
            "qa-long"
        ):
            tid = t.get("id") or t.get("timer_id")
            if not tid:
                continue
            nxt = cli("next", "--json", str(tid), "3")
            out["next"][name] = nxt
            (EVIDENCE / f"bellman-next-{name}.json").write_text(nxt)
    (EVIDENCE / "collision-cli-parity.json").write_text(json.dumps(out, indent=2) + "\n")
    return out


def filter_by_name(query: str):
    d = qa.driver()
    By = qa._by()
    # Prefer dedicated filter input if present
    for css in (
        "input[placeholder*='Filter']",
        "input[aria-label*='Filter']",
        "input[type='search']",
        ".filter input",
        "input",
    ):
        els = d.find_elements(By.CSS_SELECTOR, css)
        for el in els:
            try:
                if not el.is_displayed():
                    continue
                # Skip dialog fields when dialog open
                ph = (el.get_attribute("placeholder") or "") + (el.get_attribute("aria-label") or "")
                if "Filter" in ph or "Search" in ph or "filter" in ph.lower():
                    eid = el.get_attribute("id")
                    if eid:
                        qa.set_input_value(f"#{eid}", query)
                    else:
                        d.execute_script(
                            """
                            const el = arguments[0], val = arguments[1];
                            el.focus(); el.value = val;
                            el.dispatchEvent(new Event('input', {bubbles:true}));
                            """,
                            el,
                            query,
                        )
                    time.sleep(0.4)
                    return True
            except Exception:
                continue
    return False


def shot_list_sort_filter(d):
    print("\n== LIST sort/filter ==")
    qa.close_dialog_if_open()
    qa.click_tab("All timers")
    time.sleep(0.45)
    filter_by_name("qa-collide")
    qa.capture(
        d,
        "p4e-list-filter-search",
        {"filter": "qa-collide", "expect": "only collision timers visible"},
    )
    filter_by_name("")
    time.sleep(0.3)
    qa.capture(d, "p4e-list-sort-next-fire", {"sort": "next fire default", "density": True})


def shot_calendar_create(d):
    print("\n== MONTH click-to-create ==")
    qa.close_dialog_if_open()
    qa.click_tab("Month")
    time.sleep(0.55)
    qa.capture(d, "p4e-month-fire-counts", {"expect": "day cells show fire counts"})
    try:
        qa.click_button("next month", exact=False, timeout=2.0)
        time.sleep(0.4)
    except Exception:
        pass
    drv = qa.driver()
    By = qa._by()
    creates = [
        b
        for b in drv.find_elements(By.CSS_SELECTOR, "button")
        if (b.get_attribute("aria-label") or b.text or "").startswith("Create timer on")
    ]
    if creates:
        target = creates[min(14, len(creates) - 1)]
        label = target.get_attribute("aria-label") or target.text
        print(f"  clicking {label!r}")
        target.click()
        time.sleep(0.75)
        qa.capture(d, "p4e-month-create-prefill", {"from": label})
        qa.fill_field("Name", "qa-from-month-cell")
        time.sleep(0.6)
        qa.capture(d, "p4e-month-create-dialog", {"name": "qa-from-month-cell"})
        qa.click_button("Create")
        time.sleep(0.95)
        dump_store(EVIDENCE / "store-after-month-create.json")
        qa.click_tab("All timers")
        time.sleep(0.35)
        filter_by_name("qa-from-month-cell")
        qa.capture(d, "p4e-list-after-month-create", {"expect": "qa-from-month-cell row"})
    else:
        print("  no Create timer on … buttons; skip prefill shot")


def shot_week_create(d):
    print("\n== WEEK empty-day create ==")
    qa.close_dialog_if_open()
    qa.click_tab("Week")
    time.sleep(0.45)
    qa.capture(d, "p4e-week-day-counts", {"expect": "day counts + empty + New"})
    drv = qa.driver()
    By = qa._by()
    news = [
        b
        for b in drv.find_elements(By.CSS_SELECTOR, "button")
        if "+ New" in (b.text or "") or "New on this day" in (b.text or "")
    ]
    if news:
        news[0].click()
        time.sleep(0.65)
        qa.capture(d, "p4e-week-create-prefill", {"expect": "dialog prefilled from week day"})
        qa.close_dialog_if_open()


def main() -> int:
    global DATA_DIR, CLI
    qa.DATA_DIR = Path(os.environ.get("BELLMAN_QA_DATA", str(DATA_DIR)))
    qa.DISPLAY_NAME = os.environ.get("DISPLAY", "")
    qa.OUT = OUT
    qa.EVIDENCE = EVIDENCE
    DATA_DIR = qa.DATA_DIR
    if os.environ.get("BELLMAN_CLI"):
        qa.CLI_BIN = os.environ["BELLMAN_CLI"]
    CLI = qa.CLI_BIN

    OUT.mkdir(parents=True, exist_ok=True)
    EVIDENCE.mkdir(parents=True, exist_ok=True)

    disp = os.environ.get("DISPLAY", "")
    if disp in (":0", ":0.0") and os.environ.get("BELLMAN_QA_ALLOW_DISPLAY0") != "1":
        print(f"ERROR: refusing DISPLAY={disp}", file=sys.stderr)
        return 2

    print(f"P4e WebDriver session DISPLAY={disp} DATA={DATA_DIR}")
    qa.start_session()
    d = qa.xdisp()
    qa.resize_window(960, 640)
    time.sleep(0.35)
    t0 = time.time()

    create_daily(d, name="qa-collide-alpha-backup", time_hhmm="09:00:00", tz="UTC")
    create_daily(
        d,
        name="qa-collide-beta-launch-heavy-workload",
        time_hhmm="09:00:00",
        tz="UTC",
        launch=True,
    )
    create_daily(d, name="qa-collide-gamma-notify", time_hhmm="09:00:00", tz="UTC")
    create_daily(d, name="qa-nearby-two-min", time_hhmm="09:02:00", tz="UTC")
    long_name = (
        "qa-long-name-morning-backup-and-sync-pipeline-with-extra-descriptive-words-end"
    )
    create_daily(d, name=long_name, time_hhmm="14:30:00", tz="UTC")
    dump_store(EVIDENCE / "store-after-collision-create.json")

    print("\n== DIALOG collision naming three peers ==")
    qa.open_new_timer()
    qa.select_kind("daily")
    qa.fill_fields(
        [
            ("Name", "qa-collide-delta-fourth"),
            ("Timezone", "UTC"),
            ("Wall-clock", "09:00:00"),
        ]
    )
    t_n0 = time.time()
    time.sleep(1.8)
    t_n1 = time.time()
    qa.capture(
        d,
        "p4e-dialog-collision-names-three",
        {
            "phase": "collision",
            "expect": "Also firing names the three qa-collide-* timers",
            "neighbour_wait_ms": int((t_n1 - t_n0) * 1000),
        },
    )
    qa.resize_window(1280, 800)
    time.sleep(0.45)
    qa.capture(d, "p4e-dialog-collision-1280x800", {"viewport": "1280x800"})
    qa.resize_window(960, 640)
    time.sleep(0.35)
    qa.close_dialog_if_open()

    print("\n== DIALOG nearby-only ==")
    qa.open_new_timer()
    qa.select_kind("daily")
    qa.fill_fields(
        [
            ("Name", "qa-probe-near-0901"),
            ("Timezone", "UTC"),
            ("Wall-clock", "09:01:00"),
        ]
    )
    time.sleep(1.5)
    qa.capture(
        d,
        "p4e-dialog-nearby-not-collision",
        {
            "phase": "nearby",
            "expect": "nearby list shows ±60s to collide timers; no same-second badge if none",
        },
    )
    qa.close_dialog_if_open()

    print("\n== DIALOG no-collision clear state ==")
    qa.open_new_timer()
    qa.select_kind("daily")
    qa.fill_fields(
        [
            ("Name", "qa-lonely-1530"),
            ("Timezone", "UTC"),
            ("Wall-clock", "15:30:00"),
        ]
    )
    time.sleep(1.4)
    qa.capture(
        d,
        "p4e-dialog-no-collision",
        {"phase": "clear", "expect": "No other timers fire at or near…"},
    )
    qa.close_dialog_if_open()

    qa.click_tab("All timers")
    time.sleep(0.35)
    filter_by_name("qa-long-name")
    qa.capture(
        d,
        "p4e-list-long-name-readable",
        {"expect": "full long timer name visible, no silent ellipsis"},
    )

    measure_neighbours_cli()
    shot_list_sort_filter(d)
    shot_week_create(d)
    shot_calendar_create(d)

    seed_bulk_cli(50)
    print("\n== BOUNDED WORK with ≥50 timers ==")
    qa.open_new_timer()
    qa.select_kind("daily")
    qa.fill_fields([("Name", "qa-timing-probe"), ("Timezone", "UTC")])
    t_b0 = time.time()
    qa.fill_field("Wall-clock", "09:00:00")
    time.sleep(2.0)
    t_b1 = time.time()
    qa.capture(
        d,
        "p4e-dialog-collision-50plus",
        {
            "phase": "bounded-work",
            "store_timers": len(dump_store(EVIDENCE / "store-final.json")),
            "dialog_response_ms_wait": int((t_b1 - t_b0) * 1000),
            "caps": {
                "window_secs": 300,
                "horizon_secs": 14 * 86400,
                "max_fires_per_timer": 48,
            },
        },
    )
    qa.close_dialog_if_open()

    elapsed = time.time() - t0
    summary = {
        "elapsed_s": round(elapsed, 2),
        "cli": CLI,
        "data_dir": str(DATA_DIR),
        "display": disp,
        "input_backend": "tauri-driver+WebKitWebDriver",
        "shots": sorted(p.name for p in OUT.glob("p4e-*.png")),
    }
    (EVIDENCE / "p4e-capture-summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print("\nDONE", json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    finally:
        try:
            qa.stop_session()
        except Exception:
            pass
