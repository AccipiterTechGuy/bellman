#!/usr/bin/env python3
"""The Bellman lightbulb GUI demo app.

Demonstrates the two-way integration protocol (docs/INTEGRATION.md) with a GUI window.
Features:
  - Create/delete timers via documented slot claims (bellman-slot/1).
  - Watch slots/fires/ for fire notifications matching app_name.
  - Interactive 4-step protocol handshake visualization:
      fired -> acknowledged -> running -> completed (or failed)
  - Golden filament bulb animation on dark neon background.
  - "Make it fail" button for error-path testing.

Stdlib Python 3 only (tkinter).
"""

import argparse
import json
import math
import os
import pathlib
import sys
import time
import uuid
from datetime import datetime, timezone, timedelta
import tkinter as tk
from tkinter import font as tkfont
from tkinter import messagebox

# Color Palette (Spec)
BG_COLOR = "#0a0a12"        # near-black indigo
PANEL_BG = "#12121f"        # grouped controls & log strip
NEON_CYAN = "#00e5ff"       # headings, borders, countdown
NEON_MAGENTA = "#ff2fb9"    # active/running accent, focus rings
NEON_VIOLET = "#8b5cf6"     # secondary chrome, timer next-fire line
BULB_OFF = "#3a3326"        # dim bronze
BULB_ON = "#ffc94a"         # warm gold
SUCCESS_GREEN = "#39ff88"   # neon green for completed chip
FAILURE_RED = "#ff3b5c"     # neon red for failed chip
TEXT_DIM = "#6b7280"        # dimmed text
TEXT_LIGHT = "#e5e7eb"      # light text


def now_utc():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def now_time_str():
    return datetime.now().strftime("%H:%M:%S")


def interpolate_color(c1_hex, c2_hex, factor):
    """Linearly interpolate between two RGB hex colors."""
    factor = max(0.0, min(1.0, factor))
    r1, g1, b1 = int(c1_hex[1:3], 16), int(c1_hex[3:5], 16), int(c1_hex[5:7], 16)
    r2, g2, b2 = int(c2_hex[1:3], 16), int(c2_hex[3:5], 16), int(c2_hex[5:7], 16)
    r = r1 + (r2 - r1) * factor
    g = g1 + (g2 - g1) * factor
    b = b1 + (b2 - b1) * factor
    return f"#{int(r):02x}{int(g):02x}{int(b):02x}"


def reply(fire, **fields):
    """The reply protocol: read pre-filled stub, update fields, atomic write."""
    path = fire["reply_path"]  # from notification — open verbatim, never construct
    with open(path, "r", encoding="utf-8") as f:
        r = json.load(f)
    r.update(fields)
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(r, f, indent=2)
    os.replace(tmp, path)


def claim_and_publish_slot(slots_root, operation, payload):
    """Publish a request via free stub claim and same-directory atomic rename."""
    free_dir = pathlib.Path(slots_root) / "free"
    done_dir = pathlib.Path(slots_root) / "done"

    if not free_dir.exists():
        free_dir.mkdir(parents=True, exist_ok=True)
    if not done_dir.exists():
        done_dir.mkdir(parents=True, exist_ok=True)

    free_files = sorted(free_dir.glob("slot-*.json"))
    claimed_file = None
    claim_path = None
    slot_id = None

    for path in free_files:
        if path.is_symlink():
            continue
        try:
            with open(path, "r", encoding="utf-8") as f:
                data = json.load(f)
            # Free stub has no operation or request_id
            if data.get("operation") is not None or data.get("request_id") is not None:
                continue
            slot_id = data.get("slot_id")
            if not slot_id:
                slot_id = path.stem.replace("slot-", "")

            # Exclusive claim rename: free/slot-NNNN.json -> free/.claim-<uuid>-slot-NNNN.json
            c_name = f".claim-{uuid.uuid4()}-{path.name}"
            c_path = free_dir / c_name
            os.rename(path, c_path)
            claimed_file = path
            claim_path = c_path
            break
        except (OSError, ValueError, KeyError):
            continue

    if not claimed_file or not claim_path or not slot_id:
        raise RuntimeError(f"No free slot stubs available in {free_dir}")

    request_id = str(uuid.uuid4())
    req = {
        "schema": "bellman-slot/1",
        "slot_id": slot_id,
        "request_id": request_id,
        "logged_at": now_utc(),
        "operation": operation,
        "payload": payload,
    }

    # Write request via temp + same-directory rename
    tmp_path = free_dir / f".tmp-{uuid.uuid4()}-{claimed_file.name}"
    with open(tmp_path, "w", encoding="utf-8") as f:
        json.dump(req, f, indent=2)
    os.replace(tmp_path, claimed_file)

    try:
        if claim_path.exists():
            os.remove(claim_path)
    except OSError:
        pass

    return slot_id, request_id


