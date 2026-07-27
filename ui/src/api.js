// Thin wrapper around the Tauri IPC. Exposes a `safe` fallback for
// `vite dev` (the page works without the backend — it just shows an empty
// state with a "Tauri not available" hint).
//
// All IPC paths read `window.__TAURI_INTERNALS__` REACTIVELY (at call
// time, not at module load) so tests that swap the runtime mid-flight
// behave the same as a real Tauri window that boots a frame after the
// bundle loaded.

function _hasTauri() {
  // Tauri injects on `window`. happy-dom provides `window` in tests,
  // and vite dev provides it in the browser. We tolerate `window`
  // being absent in non-browser environments (node, future SSR) by
  // reading the global directly as a fallback.
  if (typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window) {
    return true;
  }
  if (typeof globalThis !== 'undefined' && '__TAURI_INTERNALS__' in globalThis) {
    return true;
  }
  return false;
}
function _internals() {
  if (typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window) {
    return window.__TAURI_INTERNALS__;
  }
  return globalThis.__TAURI_INTERNALS__;
}

// `isTauri` is exported for the UI hint. It's a getter so a runtime
// injected AFTER api.js was loaded still shows up correctly.
export function isTauri() {
  return _hasTauri();
}

async function invoke(cmd, args) {
  if (!_hasTauri()) {
    // Headless-fixtures override: when a harness sets
    // `globalThis.__bellman_fixtures__ = { [cmd]: (args) => any }`, the
    // production bundle still renders realistic content for screenshot
    // captures (notes: not the default; only activates when an explicit
    // `__bellman_fixtures__` map exists). Used by the QA_P4 harness.
    if (
      typeof globalThis !== 'undefined'
      && globalThis.__bellman_fixtures__
      && Object.prototype.hasOwnProperty.call(globalThis.__bellman_fixtures__, cmd)
    ) {
      return await globalThis.__bellman_fixtures__[cmd](args);
    }
    throw new Error(`Tauri not available (cmd: ${cmd})`);
  }
  return await window.__TAURI_INTERNALS__.invoke(cmd, args);
}

/* -------------------------------------------------------------------- *
 * listen() — works whether the Tauri JS global is injected or not.
 * -------------------------------------------------------------------- *
 * Path A (preferred): `window.__TAURI__.event.listen` exists when the
 *   app is configured with `app.withGlobalTauri: true`. We delegate to it.
 *
 * Path B (shim): even without the global, Tauri's IPC reuses the
 *   `plugin:event|listen` command. The plugin expects a callback ID
 *   number that the Rust side can invoke via
 *   `__TAURI_INTERNALS__.runCallback(cb_id, payload)`. We allocate such
 *   IDs from a local Map and register with the runtime's `transformCallback`
 *   shim — the same path `@tauri-apps/api`'s `event.listen` takes.
 *
 * The runtime shim installed by the production `tauri` IIFE exposes
 * `runCallback`, `unregisterCallback`, and `transformCallback` on
 * `__TAURI_INTERNALS__`. For tests / non-Tauri browsers we install a
 * tiny in-process shim that uses a `Map` to back the same contract.
 *
 * Returns an UNSUBSCRIBE function. Calling it removes the listener
 * (`plugin:event|unlisten`) and prevents further delivery — even if the
 * caller invokes it BEFORE the `plugin:event|listen` promise resolves.
 */

const _listenerCbs = new Map(); // cb_id -> { fn, once }
let _nextCbId = 1;

function _transformCallback(callback, _once = false) {
  const id = _nextCbId++;
  _listenerCbs.set(id, { fn: callback });
  return id;
}

function _unregisterCallback(id) {
  _listenerCbs.delete(id);
}

function _runCallback(id, payload) {
  const slot = _listenerCbs.get(id);
  if (!slot) return false;
  try {
    slot.fn(payload);
  } catch (err) {
    // Don't let a buggy handler drop the event loop. Surface to console.
    console.error('[bellman] event handler threw:', err);
  }
  return true;
}

