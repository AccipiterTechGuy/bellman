/**
 * SHIP1-D — the active data directory must be discoverable from the GUI
 * without reading docs: Settings → Data renders what `app_info` reports.
 */
import { afterEach, describe, expect, it } from 'vitest';
import { mount, unmount } from 'svelte';
import SettingsPage from './SettingsPage.svelte';

function installTauriMock(handlers = {}) {
  const invokes = [];
  globalThis.__TAURI_INTERNALS__ = {
    invoke: async (cmd, args) => {
      invokes.push({ cmd, args });
      if (Object.prototype.hasOwnProperty.call(handlers, cmd)) {
        return await handlers[cmd](args);
      }
      throw new Error(`unexpected invoke: ${cmd}`);
    },
  };
  return invokes;
}

const flush = () => new Promise((r) => setTimeout(r, 20));

const GUI_DATA_DIR = '/home/u/.local/share/io.bellman.desktop';

function handlers() {
  return {
    wake_status: async () => ({
      statusLine: 'Wake from sleep: OFF — turned off in Settings',
      enabled: false,
      masterEnabled: false,
      platformEnabled: true,
      platform: 'linux',
      fixHint: null,
      fixAction: null,
      udevSnippet: null,
      powercfgCommand: null,
      loginItemsUrl: null,
    }),
    app_info: async () => ({
      dataDir: GUI_DATA_DIR,
      dbPath: `${GUI_DATA_DIR}/timers.db`,
      logsDir: `${GUI_DATA_DIR}/logs`,
      slotsDir: `${GUI_DATA_DIR}/slots`,
      wizardCompleted: true,
      autostartEnabled: true,
      pauseAll: false,
      wakeEnabled: false,
      wakeStatusLine: '',
      maxConcurrentActions: 16,
      defaultMisfirePolicy: 'coalesce',
      defaultMisfireGraceSecs: 3600,
    }),
    get_pause_all: async () => false,
    get_misfire_defaults: async () => ({ policy: 'coalesce', graceSecs: 3600 }),
    demo_info: async () => ({ optIn: false, available: false }),
  };
}

let component = null;
afterEach(async () => {
  if (component) {
    await unmount(component);
    component = null;
  }
  document.body.innerHTML = '';
  delete globalThis.__TAURI_INTERNALS__;
});

describe('Settings → Data (SHIP1-D)', () => {
  it('renders the live data directory (and db/logs/slots) from app_info', async () => {
    installTauriMock(handlers());
    const target = document.createElement('div');
    document.body.appendChild(target);
    component = mount(SettingsPage, { target, props: {} });
    await flush();

    const section = target.querySelector('[data-testid="settings-data"]');
    expect(section, 'Settings must have a Data section').toBeTruthy();
    const dir = target.querySelector('[data-testid="settings-data-dir"]');
    expect(dir, 'the active data directory must be shown').toBeTruthy();
    expect(dir.textContent).toBe(GUI_DATA_DIR);
    expect(section.textContent).toContain(`${GUI_DATA_DIR}/timers.db`);
    expect(section.textContent).toContain(`${GUI_DATA_DIR}/logs`);
    expect(section.textContent).toContain(`${GUI_DATA_DIR}/slots`);
    // …and it must point out that the CLI uses a different default, so a
    // CLI-following user does not integrate against the wrong store.
    expect(section.textContent).toContain('~/.bellman/');
  });
});