class LightbulbGUI(tk.Tk):
    def __init__(self, slots_dir, app_name, on_secs):
        super().__init__()
        self.slots_root = pathlib.Path(slots_dir).resolve()
        self.fires_dir = self.slots_root / "fires"
        self.app_name = app_name
        self.on_secs = on_secs

        self.title("Bellman Lightbulb Demo")
        self.geometry("740x840")
        self.configure(bg=BG_COLOR)
        self.minsize(680, 760)

        # Monospace font throughout
        self.fixed_font_family = tkfont.nametofont("TkFixedFont").cget("family")
        self.f_title = (self.fixed_font_family, 16, "bold")
        self.f_sub = (self.fixed_font_family, 10)
        self.f_head = (self.fixed_font_family, 11, "bold")
        self.f_body = (self.fixed_font_family, 10)
        self.f_small = (self.fixed_font_family, 9)
        self.f_large = (self.fixed_font_family, 18, "bold")

        # Active state
        self.active_timer_id = None
        self.active_timer_name = None
        self.next_fire_at = None
        self.next_fire_dt = None
        self.seen_runs = set()

        # Run state
        self.current_fire = None
        self.run_in_flight = False
        self.run_start_time = 0.0
        self.fail_requested = False

        # Pulse animation tick
        self.anim_tick = 0

        self.protocol("WM_DELETE_WINDOW", self.on_close)

        self._build_ui()
        self.log(f"Started lightbulb GUI watching {self.fires_dir}")
        self.log(f"Integration app_name: '{self.app_name}'")

        # Start non-blocking tick loops
        self.after(250, self.tick_poll_fires)
        self.after(50, self.tick_ui)

    def _build_ui(self):
        # Outer container frame
        main_frame = tk.Frame(self, bg=BG_COLOR, padx=16, pady=16)
        main_frame.pack(fill=tk.BOTH, expand=True)

        # 1. Header
        header_frame = tk.Frame(main_frame, bg=BG_COLOR)
        header_frame.pack(fill=tk.X, pady=(0, 12))

        title_lbl = tk.Label(
            header_frame, text="BELLMAN LIGHTBULB DEMO", font=self.f_title,
            fg=NEON_CYAN, bg=BG_COLOR
        )
        title_lbl.pack(anchor="w")

        sub_lbl = tk.Label(
            header_frame,
            text=f"Slots: {self.slots_root}  |  App Owner: {self.app_name}",
            font=self.f_sub, fg=NEON_VIOLET, bg=BG_COLOR
        )
        sub_lbl.pack(anchor="w", pady=(2, 0))

        # Thin rule
        rule1 = tk.Frame(main_frame, bg=NEON_CYAN, height=1)
        rule1.pack(fill=tk.X, pady=(0, 12))

        # 2. Schedule Timer Panel
        sched_panel = tk.Frame(main_frame, bg=PANEL_BG, highlightbackground=NEON_VIOLET, highlightthickness=1, padx=12, pady=10)
        sched_panel.pack(fill=tk.X, pady=(0, 12))

        p_title = tk.Label(sched_panel, text="1. SCHEDULE A TIMER", font=self.f_head, fg=NEON_CYAN, bg=PANEL_BG)
        p_title.pack(anchor="w", pady=(0, 6))

        picker_frame = tk.Frame(sched_panel, bg=PANEL_BG)
        picker_frame.pack(fill=tk.X, pady=(0, 6))

        self.sched_mode_var = tk.StringVar(value="in_n_secs")

        r1 = tk.Radiobutton(
            picker_frame, text="in N seconds", variable=self.sched_mode_var,
            value="in_n_secs", font=self.f_body, fg=TEXT_LIGHT, bg=PANEL_BG,
            selectcolor=BG_COLOR, activebackground=PANEL_BG, activeforeground=NEON_CYAN,
            command=self._on_sched_mode_change
        )
        r1.pack(side=tk.LEFT, padx=(0, 12))

        r2 = tk.Radiobutton(
            picker_frame, text="at HH:MM", variable=self.sched_mode_var,
            value="at_hhmm", font=self.f_body, fg=TEXT_LIGHT, bg=PANEL_BG,
            selectcolor=BG_COLOR, activebackground=PANEL_BG, activeforeground=NEON_CYAN,
            command=self._on_sched_mode_change
        )
        r2.pack(side=tk.LEFT, padx=(0, 12))

        r3 = tk.Radiobutton(
            picker_frame, text="every N minutes", variable=self.sched_mode_var,
            value="every_n_mins", font=self.f_body, fg=TEXT_LIGHT, bg=PANEL_BG,
            selectcolor=BG_COLOR, activebackground=PANEL_BG, activeforeground=NEON_CYAN,
            command=self._on_sched_mode_change
        )
        r3.pack(side=tk.LEFT)

        entry_frame = tk.Frame(sched_panel, bg=PANEL_BG)
        entry_frame.pack(fill=tk.X, pady=(0, 8))

        self.val_lbl = tk.Label(entry_frame, text="Seconds:", font=self.f_body, fg=TEXT_LIGHT, bg=PANEL_BG)
        self.val_lbl.pack(side=tk.LEFT, padx=(0, 6))

        self.val_entry = tk.Entry(
            entry_frame, font=self.f_body, bg=BG_COLOR, fg=NEON_CYAN,
            insertbackground=NEON_CYAN, relief=tk.FLAT, highlightbackground=NEON_VIOLET,
            highlightthickness=1, width=12
        )
        self.val_entry.insert(0, "10")
        self.val_entry.pack(side=tk.LEFT, padx=(0, 16))

        self.btn_create = tk.Button(
            entry_frame, text="Create Timer", font=self.f_head, bg=BG_COLOR, fg=NEON_CYAN,
            activebackground=NEON_CYAN, activeforeground=BG_COLOR, relief=tk.FLAT,
            highlightbackground=NEON_CYAN, highlightthickness=1, padx=10, pady=2,
            command=self.on_create_timer
        )
        self.btn_create.pack(side=tk.LEFT, padx=(0, 8))

        self.btn_remove = tk.Button(
            entry_frame, text="Remove Timer", font=self.f_head, bg=BG_COLOR, fg=TEXT_DIM,
            activebackground=FAILURE_RED, activeforeground=TEXT_LIGHT, relief=tk.FLAT,
            highlightbackground=TEXT_DIM, highlightthickness=1, padx=10, pady=2,
            state=tk.DISABLED, command=self.on_remove_timer
        )
        self.btn_remove.pack(side=tk.LEFT)

        # 3. Timer Details & Countdown Panel
        details_panel = tk.Frame(main_frame, bg=PANEL_BG, highlightbackground=NEON_VIOLET, highlightthickness=1, padx=12, pady=10)
        details_panel.pack(fill=tk.X, pady=(0, 12))

        d_title = tk.Label(details_panel, text="2. TIMER STATUS & COUNTDOWN", font=self.f_head, fg=NEON_CYAN, bg=PANEL_BG)
        d_title.pack(anchor="w", pady=(0, 4))

        self.lbl_timer_id = tk.Label(
            details_panel, text="Timer ID:     (none)", font=self.f_body,
            fg=TEXT_LIGHT, bg=PANEL_BG
        )
        self.lbl_timer_id.pack(anchor="w")

        self.lbl_next_fire = tk.Label(
            details_panel, text="Next Fire At: (none)", font=self.f_body,
            fg=NEON_VIOLET, bg=PANEL_BG
        )
        self.lbl_next_fire.pack(anchor="w")

        self.lbl_countdown = tk.Label(
            details_panel, text="Countdown:   Idle", font=self.f_large,
            fg=NEON_CYAN, bg=PANEL_BG
        )
        self.lbl_countdown.pack(anchor="w", pady=(4, 0))

        # 4. Canvas - Golden Bulb & Glow
        bulb_panel = tk.Frame(main_frame, bg=PANEL_BG, highlightbackground=NEON_MAGENTA, highlightthickness=1, padx=12, pady=10)
        bulb_panel.pack(fill=tk.X, pady=(0, 12))

        b_title = tk.Label(bulb_panel, text="3. THE GOLDEN BULB", font=self.f_head, fg=NEON_MAGENTA, bg=PANEL_BG)
        b_title.pack(anchor="w", pady=(0, 6))

        canvas_frame = tk.Frame(bulb_panel, bg=BG_COLOR)
        canvas_frame.pack(fill=tk.X)

        self.canvas = tk.Canvas(
            canvas_frame, width=680, height=180, bg=BG_COLOR, highlightthickness=0
        )
        self.canvas.pack(fill=tk.BOTH, expand=True)

        # 5. Handshake Protocol Chips
        chips_panel = tk.Frame(main_frame, bg=PANEL_BG, highlightbackground=NEON_CYAN, highlightthickness=1, padx=12, pady=10)
        chips_panel.pack(fill=tk.X, pady=(0, 12))

        c_title = tk.Label(chips_panel, text="4. PROTOCOL HANDSHAKE CHIPS", font=self.f_head, fg=NEON_CYAN, bg=PANEL_BG)
        c_title.pack(anchor="w", pady=(0, 8))

        chips_row = tk.Frame(chips_panel, bg=PANEL_BG)
        chips_row.pack(fill=tk.X)

        self.chip_widgets = {}
        chip_specs = [
            ("fired", "FIRED", NEON_CYAN),
            ("acknowledged", "ACKNOWLEDGED", NEON_CYAN),
            ("running", "RUNNING", NEON_MAGENTA),
            ("completed", "COMPLETED", SUCCESS_GREEN),
        ]

        for idx, (key, label, color) in enumerate(chip_specs):
            col_frame = tk.Frame(chips_row, bg=PANEL_BG)
            col_frame.pack(side=tk.LEFT, expand=True, fill=tk.BOTH)

            chip_box = tk.Frame(col_frame, bg=BG_COLOR, highlightbackground=TEXT_DIM, highlightthickness=1, padx=8, pady=6)
            chip_box.pack(fill=tk.X, padx=4)

            lbl_name = tk.Label(chip_box, text=f"○ {label}", font=self.f_body, fg=TEXT_DIM, bg=BG_COLOR)
            lbl_name.pack()

            lbl_time = tk.Label(chip_box, text="-", font=self.f_small, fg=TEXT_DIM, bg=BG_COLOR)
            lbl_time.pack()

            self.chip_widgets[key] = {
                "box": chip_box,
                "name": lbl_name,
                "time": lbl_time,
                "default_color": color,
                "label_text": label,
            }

        # 6. Failure trigger & Log Strip
        control_panel = tk.Frame(main_frame, bg=PANEL_BG, highlightbackground=NEON_VIOLET, highlightthickness=1, padx=12, pady=10)
        control_panel.pack(fill=tk.BOTH, expand=True)

        c_top = tk.Frame(control_panel, bg=PANEL_BG)
        c_top.pack(fill=tk.X, pady=(0, 6))

        l_title = tk.Label(c_top, text="5. INTERACTION & LOGS", font=self.f_head, fg=NEON_CYAN, bg=PANEL_BG)
        l_title.pack(side=tk.LEFT)

        self.btn_fail = tk.Button(
            c_top, text="Make It Fail", font=self.f_head, bg=BG_COLOR, fg=FAILURE_RED,
            activebackground=FAILURE_RED, activeforeground=BG_COLOR, relief=tk.FLAT,
            highlightbackground=FAILURE_RED, highlightthickness=1, padx=10, pady=2,
            state=tk.DISABLED, command=self.on_make_it_fail
        )
        self.btn_fail.pack(side=tk.RIGHT)

        log_frame = tk.Frame(control_panel, bg=BG_COLOR)
        log_frame.pack(fill=tk.BOTH, expand=True)

        self.log_text = tk.Text(
            log_frame, font=self.f_small, bg=BG_COLOR, fg=NEON_CYAN,
            insertbackground=NEON_CYAN, relief=tk.FLAT, height=6, wrap=tk.WORD
        )
        self.log_text.pack(side=tk.LEFT, fill=tk.BOTH, expand=True)

        scrollbar = tk.Scrollbar(log_frame, command=self.log_text.yview, bg=PANEL_BG)
        scrollbar.pack(side=tk.RIGHT, fill=tk.Y)
        self.log_text.config(yscrollcommand=scrollbar.set)

    def _on_sched_mode_change(self):
        mode = self.sched_mode_var.get()
        if mode == "in_n_secs":
            self.val_lbl.config(text="Seconds:")
            self.val_entry.delete(0, tk.END)
            self.val_entry.insert(0, "10")
        elif mode == "at_hhmm":
            self.val_lbl.config(text="Time HH:MM:")
            self.val_entry.delete(0, tk.END)
            # Default to 1 minute from now
            now = datetime.now()
            t_next = now.timestamp() + 60
            hhmm = datetime.fromtimestamp(t_next).strftime("%H:%M")
            self.val_entry.insert(0, hhmm)
        elif mode == "every_n_mins":
            self.val_lbl.config(text="Minutes:")
            self.val_entry.delete(0, tk.END)
            self.val_entry.insert(0, "1")

    def log(self, msg):
        timestamp = now_time_str()
        line = f"[{timestamp}] {msg}\n"
        self.log_text.insert(tk.END, line)
        self.log_text.see(tk.END)

    def set_chip_state(self, key, is_lit, timestamp_str="-", custom_color=None, custom_text=None):
        if key not in self.chip_widgets:
            return
        chip = self.chip_widgets[key]
        color = custom_color or chip["default_color"]
        label_text = custom_text or chip["label_text"]

        if is_lit:
            chip["box"].config(highlightbackground=color)
            chip["name"].config(text=f"● {label_text}", fg=color)
            chip["time"].config(text=timestamp_str, fg=TEXT_LIGHT)
        else:
            chip["box"].config(highlightbackground=TEXT_DIM)
            chip["name"].config(text=f"○ {label_text}", fg=TEXT_DIM)
            chip["time"].config(text="-", fg=TEXT_DIM)

    def reset_chips(self):
        for key in self.chip_widgets:
            self.set_chip_state(key, False)

    def on_create_timer(self):
        mode = self.sched_mode_var.get()
        val_str = self.val_entry.get().strip()

        try:
            if mode == "in_n_secs":
                secs = int(val_str)
                if secs < 1:
                    raise ValueError("Seconds must be >= 1")
                fire_dt = datetime.now(timezone.utc) + timedelta(seconds=secs)
                time_str = fire_dt.strftime("%Y-%m-%dT%H:%M:%S")
                occ = {"kind": "once", "time": time_str}
                desc = f"once in {secs}s ({time_str})"
            elif mode == "at_hhmm":
                parts = val_str.split(":")
                if len(parts) != 2:
                    raise ValueError("Format must be HH:MM")
                hh, mm = int(parts[0]), int(parts[1])
                if not (0 <= hh <= 23 and 0 <= mm <= 59):
                    raise ValueError("Invalid HH:MM time")
                time_s = f"{hh:02d}:{mm:02d}:00"
                occ = {"kind": "daily", "time": time_s}
                desc = f"daily at {time_s}"
            elif mode == "every_n_mins":
                mins = int(val_str)
                if mins < 1:
                    raise ValueError("Minutes must be >= 1")
                secs = mins * 60
                occ = {"kind": "interval", "every_secs": secs}
                desc = f"interval every {mins}m"
        except ValueError as err:
            messagebox.showerror("Invalid Input", str(err), parent=self)
            return

        payload = {
            "app_name": self.app_name,
            "timer_name": "lightbulb-gui-demo",
            "tz": "UTC",
            "occurrence": occ,
        }

        try:
            slot_id, req_id = claim_and_publish_slot(self.slots_root, "add", payload)
            self.log(f"Published 'add' request via slot-{slot_id} (request_id={req_id[:8]}...) [{desc}]")
            self.lbl_countdown.config(text="Waiting for slot response...", fg=NEON_CYAN)

            # Poll for done/slot-NNNN.json
            self.after(100, lambda: self._poll_slot_done(slot_id, req_id))
        except Exception as err:
            self.log(f"ERROR publishing slot add: {err}")
            messagebox.showerror("Slot Publish Error", str(err), parent=self)

    def _poll_slot_done(self, slot_id, req_id, attempts=0):
        done_file = self.slots_root / "done" / f"slot-{slot_id}.json"
        if done_file.exists():
            try:
                with open(done_file, "r", encoding="utf-8") as f:
                    res = json.load(f)
                if res.get("request_id") == req_id:
                    if res.get("status") == "ok":
                        self.active_timer_id = res.get("timer_id")
                        self.active_timer_name = "lightbulb-gui-demo"
                        self.next_fire_at = res.get("next_fire_at")

                        self.lbl_timer_id.config(text=f"Timer ID:     {self.active_timer_id}")
                        self.lbl_next_fire.config(text=f"Next Fire At: {self.next_fire_at}")
                        self.btn_remove.config(state=tk.NORMAL, fg=FAILURE_RED, highlightbackground=FAILURE_RED)
                        self.log(f"Timer registered! ID={self.active_timer_id} NextFire={self.next_fire_at}")

                        # Parse next_fire_at for countdown
                        if self.next_fire_at:
                            try:
                                iso_str = self.next_fire_at.replace("Z", "+00:00")
                                self.next_fire_dt = datetime.fromisoformat(iso_str)
                            except ValueError:
                                self.next_fire_dt = None
                        return
                    else:
                        err_msg = res.get("error", "Unknown slot error")
                        self.log(f"Slot response error: {err_msg}")
                        messagebox.showerror("Slot Error", err_msg, parent=self)
                        return
            except (OSError, ValueError):
                pass

        if attempts < 50:  # 5 seconds max wait
            self.after(100, lambda: self._poll_slot_done(slot_id, req_id, attempts + 1))
        else:
            self.log("Timed out waiting for slot done response.")

    def on_remove_timer(self):
        if not self.active_timer_id:
            return

        payload = {
            "app_name": self.app_name,
            "timer_id": self.active_timer_id,
        }

        try:
            slot_id, req_id = claim_and_publish_slot(self.slots_root, "delete", payload)
            self.log(f"Published 'delete' request via slot-{slot_id} for timer {self.active_timer_id[:8]}...")

            self.after(100, lambda: self._poll_delete_done(slot_id, req_id))
        except Exception as err:
            self.log(f"ERROR publishing slot delete: {err}")
            messagebox.showerror("Slot Delete Error", str(err), parent=self)

    def _poll_delete_done(self, slot_id, req_id, attempts=0):
        done_file = self.slots_root / "done" / f"slot-{slot_id}.json"
        if done_file.exists():
            try:
                with open(done_file, "r", encoding="utf-8") as f:
                    res = json.load(f)
                if res.get("request_id") == req_id:
                    if res.get("status") == "ok":
                        self.log(f"Timer {self.active_timer_id[:8]}... successfully removed!")
                        self.active_timer_id = None
                        self.active_timer_name = None
                        self.next_fire_at = None
                        self.next_fire_dt = None
                        self.lbl_timer_id.config(text="Timer ID:     (none)")
                        self.lbl_next_fire.config(text="Next Fire At: (none)")
                        self.lbl_countdown.config(text="Countdown:   Idle", fg=NEON_CYAN)
                        self.btn_remove.config(state=tk.DISABLED, fg=TEXT_DIM, highlightbackground=TEXT_DIM)
                        return
            except (OSError, ValueError):
                pass

        if attempts < 50:
            self.after(100, lambda: self._poll_delete_done(slot_id, req_id, attempts + 1))
        else:
            self.log("Timed out waiting for delete response.")

    def on_make_it_fail(self):
        if self.run_in_flight:
            self.fail_requested = True
            self.log("User triggered 'Make it fail'!")

    def tick_poll_fires(self):
        """Rescan slots/fires/ for fire notifications matching our app_name."""
        try:
            if self.fires_dir.exists():
                for fire_file in sorted(self.fires_dir.glob("fire-*.json")):
                    try:
                        with open(fire_file, "r", encoding="utf-8") as f:
                            fire = json.load(f)
                    except (OSError, ValueError):
                        continue

                    if fire.get("app_name") != self.app_name:
                        continue  # Not for us

                    run_id = fire.get("run_id")
                    if not run_id or run_id in self.seen_runs:
                        continue  # Already handled (deduplication by run_id)

                    self.seen_runs.add(run_id)
                    self.log(f"Fire received! run_id={run_id[:8]}...")
                    self.start_run_handshake(fire)
                    break
        except Exception as err:
            self.log(f"Error checking fires: {err}")

        self.after(250, self.tick_poll_fires)

    def start_run_handshake(self, fire):
        self.current_fire = fire
        self.run_in_flight = True
        self.run_start_time = time.monotonic()
        self.fail_requested = False
        self.btn_fail.config(state=tk.NORMAL)

        # 1. fired
        fired_raw = fire.get("fired_at", "")
        if "T" in fired_raw:
            fired_ts = fired_raw.split("T")[1].rstrip("Z").split(".")[0]
        else:
            fired_ts = now_time_str()
        self.set_chip_state("fired", True, fired_ts)

        # 2. acknowledge
        try:
            reply(fire, state="acknowledged", acknowledged_at=now_utc(), expected_secs=int(self.on_secs))
            ack_ts = now_time_str()
            self.set_chip_state("acknowledged", True, ack_ts)
            self.log(f"Written reply 'acknowledged' (expected_secs={int(self.on_secs)})")
        except Exception as err:
            self.log(f"ERROR writing acknowledge reply: {err}")

        # 3. running
        run_ts = now_time_str()
        self.set_chip_state("running", True, run_ts)
        self.log(f"Run active! Lightbulb ON for {self.on_secs}s")

    def tick_ui(self):
        """Drive countdowns, animations, and run ticks."""
        self.anim_tick += 1
        now_mono = time.monotonic()

        # Update general countdown if timer is active & not running
        if self.next_fire_dt and not self.run_in_flight:
            now_dt = datetime.now(timezone.utc)
            diff_secs = (self.next_fire_dt - now_dt).total_seconds()
            if diff_secs > 0:
                self.lbl_countdown.config(text=f"Countdown:   Fires in {diff_secs:4.1f}s", fg=NEON_CYAN)
            else:
                self.lbl_countdown.config(text="Countdown:   Firing due...", fg=NEON_MAGENTA)

        # Handle active run in flight
        if self.run_in_flight and self.current_fire:
            elapsed = now_mono - self.run_start_time
            remaining = self.on_secs - elapsed

            # Check if user requested failure
            if self.fail_requested:
                self._finish_run_failed("User requested failure")
            elif remaining <= 0:
                self._finish_run_completed(elapsed)
            else:
                self.lbl_countdown.config(
                    text=f"BULB ON — {remaining:4.1f}s remaining", fg=BULB_ON
                )
                self.draw_bulb(on=True, pulse_step=self.anim_tick, remaining=remaining)
        else:
            self.draw_bulb(on=False)

        self.after(50, self.tick_ui)

    def _finish_run_completed(self, duration):
        self.run_in_flight = False
        self.btn_fail.config(state=tk.DISABLED)
        comp_ts = now_time_str()
        self.set_chip_state("completed", True, comp_ts)
        self.lbl_countdown.config(text="Run Completed!", fg=SUCCESS_GREEN)

        try:
            reply(
                self.current_fire,
                state="completed",
                completed_at=now_utc(),
                result={"on_duration_secs": round(duration, 2)},
            )
            self.log(f"Written reply 'completed' (duration={duration:.2f}s)")
        except Exception as err:
            self.log(f"ERROR writing completed reply: {err}")

        self.current_fire = None

    def _finish_run_failed(self, reason):
        self.run_in_flight = False
        self.btn_fail.config(state=tk.DISABLED)
        fail_ts = now_time_str()

        # Update completed chip to show RED failed state
        self.set_chip_state(
            "completed", True, fail_ts, custom_color=FAILURE_RED, custom_text="FAILED"
        )
        self.lbl_countdown.config(text=f"Run Failed: {reason}", fg=FAILURE_RED)

        try:
            reply(
                self.current_fire,
                state="failed",
                failed_at=now_utc(),
                reason=reason,
            )
            self.log(f"Written reply 'failed' (reason='{reason}')")
        except Exception as err:
            self.log(f"ERROR writing failed reply: {err}")

        self.current_fire = None

    def draw_bulb(self, on=False, pulse_step=0, remaining=0.0):
        c = self.canvas
        c.delete("all")

        width = c.winfo_width() or 680
        height = c.winfo_height() or 180
        cx = width / 2
        cy = height / 2 - 10

        # Draw Glow Halo (concentric ovals)
        if on:
            pulse = 1.0 + 0.06 * math.sin(pulse_step * 0.2)
            num_rings = 12
            max_r = 75 * pulse
            min_r = 25

            for i in range(num_rings - 1, -1, -1):
                factor = (1.0 - (i / float(num_rings))) ** 1.8
                r = min_r + (max_r - min_r) * (i / float(num_rings))
                ring_color = interpolate_color(BG_COLOR, BULB_ON, factor)
                c.create_oval(
                    cx - r, cy - r, cx + r, cy + r,
                    fill=ring_color, outline=""
                )

        # Bulb Glass & Base
        glass_r = 24
        glass_fill = BULB_ON if on else BULB_OFF
        glass_outline = "#ffe082" if on else "#554835"

        # Screw Base / Socket below glass
        c.create_rectangle(
            cx - 10, cy + 18, cx + 10, cy + 34,
            fill="#333344", outline="#555566", width=1
        )
        c.create_rectangle(
            cx - 12, cy + 14, cx + 12, cy + 18,
            fill="#444455", outline="#666677", width=1
        )

        # Glass Globe
        c.create_oval(
            cx - glass_r, cy - glass_r, cx + glass_r, cy + glass_r,
            fill=glass_fill, outline=glass_outline, width=2
        )

        # Inner Filament
        fil_color = "#ffffff" if on else "#665544"
        c.create_line(cx - 8, cy + 10, cx - 4, cy - 6, fill=fil_color, width=2)
        c.create_line(cx + 8, cy + 10, cx + 4, cy - 6, fill=fil_color, width=2)
        c.create_line(cx - 4, cy - 6, cx + 4, cy - 6, fill=fil_color, width=2)

        # Status text below bulb
        if on:
            status_text = f"💡 BULB IS LIT — {remaining:4.1f}s remaining"
            text_color = BULB_ON
        else:
            status_text = "💡 Lightbulb is OFF"
            text_color = TEXT_DIM

        c.create_text(
            cx, cy + 56, text=status_text, font=self.f_head, fill=text_color
        )

    def on_close(self):
        if self.active_timer_id:
            ans = messagebox.askyesno(
                "Remove Timer?",
                f"You have an active timer ({self.active_timer_id[:8]}...).\nRemove it before closing?",
                parent=self,
            )
            if ans:
                try:
                    payload = {"app_name": self.app_name, "timer_id": self.active_timer_id}
                    claim_and_publish_slot(self.slots_root, "delete", payload)
                    self.log("Published cleanup delete request on exit.")
                except Exception:
                    pass
        self.destroy()


def main():
    parser = argparse.ArgumentParser(description="Bellman Lightbulb GUI demo app")
    parser.add_argument(
        "--slots",
        default=os.environ.get(
            "BELLMAN_SLOTS", str(pathlib.Path.home() / ".bellman" / "slots")
        ),
        help="Path to slots root directory",
    )
    parser.add_argument(
        "--app-name", default="lightbulb-gui", help="Integration app owner name"
    )
    parser.add_argument(
        "--on-secs", type=float, default=15, help="Duration bulb stays on per fire"
    )

    args = parser.parse_args()

    app = LightbulbGUI(slots_dir=args.slots, app_name=args.app_name, on_secs=args.on_secs)
    app.mainloop()


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(0)
