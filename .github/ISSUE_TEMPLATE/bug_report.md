---
name: Bug report
about: Something built today behaves wrongly
title: ""
labels: bug
assignees: ""
---

<!-- Bellman is pre-release (see the README banner). Please don't file
     missing-feature requests here — use it for things that exist and
     misbehave. Security reports do NOT belong here: see SECURITY.md. -->

**What did you do, what did you expect, what happened instead?**

**Where did it happen?**

- Interface: GUI / `bellman` CLI / slot protocol
- OS and version:
- How installed: built from source (commit?) / deb / AppImage / other

**Evidence**

Attach what proves it from the user's side: the command you ran and its
output, a screenshot, the relevant `status.json` / reply file, or log lines
from `logs/events.current.jsonl` in your data directory
(Settings → Data in the GUI shows it).

<!-- Redact before pasting: paths under your home directory, crontab
     command lines (bellman scan prints them in full), and anything you
     would not want indexed by a search engine. CI rejects commits
     containing /home/<user> paths; issues deserve the same care. -->
