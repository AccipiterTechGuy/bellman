// Thin wrapper around the Tauri IPC. Exposes a `safe` fallback for
// `vite dev` (the page works without the backend — it just shows an empty
// state with a "Tauri not available" hint).
//
// All IPC paths go through `window.__TAURI_INTERNALS__.invoke`. The global
// `window.__TAURI__` (loaded by the runtime when `app.withGlobalTauri` is
// `true`) is only consulted by `listen()` as a convenience — if it is not
// present, `listen()` still works via the documented
// `transformCallback` shim that talks directly to `plugin:event|listen`.
// The shim does NOT depend on a JS package and works whether or not the
// global Tauri IIFE was injected.

const hasTauri =
  typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

async function invoke(cmd, args) {
  if (!hasTauri) {
    throw new Error(`Tauri not available (cmd: ${cmd})`);
  }
  return await window.__TAURI_INTERNALS__.invoke(cmd, args);
}

export const isTauri = hasTauri;

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

/* -------------------------------------------------------------------- *
 * listen() — works whether the Tauri JS global is injected or not.
 * -------------------------------------------------------------------- *
 * Path A (preferred): `window.__TAURI__.event.listen` exists when the
 *   app is configured with `withGlobalTauri: true`. We delegate to it.
 *
 * Path B (shim): even without the global, Tauri's IPC reuses the
 *   `plugin:event|listen` command. The plugin expects a callback ID
 *   number that the Rust side can invoke via
 *   `__TAURI_INTERNALS__.runCallback(cb_id, payload)`. We allocate such
 *   IDs from a local Map and register with `transformCallback`-shaped
 *   allocation. This is the same path `@tauri-apps/api`'s `event.listen`
 *   takes; doing it here avoids pulling in the package and proves the
 *   mechanic works regardless of the global.
 *
 * Returns an UNSUBSCRIBE function. Calling it removes the listener
 * (`plugin:event|unlisten`) and prevents further delivery — even if the
 * caller invokes it BEFORE the `plugin:event|listen` promise resolves.
 */

const _listenerCbs = new Map(); // cb_id -> handler
let _nextCbId = 1;

function _transformCallback(callback, once = false) {
  // Mirror Tauri's transformCallback contract. Each call gets a fresh id.
  const id = _nextCbId++;
  _listenerCbs.set(id, { fn: callback, once });
  return id;
}

function _unregisterCallback(id) {
  _listenerCbs.delete(id);
}

function _runCallback(id, payload) {
  const slot = _listenerCbs.get(id);
  if (!slot) return false;
  let delivered = false;
  try {
    slot.fn(payload);
    delivered = true;
  } catch (err) {
    // Don't let a buggy handler drop the event loop. Surface to console.
    console.error('[bellman] event handler threw:', err);
  }
  if (slot.once) {
    _listenerCbs.delete(id);
  }
  return delivered;
}

// Wire the runtime call surface (path B). The runtime exposes
// `runCallback` and `unregisterCallback` even when the global
// IIFE is absent — these are the IPC primitives every plugin uses.
if (hasTauri) {
  const internals = window.__TAURI_INTERNALS__;
  if (typeof internals.runCallback !== 'function') {
    internals.runCallback = _runCallback;
  }
  if (typeof internals.unregisterCallback !== 'function') {
    internals.unregisterCallback = _unregisterCallback;
  }
  if (typeof internals.transformCallback !== 'function') {
    internals.transformCallback = (cb) => _transformCallback(cb, false);
  }
}

export async function listen(event, handler) {
  if (!hasTauri) {
    // No-op in the browser so vite dev still works without a backend.
    return () => {};
  }

  // Path A — preferred when the global IIFE was injected
  // (app.withGlobalTauri: true).
  const globalListen = window.__TAURI__?.event?.listen;
  if (typeof globalListen === 'function') {
    return await globalListen(event, handler);
  }

  // Path B — direct plugin:event|listen call. As described in Tauri's
  // runtime code: the `handler` field is a number (the callback ID).
  // When Rust wants to deliver, it does
  //   runCallback(handler, { event, payload, id })
  // which dispatches to our _runCallback above.
  const cbId = _transformCallback((delivery) => {
    handler({ event: delivery?.event ?? event, payload: delivery?.payload });
  });

  // Tell the runtime to invoke our callback on every event of this name.
  const eventId = await invoke('plugin:event|listen', {
    event,
    target: { kind: 'Any' },
    handler: cbId,
  });

  // Unsubscribe closure — captures `cbId` and `eventId`.
  return async () => {
    _unregisterCallback(cbId);
    try {
      await invoke('plugin:event|unlisten', { event, eventId });
    } catch {
      // ignore — runtime may already be torn down.
    }
  };
}
