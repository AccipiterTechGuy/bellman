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
  // Tauri 2 exposes `__TAURI_INTERNALS__.invoke` for direct IPC.
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

// Event subscription (Tauri 2 global event API).
// Tauri 2 exposes an `event` plugin: `__TAURI_INTERNALS__.invoke('plugin:event|listen', { event, target, handler })`
// For C7 we do not need a real event bus — the All-timers page polls
// every 5 s and re-fetches on any user action, which is more than fast
// enough for a single-user tray app. The stub here is kept only as the
// import surface for the future event-driven paths.
export function listen(_event, _handler) {
  if (!hasTauri) return () => {};
  return () => {};
}
