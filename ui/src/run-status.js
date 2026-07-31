// IK5 — live run-state rendering helpers.
//
// Pure functions over the `list_run_states` DTO (the database mirror of
// `status.json`). All display arithmetic lives here so the components stay
// dumb and the rules stay testable:
//
// - `overdue` is a LABEL, computed at render time: `now − fired_at >
//   expected_secs` — 1× the app's own estimate, NEVER the watchdog's
//   × factor. No timer, no wakeup, no event: crossing it writes nothing.
// - The label anchors on `fired_at` (Bellman's own stamp, present on every
//   run). A heartbeat does NOT move it; only a newly accepted
//   `expected_secs` does — and the backend has already folded the latest
//   value into the DTO, so this module just reads it.
// - Absent optional fields render as NOTHING. Absence is not a state.

const NON_TERMINAL = new Set(['fired', 'fired_late', 'acknowledged', 'running']);

/** Non-terminal app-lifecycle states (the run is still open). */
export function isNonTerminal(state) {
  return NON_TERMINAL.has(state);
}

/** Whole seconds since the run fired — the only clock the label has. */
export function elapsedSecs(run, nowMs) {
  const fired = Date.parse(run?.firedAt ?? '');
  if (Number.isNaN(fired)) return 0;
  return Math.max(0, Math.floor((nowMs - fired) / 1000));
}

/**
 * The overdue label: non-terminal AND past 1× the app's own expected_secs,
 * measured from `fired_at`. Render-time only — no backend event exists.
 */
export function isOverdue(run, nowMs) {
  if (!run || !isNonTerminal(run.state)) return false;
  const expected = run.expectedSecs;
  if (typeof expected !== 'number' || expected <= 0) return false;
  return elapsedSecs(run, nowMs) > expected;
}

/** Compact duration: "7s", "74m", "3d 4h" (minutes below 24h, per card). */
export function formatElapsed(totalSecs) {
  const s = Math.max(0, Math.floor(totalSecs));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 1440) return `${m}m`;
  const d = Math.floor(m / 1440);
  const rh = Math.floor((m % 1440) / 60);
  return rh ? `${d}d ${rh}h` : `${d}d`;
}

/**
 * The one-line live state for an integration-owned timer's row (and for
 * pinned live entries in Run history). Shape:
 *
 *   ● running · 7s · bulb on, 7s elapsed
 *   ● running · 74m · overdue (expected ~10m)
 *   ⚠ failed · timed out
 *
 * Absent optional fields contribute NOTHING — no "never", no "—", no
 * greyed placeholder. Returns `{ dot, tone, text }`, or null when there is
 * no run to show. tone: live | ok | err | warn | muted.
 */
export function runRowDisplay(run, nowMs) {
  if (!run || !run.state) return null;
  const state = run.state;

  if (isNonTerminal(state)) {
    const word = state === 'fired_late' ? 'fired' : state;
    const parts = [word, formatElapsed(elapsedSecs(run, nowMs))];
    if (run.progress) parts.push(run.progress);
    const overdue = isOverdue(run, nowMs);
    if (overdue) parts.push(`overdue (expected ~${formatElapsed(run.expectedSecs)})`);
    return { dot: '●', tone: overdue ? 'warn' : 'live', text: parts.join(' · ') };
  }

  switch (state) {
    case 'completed':
      return { dot: '✓', tone: 'ok', text: 'completed' };
    case 'failed': {
      const kind =
        run.failureKind === 'timed_out'
          ? 'timed out'
          : run.failureKind === 'reported'
            ? 'reported'
            : '';
      return { dot: '⚠', tone: 'err', text: kind ? `failed · ${kind}` : 'failed' };
    }
    case 'no_ack':
      return { dot: '⚠', tone: 'err', text: 'no ack' };
    case 'cancelled':
      return { dot: '■', tone: 'muted', text: 'cancelled' };
    default:
      return { dot: '■', tone: 'muted', text: state };
  }
}

/** Distinct label for a terminal event-log kind (Run history entries). */
export function eventKindDisplay(event) {
  const k = event?.kind ?? '';
  if (k === 'failed') {
    const fk = event?.detail?.failure_kind;
    if (fk === 'timed_out') return { label: 'failed · timed out', cls: 'err' };
    if (fk === 'reported') return { label: 'failed · reported', cls: 'err' };
    return { label: 'failed', cls: 'err' };
  }
  if (k === 'completed') return { label: 'completed', cls: 'ok' };
  if (k === 'no_ack') return { label: 'no ack', cls: 'err' };
  if (k === 'superseded') return { label: 'superseded', cls: 'warn' };
  if (k === 'cancelled') return { label: 'cancelled', cls: 'warn' };
  if (k === 'acknowledged') return { label: 'acknowledged', cls: '' };
  if (k === 'running') return { label: 'running', cls: '' };
  if (k === 'reply_rejected') return { label: 'reply rejected', cls: 'warn' };
  return null; // caller falls back to its own mapping
}