/** Make sure `__TAURI_INTERNALS__` exposes our shim or the runtime's. */
function _ensureRuntimeCallbacks() {
  if (!_hasTauri()) return;
  const internals = window.__TAURI_INTERNALS__;
  if (typeof internals.transformCallback !== 'function') {
    internals.transformCallback = (cb) => _transformCallback(cb, false);
  }
  if (typeof internals.unregisterCallback !== 'function') {
    internals.unregisterCallback = _unregisterCallback;
  }
  if (typeof internals.runCallback !== 'function') {
    internals.runCallback = _runCallback;
  }
}

export async function listen(event, handler) {
  if (!_hasTauri()) {
    // No-op in the browser so vite dev still works without a backend.
    return () => {};
  }
  _ensureRuntimeCallbacks();

  // Path A — preferred when the global IIFE was injected
  // (app.withGlobalTauri: true).
  const globalListen = typeof window.__TAURI__ !== 'undefined'
    ? window.__TAURI__?.event?.listen
    : undefined;
  if (typeof globalListen === 'function') {
    return await globalListen(event, handler);
  }

  // Path B — direct plugin:event|listen call. See the comment block at
  // the top of the file; this is the documented fallback used by
  // @tauri-apps/api when the global IIFE is absent.
  const cbId = window.__TAURI_INTERNALS__.transformCallback((delivery) => {
    handler({ event: delivery?.event ?? event, payload: delivery?.payload });
  });

  // Tell the runtime to invoke our callback on every event of this name.
  const eventId = await window.__TAURI_INTERNALS__.invoke('plugin:event|listen', {
    event,
    target: { kind: 'Any' },
    handler: cbId,
  });

  // Unsubscribe closure — captures `cbId` and `eventId`.
  return async () => {
    window.__TAURI_INTERNALS__.unregisterCallback(cbId);
    try {
      await window.__TAURI_INTERNALS__.invoke('plugin:event|unlisten', {
        event,
        eventId,
      });
    } catch {
      // ignore — runtime may already be torn down.
    }
  };
}

export async function listTimers() {
  return await invoke('list_timers');
}
export async function getTimer(id) {
  return await invoke('get_timer', { id });
}
export async function setEnabled(id, enabled, expectedRevision) {
  return await invoke('set_enabled', { id, enabled, expectedRevision });
}
export async function runNow(id) {
  return await invoke('run_now', { id });
}
export async function listLogTail(timerId, limit) {
  return await invoke('list_log_tail', { timerId, limit });
}
export async function createTimer(input) {
  return await invoke('create_timer', { input });
}
export async function updateTimer(id, expectedRevision, patch) {
  return await invoke('update_timer', { id, expectedRevision, patch });
}
export async function deleteTimer(id) {
  return await invoke('delete_timer', { id });
}
export async function previewFires(input, n = 5) {
  return await invoke('preview_fires', { input, n });
}
export async function getPauseAll() {
  return await invoke('get_pause_all');
}
export async function setPauseAll(paused) {
  return await invoke('set_pause_all', { paused });
}
export async function wizardStatus() {
  return await invoke('wizard_status');
}
export async function wizardSetChoice(choice) {
  return await invoke('wizard_set_choice', { choice });
}
export async function wizardReRun() {
  return await invoke('wizard_re_run');
}
export async function appInfo() {
  return await invoke('app_info');
}

// ── UI-side helpers (calendar / dialog math). Keep these pure so unit tests
// ── can import them and run without a webview. See api.test.js for the
// ── round-trip coverage.
export const WEEKDAY_LABELS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];

/** `mon`→1, `tue`→2, … `sun`→7. Inverse of the TimerDto `Weekdays` bitmask. */
export const WEEKDAY_FROM_KEY = {
  mon: 1, tue: 2, wed: 3, thu: 4, fri: 5, sat: 6, sun: 7,
};

/** Pull the kind discriminator out of a TimerDto's structured `occurrence`. */
export function kindFromOccurrence(occ) {
  return (occ && typeof occ.occ === 'string') ? occ.occ : '';
}

