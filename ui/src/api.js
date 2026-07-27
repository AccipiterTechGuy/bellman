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
