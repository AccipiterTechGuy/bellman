# Security Policy

## Supported versions

Bellman has **no tagged release** — it is under active construction and every
build is unsigned. Only the latest `main` branch receives fixes. Nothing here
is yet covered by a support commitment.

## Reporting a vulnerability

**Please do not open a public issue for a security report.** Use GitHub's
private vulnerability reporting ("Security" tab → "Report a vulnerability")
on this repository. If that is unavailable to you, open a minimal public
issue that says only that you have a security report to share, and a
maintainer will make contact.

Please include: the affected commit or build, the platform, a description of
the impact, and reproduction steps or a proof of concept. You can expect an
acknowledgement within a few days; this is a spare-time project, not a
vendor, so there is no SLA beyond honest effort.

## Scope and threat model

Bellman's integration surface is deliberately **unauthenticated**: the JSON
slot channel (`slots/`), reply files, and the local IPC socket all trust the
OS boundary — *any process running as your user may ask Bellman to schedule
things*. That is the documented design, not a vulnerability. Reports about
it will be closed as intended behaviour.

In scope and genuinely wanted:

- A way for one **user's** Bellman to be driven by **another** user, or by a
  process sandboxed away from the session, past the file permissions and
  socket ACLs (0700 directories, 0600 socket, restricted named pipe).
- Bellman **executing** something because of bytes an app wrote into a file
  Bellman owns (`timer.json`, `status.json`, the event log) — a reply is
  data, never a command, by design.
- Path traversal or symlink escapes in the slot/reply/fire file handling
  (writes outside the data directory, clobbering unrelated files).
- Injection through a timer's launch action beyond the documented
  arg-array-no-shell contract.
- The GUI rendering app-supplied text (`progress`, `result`, timer names) as
  markup or code.
- Leaks of personal data into this public repository (paths, crontab lines,
  schedules) via tooling, CI, or committed evidence.

## Notes for integrators

- `bellman scan` prints **full command lines** of every schedule on the
  machine — including paths and possibly tokens embedded in crontabs. Do not
  paste raw scan output into public issues; redact first.
- Launch actions run with the privileges of the Bellman process. A timer
  created through the slot channel can name any command — schedule nothing
  you would not run yourself.