/** Parse a TimerDto's structured weekly `days` bitmask → sorted ISO DOW list. */
export function weeklyDaysFromOccurrence(occ) {
  const days = occ && occ.days ? occ.days : {};
  const out = [];
  for (const k of Object.keys(days)) {
    if (days[k]) {
      const dow = WEEKDAY_FROM_KEY[k.toLowerCase()];
      if (dow) out.push(dow);
    }
  }
  return out.sort();
}

/** ISO weekday (Mon=1 .. Sun=7) for a JS Date in the local zone. */
export function jsIsoWeekday(date) {
  const d = date.getDay();
  // JS Sunday=0..Saturday=6 → ISO Mon=1..Sun=7
  return d === 0 ? 7 : d;
}

/** ISO calendar week (Mon-based) for a JS Date — used by the Week page. */
export function isoWeekStart(date) {
  const d = new Date(date.getFullYear(), date.getMonth(), date.getDate());
  const dow = jsIsoWeekday(d);
  d.setDate(d.getDate() - (dow - 1));
  d.setHours(0, 0, 0, 0);
  return d;
}

/** Add `days` calendar days to a Date, returning a new Date. */
export function addDays(date, days) {
  const d = new Date(date.getFullYear(), date.getMonth(), date.getDate());
  d.setDate(d.getDate() + days);
  return d;
}

/** Return the first day of the month for a given year/month. */
export function monthStart(year, month /* 0-indexed */) {
  return new Date(year, month, 1);
}

/** Number of days in a month (handles leap year). */
export function daysInMonth(year, month /* 0-indexed */) {
  return new Date(year, month + 1, 0).getDate();
}

/** Build the 6×7 calendar grid for `year`/`month` (0-indexed), ISO-DOW rows. */
export function monthGrid(year, month) {
  const first = monthStart(year, month);
  const offset = jsIsoWeekday(first) - 1; // 0..6
  const start = addDays(first, -offset);
  const out = [];
  for (let i = 0; i < 42; i++) {
    out.push(addDays(start, i));
  }
  return out;
}

/** Format a YYYY-MM-DD string in local tz. */
export function isoDate(d) {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${day}`;
}

/** Format HH:MM:SS in local tz. */
export function isoClock(d) {
  return [d.getHours(), d.getMinutes(), d.getSeconds()]
    .map((n) => String(n).padStart(2, '0'))
    .join(':');
}

/** Convert a UTC ISO string to a JS Date (UTC parsed). */
export function parseUtc(iso) {
  if (!iso) return null;
  return new Date(iso);
}

/** Format a UTC ISO as YYYY-MM-DD in the schedule's tz offset. */
export function formatLocalDate(iso, tzOffsetMinutes = null) {
  const d = parseUtc(iso);
  if (!d) return '';
  if (tzOffsetMinutes == null) {
    return isoDate(d);
  }
  // Shift the UTC instant into the desired tz for display.
  const shifted = new Date(d.getTime() + tzOffsetMinutes * 60_000);
  return isoDate(shifted);
}

/** Format a UTC ISO as HH:MM:SS in the schedule's tz offset. */
export function formatLocalTime(iso, tzOffsetMinutes = null) {
  const d = parseUtc(iso);
  if (!d) return '';
  if (tzOffsetMinutes == null) {
    return isoClock(d);
  }
  const shifted = new Date(d.getTime() + tzOffsetMinutes * 60_000);
  return isoClock(shifted);
}

/** Parse "HH:MM" or "HH:MM:SS" into seconds-since-midnight, or null on fail. */
export function clockToSeconds(s) {
  if (!s) return null;
  const m = /^(\d{1,2}):(\d{2})(?::(\d{2}))?$/.exec(s.trim());
  if (!m) return null;
  const h = +m[1], mm = +m[2], ss = +(m[3] ?? '0');
  if (h > 23 || mm > 59 || ss > 59) return null;
  return h * 3600 + mm * 60 + ss;
}
