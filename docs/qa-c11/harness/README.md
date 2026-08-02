# C11 validation harness

Every clock-driven scenario in [`../../VALIDATION.md`](../../VALIDATION.md)
was produced by one of these scripts. They are committed so the evidence can
be re-run rather than taken on trust.

**No script here calls `run-now`.** Each one starts the real desktop app and
waits for the wall clock, which is the point.

## Build first

```sh
cd <repo>
(cd ui && npm ci)
cargo tauri build --no-bundle --ci      # target/release/{bellman,bellman-app}
```

The harness resolves the repo from its own location; override with
`BELLMAN_ROOT=/path/to/repo` if you copy it elsewhere. Isolated session data
goes to `/tmp/c11/<name>`; override with `BELLMAN_QA_RUN_ROOT`.

## What each script does to stay out of your way

`e2e_lib.py` gives every run its own `Xvfb` display, its own
`XDG_DATA_HOME` / `XDG_CONFIG_HOME` / `XDG_RUNTIME_DIR` / `HOME`, and its own
**D-Bus session** (`dbus-run-session`). That last one is not optional: Tauri's
single-instance plugin is keyed on the session bus, so without it a second
Bellman exits immediately against whatever Bellman you already have running.
Your own data directory and your own running app are never touched.

Expect the app to take ~30 s to reach its `setup()` on a private bus —
`xdg-desktop-portal` spends that long failing to reach
`org.freedesktop.secrets`. That is the isolation, not Bellman.

## Host scenarios

Run from anywhere; each is self-contained and prints its evidence path.

| scenario | command | what it proves | evidence |
|---|---|---|---|
| 7 occurrence kinds | `python3 e2e_kinds.py` | once / interval / daily / weekly / monthly / yearly / cron all fire on their own schedule, submitted from outside a running app (also SCH2) | `../kinds_evidence.json` |
| Misfire policy | `python3 e2e_misfire.py` | app stopped before a due time and restarted after it: `coalesce` → one `fired_late`, `skip` → `skipped_misfire` | `../misfire_evidence.json` |
| Reply channel | `python3 e2e_reply.py` | happy path, `no_ack` + late revision, watchdog timeout, heartbeat extension, five rejection cases, superseded stale reply | `../reply_evidence.json` |
| Apps + both transports | `python3 e2e_apps.py` | `testing_apps/lightbulb`, the Perl client, and the same reply over files and over IPC | `../apps_evidence.json` |
| Slot CRUD, live | `python3 e2e_crud.py` | add / modify / delete against a scheduler that is never restarted | `../crud_evidence.json` |
| Pruner + retention | `python3 e2e_prune.py` | rotation at the threshold, gzip archives, 30-day age rule, byte budget | `../prune_evidence.json` |
| Two data directories | `python3 e2e_datadirs.py` | the CLI store and the app store are separate, and nothing fires the CLI one | `../datadirs_evidence.json` |

Each script exits non-zero if its own assertions fail, so
`for s in e2e_*.py; do python3 "$s" || echo "FAILED: $s"; done` is a complete
re-run. Budget ~35 minutes for all seven: they are mostly waiting for real
seconds to pass, which is the whole idea.

## Container scenarios

`dst_inner.py` and `deb_demo_inner.py` run **inside** a disposable container,
never on your machine. Build the image first — Ubuntu with the Tauri runtime
deps plus `Xvfb`, `dbus-x11` and `faketime`:

```sh
docker run --name c11-dstbase ubuntu:24.04 bash -c '
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y -qq libwebkit2gtk-4.1-0 libayatana-appindicator3-1 \
      librsvg2-2 xvfb dbus-x11 faketime tzdata python3 perl'
docker commit c11-dstbase c11-dst
```

**DST, against a real clock** — `libfaketime` parks the process tree a couple
of minutes before a real Europe/Helsinki transition and the container's own
seconds carry it through. Your host clock is never touched.

```sh
docker run --rm -v $PWD/dst_inner.py:/dst_inner.py:ro \
  -v <repo>/target/release/bellman-app:/usr/bin/bellman-app:ro \
  -v <repo>/target/release/bellman:/usr/bin/bellman:ro \
  c11-dst python3 /dst_inner.py gap  1200    # spring forward, ~20 min
  #                              fold 5100   # fall back,     ~85 min
```

> **`FAKETIME_DONT_FAKE_MONOTONIC=1` is mandatory and the script sets it.**
> libfaketime fakes `CLOCK_MONOTONIC` too by default, which stalls the
> scheduler's chunked sleeps outright — nothing fires at all. Control run,
> same container and binary, 60 s interval timer: no faketime fires at
> t=65 s; faketime with faked monotonic never fires; faketime with the
> variable set fires at t=60 s.

**The packaged demo** — install a built `.deb` into that image and run the
demo the package ships, from the path the first-run wizard names:

```sh
docker run --rm -v $PWD/deb_demo_inner.py:/d.py:ro \
  -v <repo>/target/release/bundle/deb/Bellman_*.deb:/new.deb:ro \
  c11-dst bash -c 'apt-get install -y /new.deb >/dev/null && python3 /d.py'
```

Both write `/dst/result.json` or `/deb/result.json`; copy it out with
`docker cp` (the runs above use `--rm`, so use `--name` if you want to).

## The GUI demo

Not a script here — the repo's own harness already covers it:

```sh
scripts/run_gui_qa.sh wiz1
```

Isolated Xvfb + private D-Bus + `tauri-driver`; drives the Bellman window
through its own webview and the demo's tk window with XTEST clicks on that
display only. Needs `webkit2gtk-driver`, `tauri-driver`, `metacity` and a
Python with `selenium` (see `docs/BUILD_PLAN.md` → *to RUN the GUI test
suite*).

## Originality sweep

```sh
BELLMAN_REFERENCE_REPOS=/path/to/clones python3 ../originality_sweep.py \
  > ../originality.json
```

## The README §Install runs

`install/` holds the transcripts behind §4. They are the README's own
commands, in order, with nothing added — each run as the *only* thing in a
clean container:

```sh
docker run --rm -v $PWD/docs/qa-c11/harness/install/readme_install_ubuntu.sh:/v.sh:ro \
  -v <repo>:/srcrepo:ro ubuntu:24.04 bash /v.sh
#   readme_install_fedora.sh  fedora:latest
#   readme_install_arch.sh    archlinux:latest
```

§Install names two identities, and both are covered:

| script | who runs it | what it exercises |
|---|---|---|
| `readme_install_ubuntu.sh` | root, no `sudo` in the image | step 1 without the `sudo` prefix, steps 2+ with no prefix |
| `readme_install_fedora.sh` | root, but the image **has** `sudo` | step 1 keeps the prefix exactly as written |
| `readme_install_arch.sh` | root, no `sudo` | step 1 without the prefix, plus `--noconfirm` because the README says to add it when running unattended |
| `desktop_identity_ubuntu.sh` | an ordinary user with `sudo` | the desktop case: `sudo` on steps 1 and 5, **no** `sudo` on 2–4, and an assertion that the toolchain landed in the user's `$HOME` and not in root's |

The last one needs the container to be made to resemble a desktop first —
install `sudo`, create a user, put them in the sudoers file. That scaffolding
is above a marked line in the script and is **not** part of §Install: a real
desktop arrives with all three already true. It is written out rather than
hidden so it cannot be mistaken for a step the README asks of anyone.

The one substitution in all four: step 4's `git clone` reads `/srcrepo`, a
read-only bind mount of the checkout, instead of the public URL — the branch
under test is not on public `main` yet. Each script says so at the top.
