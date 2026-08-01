#!/usr/bin/env python3
"""Shared helpers for the C11 end-to-end runs.

Everything here drives Bellman the way a third party would: the desktop app
runs (that is what owns the scheduler clock), timers arrive through the slot
protocol, and evidence is read out of the event log, status.json and the
fires/ notifications. No `run-now` anywhere.
"""
import json
import os
import shutil
import signal
import subprocess
import sys
import time
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path

# Repo root: this file lives at <repo>/docs/qa-c11/harness/, so walk up.
# BELLMAN_ROOT overrides it when the harness is copied elsewhere.
REPO = Path(os.environ.get("BELLMAN_ROOT",
                           Path(__file__).resolve().parents[3]))
# Where the isolated sessions live. Never inside the repo.
RUN_ROOT = Path(os.environ.get("BELLMAN_QA_RUN_ROOT", "/tmp/c11"))
CLI = REPO / "target" / "release" / "bellman"
APP = REPO / "target" / "release" / "bellman-app"


class Run:
    """One isolated Bellman desktop session on a private Xvfb display."""

    def __init__(self, name, display=":91", evidence_root=None):
        self.name = name
        self.display = display
        self.root = Path(evidence_root or RUN_ROOT / name)
        self.xdg = self.root / "xdg"
        self.cfg = self.root / "cfg"
        self.rt = self.root / "run"
        self.appdata = self.xdg / "io.bellman.desktop"
        self.xvfb = None
        self.wm = None
        self.app = None
        self.applog = None

    # -- lifecycle ------------------------------------------------------
    def fresh(self):
        if self.root.exists():
            # xdg-desktop-portal leaves a fuse.portal mount under the private
            # XDG_RUNTIME_DIR; unmount it before the tree can be removed.
            for sub in ("run/doc", "run/gvfs"):
                subprocess.run(["fusermount", "-uz", str(self.root / sub)],
                               capture_output=True)
            shutil.rmtree(self.root, ignore_errors=True)
            if self.root.exists():
                shutil.rmtree(self.root)
        for d in (self.xdg, self.cfg, self.rt):
            d.mkdir(parents=True, exist_ok=True)
        self.rt.chmod(0o700)
        return self

    def seed_config(self, **keys):
        """Pre-write config.json the way a user editing the documented file would."""
        self.appdata.mkdir(parents=True, exist_ok=True)
        cfg = {"wizard_completed": True, "autostart_enabled": False,
               "start_minimized": False, "wake_enabled": False}
        cfg.update(keys)
        (self.appdata / "config.json").write_text(json.dumps(cfg, indent=2))
        return self

    def env(self):
        e = dict(os.environ)
        e.update(
            DISPLAY=self.display,
            XDG_DATA_HOME=str(self.xdg),
            XDG_CONFIG_HOME=str(self.cfg),
            XDG_RUNTIME_DIR=str(self.rt),
            XDG_CACHE_HOME=str(self.root / "cache"),
            HOME=str(self.root),
            GDK_BACKEND="x11",
            GIO_USE_VFS="local",
            GTK_USE_PORTAL="0",
            LIBGL_ALWAYS_SOFTWARE="1",
            WEBKIT_DISABLE_COMPOSITING_MODE="1",
        )
        # A private session bus: Tauri's single-instance plugin is keyed on the
        # session bus, so without this the operator's own Bellman would make
        # every launch here exit immediately.
        e.pop("DBUS_SESSION_BUS_ADDRESS", None)
        e.pop("BELLMAN_DB", None)
        e.pop("BELLMAN_SLOTS", None)
        return e

    def start_xvfb(self):
        if self.xvfb:
            return
        self.xvfb = subprocess.Popen(
            ["Xvfb", self.display, "-screen", "0", "1280x800x24", "-ac",
             "+extension", "RANDR", "+extension", "GLX", "-nolisten", "tcp"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(1.5)
        e = dict(os.environ, DISPLAY=self.display)
        self.wm = subprocess.Popen(["metacity", "--sm-disable", "--replace"],
                                   env=e, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL)
        time.sleep(1.0)

    def start_app(self, tag="app", timeout=180):
        self.start_xvfb()
        self.root.mkdir(parents=True, exist_ok=True)
        (self.root / "cache").mkdir(exist_ok=True)
        logp = self.root / f"{tag}.log"
        self.applog = open(logp, "ab")
        self.applog.write(f"\n=== start {datetime.now(timezone.utc).isoformat()} ===\n".encode())
        self.applog.flush()
        self.app = subprocess.Popen(["dbus-run-session", "--", str(APP)],
                                    env=self.env(), stdout=self.applog,
                                    stderr=self.applog, start_new_session=True)
        # Wait for the store to appear — that is the app owning its data dir.
        # On a private session bus xdg-desktop-portal spends ~25 s failing to
        # reach org.freedesktop.secrets before the app's setup() runs; that is
        # an isolation artefact, not Bellman being slow on a real desktop.
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if (self.appdata / "timers.db").exists():
                time.sleep(3.0)   # let the scheduler thread settle
                return self
            if self.app.poll() is not None:
                raise RuntimeError(f"bellman-app exited early: see {logp}")
            time.sleep(0.2)
        raise RuntimeError(f"bellman-app never created {self.appdata/'timers.db'}")

    def stop_app(self, sig=signal.SIGTERM, wait=20):
        if not self.app:
            return
        try:
            os.killpg(os.getpgid(self.app.pid), sig)
        except ProcessLookupError:
            pass
        try:
            self.app.wait(timeout=wait)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(os.getpgid(self.app.pid), signal.SIGKILL)
            except ProcessLookupError:
                pass
            self.app.wait(timeout=10)
        self.app = None
        time.sleep(1.0)

    def stop(self):
        self.stop_app()
        if self.wm:
            self.wm.terminate()
            try:
                self.wm.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.wm.kill()
            self.wm = None
        if self.xvfb:
            self.xvfb.terminate()
            try:
                self.xvfb.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.xvfb.kill()
            self.xvfb = None
        if self.applog:
            self.applog.close()
            self.applog = None
        for sub in ("run/doc", "run/gvfs"):
            subprocess.run(["fusermount", "-uz", str(self.root / sub)],
                           capture_output=True)

    # -- paths ----------------------------------------------------------
    @property
    def db(self):
        return self.appdata / "timers.db"

    @property
    def slots(self):
        return self.appdata / "slots"

    @property
    def events(self):
        return self.appdata / "logs" / "events.current.jsonl"

    @property
    def timers_dir(self):
        return self.appdata / "timers"

    # -- CLI ------------------------------------------------------------
    def cli(self, *args, check=True):
        cmd = [str(CLI), *args, "--db", str(self.db)]
        p = subprocess.run(cmd, capture_output=True, text=True, env=self.env())
        if check and p.returncode != 0:
            raise RuntimeError(f"{' '.join(cmd)} -> {p.returncode}\n{p.stdout}\n{p.stderr}")
        return p

    def cli_json(self, *args, check=True):
        p = self.cli(*args, "--json", check=check)
        return json.loads(p.stdout.strip().splitlines()[-1])

    # -- slot protocol --------------------------------------------------
    def submit(self, payload, operation="add", request_id=None):
        req = {
            "schema": "bellman-slot/1",
            "request_id": request_id or str(uuid.uuid4()),
            "operation": operation,
            "payload": payload,
        }
        f = self.root / f"req-{req['request_id']}.json"
        f.write_text(json.dumps(req))
        p = subprocess.run(
            [str(CLI), "slot-submit", str(f), "--slots", str(self.slots),
             "--db", str(self.db), "--json"],
            capture_output=True, text=True, env=self.env())
        if p.returncode != 0:
            raise RuntimeError(f"slot-submit failed: {p.stdout}\n{p.stderr}")
        return json.loads(p.stdout.strip().splitlines()[-1])

    # -- evidence -------------------------------------------------------
    def log_lines(self, since=0):
        if not self.events.exists():
            return []
        out = []
        for i, line in enumerate(self.events.read_text(errors="replace").splitlines()):
            if i < since:
                continue
            line = line.strip()
            if not line:
                continue
            try:
                out.append(json.loads(line))
            except ValueError:
                continue
        return out

    def log_count(self):
        if not self.events.exists():
            return 0
        return len(self.events.read_text(errors="replace").splitlines())

    def wait_for_event(self, pred, timeout=180, poll=0.5):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            for ev in self.log_lines():
                if pred(ev):
                    return ev
            time.sleep(poll)
        return None

    def status(self, timer_name):
        for d in sorted(self.timers_dir.glob(f"{timer_name}-*")):
            p = d / "status.json"
            if p.exists():
                try:
                    return json.loads(p.read_text())
                except ValueError:
                    return None
        return None

    def timer_dir(self, timer_name):
        for d in sorted(self.timers_dir.glob(f"{timer_name}-*")):
            return d
        return None


def utcnow():
    return datetime.now(timezone.utc)


def stamp(dt=None):
    return (dt or utcnow()).strftime("%Y-%m-%dT%H:%M:%SZ")


def say(*a):
    print(f"[{datetime.now().strftime('%H:%M:%S')}]", *a, flush=True)
