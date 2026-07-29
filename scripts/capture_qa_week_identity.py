#!/usr/bin/env python3
"""QA proof — Week identity header + today highlight + nav updates.

Isolated display + tauri-driver / WebKitWebDriver only. Never the operator X session.
Outputs under docs/qa4-screenshots/ (week-id-*) and docs/qa4-evidence/week-identity.json.
"""
from __future__ import annotations

import json
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import qa_webdriver as qa

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "docs" / "qa4-screenshots"
EVIDENCE = ROOT / "docs" / "qa4-evidence"

WEEK_RE = re.compile(
    r"^Week\s+(\d{1,2})\s+·\s+.+\s+\d{4}$"
)


def week_identity_text(d) -> str:
    By = qa._by()
    el = d.find_element(By.CSS_SELECTOR, "[data-testid='week-identity'], .week-identity")
    # WebKitGTK sometimes returns empty .text for flex children; prefer textContent.
    raw = (
        el.get_attribute("textContent")
        or el.get_attribute("innerText")
        or el.text
        or ""
    )
    return raw.strip()


def today_cols(d) -> list[dict]:
    By = qa._by()
    out = []
    for col in d.find_elements(By.CSS_SELECTOR, ".week-col"):
        aria = col.get_attribute("aria-current") or ""
        classes = col.get_attribute("class") or ""
        day = col.get_attribute("data-day") or ""
        if "today" in classes.split() or aria == "date":
            out.append({"day": day, "aria_current": aria, "class": classes})
    return out


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    EVIDENCE.mkdir(parents=True, exist_ok=True)

    report: dict = {"steps": [], "ok": True}
    d = qa.start_session()
    try:
        qa.resize_window(1280, 800)
        time.sleep(1.0)
        # Skip wizard if present (config may already mark completed).
        try:
            qa.click_button("Skip")
            time.sleep(0.4)
        except Exception:
            pass
        try:
            qa.click_button("Get started")
            time.sleep(0.4)
        except Exception:
            pass

        qa.click_tab("Week")
        time.sleep(0.8)

        # --- Current week ---
        heading = week_identity_text(d)
        todays = today_cols(d)
        step = {
            "name": "current_week",
            "heading": heading,
            "heading_matches": bool(WEEK_RE.match(heading)),
            "today_cols": todays,
            "today_count": len(todays),
        }
        report["steps"].append(step)
        print(f"current: {heading!r} today_cols={len(todays)}")
        if not step["heading_matches"]:
            report["ok"] = False
            print("FAIL: heading does not match Week N · range YEAR")
        if len(todays) != 1:
            report["ok"] = False
            print(f"FAIL: expected exactly 1 today column on current week, got {len(todays)}")
        elif todays[0].get("aria_current") != "date":
            report["ok"] = False
            print(f"FAIL: today column missing aria-current=date: {todays[0]}")
        qa.capture(
            d,
            "week-id-current",
            {
                "expect": "Week N · date range visible; today column highlighted",
                "heading": heading,
            },
        )

        # --- Next week ---
        qa.click_button("Next ▶")
        time.sleep(0.5)
        next_heading = week_identity_text(d)
        next_todays = today_cols(d)
        step = {
            "name": "next_week",
            "heading": next_heading,
            "heading_matches": bool(WEEK_RE.match(next_heading)),
            "changed_from_current": next_heading != heading,
            "today_cols": next_todays,
            "today_count": len(next_todays),
        }
        report["steps"].append(step)
        print(f"next:    {next_heading!r} today_cols={len(next_todays)}")
        if not step["heading_matches"] or not step["changed_from_current"]:
            report["ok"] = False
            print("FAIL: Next did not update week identity")
        if len(next_todays) != 0:
            report["ok"] = False
            print("FAIL: browsing another week must not mark a column as today")
        qa.capture(
            d,
            "week-id-next",
            {
                "expect": "week number/range advanced; no today highlight",
                "heading": next_heading,
            },
        )

        # --- Prev back to current ---
        qa.click_button("◀ Prev")
        time.sleep(0.5)
        prev_heading = week_identity_text(d)
        step = {
            "name": "prev_week",
            "heading": prev_heading,
            "restored_current": prev_heading == heading,
        }
        report["steps"].append(step)
        print(f"prev:    {prev_heading!r}")
        if not step["restored_current"]:
            report["ok"] = False
            print("FAIL: Prev did not restore previous week identity")
        qa.capture(d, "week-id-prev", {"expect": "back to prior week", "heading": prev_heading})

        # --- This week ---
        qa.click_button("Next ▶")
        time.sleep(0.3)
        qa.click_button("This week")
        time.sleep(0.5)
        this_heading = week_identity_text(d)
        this_todays = today_cols(d)
        step = {
            "name": "this_week",
            "heading": this_heading,
            "matches_current": this_heading == heading,
            "today_cols": this_todays,
            "today_count": len(this_todays),
        }
        report["steps"].append(step)
        print(f"this:    {this_heading!r} today_cols={len(this_todays)}")
        if not step["matches_current"]:
            report["ok"] = False
            print("FAIL: This week did not restore current week identity")
        if len(this_todays) != 1:
            report["ok"] = False
            print("FAIL: This week should show exactly one today column")
        qa.capture(
            d,
            "week-id-this",
            {
                "expect": "This week restores identity + today highlight",
                "heading": this_heading,
            },
        )

        # Desktop width: identity not empty / not clipped to zero
        By = qa._by()
        el = d.find_element(By.CSS_SELECTOR, ".week-identity")
        box = el.rect
        step = {
            "name": "desktop_readable",
            "rect": box,
            "text_len": len(this_heading),
        }
        report["steps"].append(step)
        if box.get("width", 0) < 80 or not this_heading:
            report["ok"] = False
            print(f"FAIL: week identity not readable at desktop width: {box}")
        else:
            print(f"desktop rect ok: {box}")

    finally:
        try:
            qa.stop_session()
        except Exception:
            pass

    path = EVIDENCE / "week-identity.json"
    path.write_text(json.dumps(report, indent=2) + "\n")
    print(f"wrote {path} ok={report['ok']}")
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
