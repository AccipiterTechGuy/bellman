# Bellman screenshots

Back to the [README](../README.md).

Every screen of the desktop app, with sample timers. To take them again, see
[Retaking the screenshots](#retaking-the-screenshots) at the end.

## All timers

Every timer in one list: what it does, when it fires next, and buttons to edit
it, read its log or run it now.

<p align="center">
  <img src="screenshots/all-timers.png" alt="Bellman — All timers list with next-fire times, actions and Run now controls" width="880" />
</p>

## A run in progress

When a timer wakes an application, its row shows the run live: here the
lightbulb demo has acknowledged the wake and is still running.

<p align="center">
  <img src="screenshots/run-live.png" alt="Bellman — All timers list with the Lightbulb demo run acknowledged and in progress" width="880" />
</p>

## Timer log

The Log button opens a timer's latest run and its recent events, from fired
to completed.

<p align="center">
  <img src="screenshots/timer-log.png" alt="Bellman — Log view of the Lightbulb demo timer with the current run and event log" width="880" />
</p>

## New timer

Create a timer: pick how often it repeats, the time and timezone, and what it
does when it fires.

<p align="center">
  <img src="screenshots/timer-new.png" alt="Bellman — empty New timer dialog" width="880" />
</p>

## Edit timer

Change a timer and check the next five fire times, and any timers that fire
at the same moment, before you save.

<p align="center">
  <img src="screenshots/timer-edit.png" alt="Bellman — Edit timer dialog for Stand-up reminder with a preview of the next five fires" width="880" />
</p>

## Week

The current week, day by day: runs that already happened and the ones still
to come.

<p align="center">
  <img src="screenshots/week.png" alt="Bellman — Week view with recorded and upcoming runs" width="880" />
</p>

## Month

A month calendar with each day's runs; click a day to add a timer on it.

<p align="center">
  <img src="screenshots/month.png" alt="Bellman — Month calendar" width="880" />
</p>

## Run history

Every event Bellman recorded, newest first, filterable by timer and kind.

<p align="center">
  <img src="screenshots/run-history.png" alt="Bellman — Run history event log" width="880" />
</p>

## Settings

Wake from sleep and autostart: how Bellman starts and whether it may wake the
machine.

<p align="center">
  <img src="screenshots/settings.png" alt="Bellman — Settings: wake from sleep and autostart" width="880" />
</p>

## Settings, continued

Defaults for missed runs, the engine (including pause all timers), and the
demo.

<p align="center">
  <img src="screenshots/settings-2.png" alt="Bellman — Settings: misfire defaults, engine and demo" width="880" />
</p>

## Demo

The lightbulb demo: a tiny example app that Bellman wakes on a schedule, and
where Bellman keeps its data.

<p align="center">
  <img src="screenshots/demo.png" alt="Bellman — Settings demo panel with the lightbulb demo and the data folders" width="880" />
</p>

## First-run wizard: start

The first time Bellman opens it asks a few questions: start at login, start
hidden in the tray, and whether to show the demo.

<p align="center">
  <img src="screenshots/wizard-1.png" alt="Bellman — setup wizard, first step" width="880" />
</p>

## First-run wizard: wake from sleep

Choose whether Bellman may wake the machine from sleep so timers fire on time.

<p align="center">
  <img src="screenshots/wizard-2.png" alt="Bellman — setup wizard, wake from sleep step" width="880" />
</p>

## First-run wizard: done

A summary: the wake status, a dependency check, and the demo if you asked
for it.

<p align="center">
  <img src="screenshots/wizard-3.png" alt="Bellman — setup wizard, setup complete" width="880" />
</p>

## Retaking the screenshots

On Linux, with the GUI test prerequisites from
[BUILD_PLAN.md](BUILD_PLAN.md) installed and the app built from this tree
(`cargo tauri build --no-bundle`):

```sh
/tmp/bellman-qa-venv/bin/python scripts/capture_docs_screenshots.py
```

It runs the app on its own hidden display with sample timers in a throwaway
folder, and never touches your own timers. The tray menu is not captured: it
is drawn by the desktop's tray, which the script does not click.

All product shots live in [`screenshots/`](screenshots/). QA before/after
captures are under [`qa4-screenshots/`](qa4-screenshots/).
