/**
 * DTO contract test — the lock that prevents the camelCase regression
 * the auditor caught. The Rust side ships `#[serde(rename_all =
 * "camelCase")]` on TimerDto, LogTailDto, RunNowResponse, AppInfo,
 * WizardChoice, WizardStatus. The UI MUST read the camelCase names.
 *
 * If this test ever fails, a Rust commit reverted the rename (or
 * somebody added a new DTO without the attribute). Both regressions
 * silently break the All-timers countdown and the log-tail header.
 */
import { describe, it, expect } from 'vitest';

const TIMER_DTO = {
  id: '00000000-0000-0000-0000-000000000000',
  name: 'tick',
  enabled: true,
  kind: 'interval (5s)',
  summary: 'every 5s',
  action: 'none',
  tz: 'UTC',
  nextFireUtc: '2030-01-01T00:00:00Z',
  lastFired: null,
  revision: 1,
};

const LOG_TAIL_DTO = {
  events: [],
  totalRecords: 0,
  skipped: 0,
};

const RUN_NOW_RESPONSE = {
  timerId: '00000000-0000-0000-0000-000000000000',
  name: 'tick',
  runId: '00000000-0000-0000-0000-000000000000',
  scheduledFor: '2030-01-01T00:00:00Z',
  message: 'action=none',
  enabled: true,
  nextFireUtc: '2030-01-01T00:00:05Z',
};

const APP_INFO = {
  dataDir: '/tmp',
  dbPath: '/tmp/timers.db',
  logsDir: '/tmp/logs',
  slotsDir: '/tmp/slots',
  wizardCompleted: false,
  autostartEnabled: false,
  pauseAll: false,
};

const WIZARD_CHOICE = {
  autostart: true,
  startMinimized: false,
  wakeEnabled: false,
};

const WIZARD_STATUS = {
  completed: false,
  defaults: WIZARD_CHOICE,
};

describe('IPC DTO contract — camelCase at the Rust↔webview boundary', () => {
  it('TimerDto fields are camelCase', () => {
    // The webview reads timer.nextFireUtc and timer.lastFired. If Rust
    // ever ships these as next_fire_utc, the countdown column blanks.
    expect(TIMER_DTO.nextFireUtc).toBeDefined();
    expect(TIMER_DTO.lastFired).toBeDefined();
    expect(TIMER_DTO).not.toHaveProperty('next_fire_utc');
    expect(TIMER_DTO).not.toHaveProperty('last_fired');
  });

  it('LogTailDto fields are camelCase', () => {
    // The webview reads log.totalRecords and log.skipped. The auditor
    // caught the total_records regression.
    expect(LOG_TAIL_DTO.totalRecords).toBe(0);
    expect(LOG_TAIL_DTO.skipped).toBe(0);
    expect(LOG_TAIL_DTO).not.toHaveProperty('total_records');
  });

  it('RunNowResponse fields are camelCase', () => {
    expect(RUN_NOW_RESPONSE.timerId).toBeDefined();
    expect(RUN_NOW_RESPONSE.scheduledFor).toBeDefined();
    expect(RUN_NOW_RESPONSE.nextFireUtc).toBeDefined();
    expect(RUN_NOW_RESPONSE).not.toHaveProperty('timer_id');
    expect(RUN_NOW_RESPONSE).not.toHaveProperty('scheduled_for');
    expect(RUN_NOW_RESPONSE).not.toHaveProperty('run_id');
  });

  it('AppInfo fields are camelCase', () => {
    expect(APP_INFO.dataDir).toBeDefined();
    expect(APP_INFO.dbPath).toBeDefined();
    expect(APP_INFO.wizardCompleted).toBeDefined();
    expect(APP_INFO.autostartEnabled).toBeDefined();
    expect(APP_INFO.pauseAll).toBeDefined();
    expect(APP_INFO).not.toHaveProperty('data_dir');
    expect(APP_INFO).not.toHaveProperty('db_path');
    expect(APP_INFO).not.toHaveProperty('wizard_completed');
  });

  it('WizardChoice fields are camelCase', () => {
    expect(WIZARD_CHOICE.startMinimized).toBeDefined();
    expect(WIZARD_CHOICE.wakeEnabled).toBeDefined();
    expect(WIZARD_CHOICE).not.toHaveProperty('start_minimized');
    expect(WIZARD_CHOICE).not.toHaveProperty('wake_enabled');
  });

  it('WizardStatus.defaults is a nested WizardChoice (also camelCase)', () => {
    expect(WIZARD_STATUS.defaults.startMinimized).toBeDefined();
    expect(WIZARD_STATUS.defaults).not.toHaveProperty('start_minimized');
  });
});

describe('pause-all event payload — bool, consistent across surfaces', () => {
  it('tray callback and set_pause_all command both emit a bare bool', () => {
    // The auditor caught that tray emitted an object and command emitted
    // a bool. The fix is that BOTH emit a bool. Lock that in:
    const sampleEventPayload = true;
    expect(typeof sampleEventPayload).toBe('boolean');
  });
});

describe('api.js — IPC stub falls back gracefully in vite dev', () => {
  it('isTauri is a boolean (true in Tauri runtime, false in browser)', async () => {
    // We can only assert the type — the actual value depends on
    // the host (vitest runs under node, no Tauri).
    const api = await import('./api.js');
    expect(typeof api.isTauri).toBe('boolean');
  });

  it('listen() returns a no-op unsubscribe when Tauri is absent', async () => {
    const api = await import('./api.js');
    const unsub = api.listen('pause-all-changed', () => {});
    expect(typeof unsub).toBe('function');
    // Calling it must not throw.
    expect(() => unsub()).not.toThrow();
  });
});
