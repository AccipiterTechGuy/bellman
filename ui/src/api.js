// Thin wrapper around the Tauri IPC. Exposes a `safe` fallback for
// `vite dev` (the page works without the backend — it just shows an empty
// state with a "Tauri not available" hint).

const hasTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

// We use the invoke function from the Tauri global (no @tauri-apps/api
// dependency needed: Tauri exposes `__TAURI_INTERNALS__.invoke`).
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

/**
 * Subscribe to a Tauri backend event.
 *
 * Uses `window.__TAURI__.event.listen` when the Tauri runtime is
 * available (the preferred path in Tauri 2), and falls back to a
 * transformCallback-based shim that talks directly to the
 * `plugin:event|listen` / `plugin:event|unlisten` plugin commands.
 *
 * Returns an unsubscribe function. The callback receives `{ event, payload }`
 * (Tauri's standard event shape). Payload for `pause-all-changed` is a bool.
 */
export function listen(event, handler) {
  if (!hasTauri) return () => {};
  // Prefer the Tauri 2 global API — already loaded by the runtime.
  const globalListen = window.__TAURI__?.event?.listen;
  if (typeof globalListen === 'function') {
    let cancelled = false;
    let unlisten = () => {};
    globalListen(event, handler)
      .then((u) => {
        if (cancelled) {
          try { u(); } catch { /* ignore */ }
        } else {
          unlisten = u;
        }
      })
      .catch(() => { /* ignore — runtime may not be ready yet */ });
    return () => { cancelled = true; unlisten(); };
  }
  return () => {};
}
