# IK1 — Timer→app→Bellman round trip, with a lightbulb reference app

Repo: `~/bellman`

## What already exists — do NOT rebuild it

Read these before writing anything; most of the outbound half is done:

- `crates/bellman-core/src/actions/write_slot.rs` — `write_output_slot()` already writes a
  fire notification (`timer_id`, `timer_name`, `run_id`, `scheduled_for`, `fired_at`, `kind`)
  via atomic temp+rename.
- `crates/bellman-core/src/slots/` — the `bellman-slot/1` protocol: request envelope,
  response envelope in `done/slot-NNNN.json`, `request_id` idempotency, claim-by-rename,
  quarantine to `bad/` with a `.err.json` sidecar, replenish ≥5.
- `docs/INTEGRATION.md` — the protocol plus copy-paste clients in Python, bash, PowerShell
  and Node.
- The JSONL event log.

**Extend this protocol. Do not invent a second one.** Same schema family, same atomic
temp+rename discipline, same quarantine behaviour.

## The gap this card closes

Today Bellman can **tell** an application that a timer fired. The application cannot **tell
Bellman what happened next**. There is no acknowledgement, no completion, no result — so the
run history can say "fired" and never "and the thing actually worked".

Add the reply direction, and with it a real lifecycle:

```
fired         Bellman wrote the notification
acknowledged  the app picked it up            (app writes)
completed     the app finished, with a result (app writes)
failed        the app finished badly, with a reason (app writes)
no_reply      nobody answered inside the window   (Bellman decides)
```

Every transition lands in the event log, correlated by `run_id`, so the run history answers
*"did the thing I scheduled actually happen"* rather than *"did Bellman press the button"*.

## Rules

- **Correlate by `run_id`.** A reply that names an unknown or already-closed run is rejected
  and quarantined, not applied.
- **Idempotent.** The same reply arriving twice changes nothing the second time.
- **Timeouts are a real outcome.** A configurable reply window, per timer. If it lapses,
  record `no_reply` and move on — never hang, never wait forever.
- **A reply is DATA, never a command.** Bellman parses it, validates it against the schema,
  logs it. It must never cause Bellman to launch, execute, schedule or modify anything on the
  strength of what an app wrote. A malformed or hostile reply can at worst produce a bad log
  line.
- **Bounded.** Cap the reply payload size and the free-text result field. Quarantine anything
  over the cap rather than reading it.
- Garbage in → `bad/` with a sidecar, exactly like the existing path.

## The lightbulb reference app

A deliberately tiny app that exists to prove the loop end to end, and to be the example a
third party copies.

1. A timer fires. Bellman writes the notification.
2. The lightbulb app sees it and writes `acknowledged`.
3. The bulb turns **on** — visibly, on screen — and stays on for **15 seconds**.
4. The app writes `completed`, including the measured on-duration.
5. Bellman validates the reply, closes the run, and the run history shows the whole chain.

Keep it small and readable — it is documentation that happens to compile. It must be
genuinely observable: a human watching the screen can see the bulb light and go out.

## Documentation

Extend `docs/INTEGRATION.md` with a **"connect your own application"** section covering the
new direction: how to watch for a fire notification, how to acknowledge, how to report
completion or failure, what the timeout means, and what happens to a malformed reply. Keep
the existing copy-paste style — short clients a reader can lift whole.

The lightbulb app is the worked example the section points at.

## Exit gate

- Full round trip observed live: fire → acknowledged → bulb on 15s → completed → validated
  and closed, with every transition in the event log under one `run_id`.
- An app that never replies produces `no_reply` after the window, and the run closes cleanly.
- A duplicate reply is a no-op.
- A reply naming an unknown `run_id` is quarantined, not applied.
- An oversized or malformed reply is quarantined with a sidecar and changes no state.
- A test proves a reply cannot cause execution of anything — it is logged and nothing else.
- The `INTEGRATION.md` section is followed by someone (or something) that has not seen the
  code, and their client works.
