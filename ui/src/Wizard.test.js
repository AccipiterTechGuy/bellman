/**
 * WIZ1 component tests — the real Wizard.svelte and DemoPanel.svelte
 * mounted under happy-dom, driven through a fake Tauri runtime.
 *
 * These assert the exit-gate invariants at the UI layer:
 *   - the demo tick is present on the wizard's first step and defaults to
 *     UNTICKED;
 *   - finishing unticked shows the completion step exactly as before (no
 *     demo panel, no demo IPC at all);
 *   - finishing ticked shows the demo panel with the copyable command and
 *     (when the backend says the demo can run) a Run the demo button;
 *   - the wizard NEVER creates a timer, under either choice (no
 *     `create_timer` IPC — the demo creates its own via the slot protocol);
 *   - with python3 present but tkinter MISSING (stock Ubuntu 24.04 without
 *     python3-tk) the button is NOT shown and the python3-tk note is.
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { mount, unmount } from 'svelte';
import Wizard from './Wizard.svelte';
import DemoPanel from './DemoPanel.svelte';

/** A DemoInfoDto-shaped fixture (matches src-tauri/src/demo.rs camelCase). */
function demoInfoFixture(overrides = {}) {
  return {
    optIn: true,
    available: true,
    demoDir: '/usr/share/bellman/testing_apps',
    command:
      'python3 /usr/share/bellman/testing_apps/lightbulb_gui/lightbulb_gui.py --slots /home/u/.local/share/io.bellman.desktop/slots',
    canLaunch: true,
    python3Present: true,
    tkinterPresent: true,
    slotsDir: '/home/u/.local/share/io.bellman.desktop/slots',
    docsPath: '/usr/share/bellman/docs/INTEGRATION.md',
    ...overrides,
  };
}

/**
 * Install a fake Tauri runtime. `handlers` maps cmd → (args) => value.
 * Returns the recorded invoke log: [{ cmd, args }].
 */
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

function wizardHandlers(demoInfoValue, wakeStatusValue = null) {
  const choices = [];
  return {
    choices,
    handlers: {
      wizard_status: async () => ({
        completed: false,
        defaults: {
          autostart: true,
          startMinimized: false,
          wakeEnabled: false,
          demo: false,
        },
      }),
      wizard_set_choice: async ({ choice }) => {
        choices.push(choice);
        return { completed: true, defaults: choice };
      },
      dependency_check: async () => ({ items: [] }),
      demo_info: async () => demoInfoValue,
      wake_status: async () => wakeStatusValue,
      wake_reprobe: async () => wakeStatusValue,
    },
  };
}

function mountWizard() {
  const target = document.createElement('div');
  document.body.appendChild(target);
  const component = mount(Wizard, { target, props: { onDone: () => {} } });
  return { target, component };
}

function setCheckbox(el, checked) {
  el.checked = checked;
  el.dispatchEvent(new window.Event('change', { bubbles: true }));
}

function clickButtonByText(root, text) {
  const btn = [...root.querySelectorAll('button')].find(
    (b) => b.textContent.trim() === text && !b.disabled,
  );
  if (!btn) throw new Error(`button not found: ${text}`);
  btn.click();
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

describe('Wizard demo offer (WIZ1)', () => {
  it('shows the demo tick on the first step, unticked by default', async () => {
    const { handlers } = wizardHandlers(demoInfoFixture());
    installTauriMock(handlers);
    const { target, component: c } = mountWizard();
    component = c;
    await flush();

    const tick = target.querySelector('[data-testid="demo-tick"]');
    expect(tick, 'demo tick must be present on the first step').toBeTruthy();
    expect(tick.checked, 'demo tick must default to unticked').toBe(false);
  });

  it('unticked: completion step is unchanged — no panel, no demo IPC, no timer created', async () => {
    const { handlers, choices } = wizardHandlers(demoInfoFixture());
    const invokes = installTauriMock(handlers);
    const { target, component: c } = mountWizard();
    component = c;
    await flush();

    clickButtonByText(target, 'Next');
    await flush();
    clickButtonByText(target, 'No thanks');
    await flush();

    expect(target.textContent).toContain('Setup complete');
    expect(
      target.querySelector('[data-testid="demo-panel"]'),
      'unticked must not show the demo panel',
    ).toBeNull();
    expect(choices).toHaveLength(1);
    expect(choices[0].demo).toBe(false);
    const cmds = invokes.map((i) => i.cmd);
    expect(cmds).not.toContain('demo_info');
    expect(cmds).not.toContain('demo_launch');
    expect(cmds).not.toContain('create_timer');
  });

  it('ticked: completion step shows the panel with command + run button; no timer created', async () => {
    const { handlers, choices } = wizardHandlers(demoInfoFixture());
    const invokes = installTauriMock(handlers);
    const { target, component: c } = mountWizard();
    component = c;
    await flush();

    setCheckbox(target.querySelector('[data-testid="demo-tick"]'), true);
    await flush();
    clickButtonByText(target, 'Next');
    await flush();
    clickButtonByText(target, 'No thanks');
    await flush();

    expect(target.textContent).toContain('Setup complete');
    const panel = target.querySelector('[data-testid="demo-panel"]');
    expect(panel, 'ticked must show the demo panel').toBeTruthy();
    const cmd = panel.querySelector('[data-testid="demo-command"]');
    expect(cmd.value).toContain('lightbulb_gui.py --slots');
    expect(
      panel.querySelector('[data-testid="demo-run"]'),
      'canLaunch fixture must render the run button',
    ).toBeTruthy();
    expect(panel.querySelector('[data-testid="demo-docs"]')).toBeTruthy();
    expect(choices).toHaveLength(1);
    expect(choices[0].demo).toBe(true);
    expect(choices[0].autostart).toBe(true);
    const cmds = invokes.map((i) => i.cmd);
    expect(cmds).toContain('demo_info');
    expect(cmds).not.toContain('create_timer');
  });
});

describe('DemoPanel interpreter gating (WIZ1)', () => {  function mountPanel(info) {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const c = mount(DemoPanel, { target, props: { info } });
    return { target, component: c };
  }

  it('python3 present but tkinter MISSING: no run button, python3-tk note + command shown', () => {
    installTauriMock({});
    const { target, component: c } = mountPanel(
      demoInfoFixture({ canLaunch: false, python3Present: true, tkinterPresent: false }),
    );
    component = c;

    expect(target.querySelector('[data-testid="demo-run"]')).toBeNull();
    const note = target.querySelector('[data-testid="demo-tk-note"]');
    expect(note, 'the python3-tk note must appear').toBeTruthy();
    expect(note.textContent).toContain('python3-tk');
    expect(target.querySelector('[data-testid="demo-command"]').value).toContain(
      'lightbulb_gui.py',
    );
  });

  it('demo unavailable: no command, no run button, honest note', () => {
    installTauriMock({});
    const { target, component: c } = mountPanel(
      demoInfoFixture({
        available: false,
        canLaunch: false,
        command: null,
        demoDir: null,
        docsPath: null,
      }),
    );
    component = c;

    expect(target.querySelector('[data-testid="demo-command"]')).toBeNull();
    expect(target.querySelector('[data-testid="demo-run"]')).toBeNull();
    expect(target.querySelector('[data-testid="demo-docs"]')).toBeNull();
    expect(target.querySelector('[data-testid="demo-unavailable"]')).toBeTruthy();
  });

  it('Run the demo invokes demo_launch (never create_timer)', async () => {
    const invokes = installTauriMock({ demo_launch: async () => 4242 });
    const { target, component: c } = mountPanel(demoInfoFixture());
    component = c;

    target.querySelector('[data-testid="demo-run"]').click();
    await flush();

    const cmds = invokes.map((i) => i.cmd);
    expect(cmds).toContain('demo_launch');
    expect(cmds).not.toContain('create_timer');
  });
});

describe('Wizard wake hint honesty (SHIP1-B)', () => {
  // The platform module's own words (crates/bellman-core/src/platform/wake/mod.rs
  // DisabledReason::fix_hint) — the wake_status DTO carries them verbatim, and
  // the wizard must render them verbatim rather than restating them.
  const PLATFORM_FIX_HINT =
    "timerfd needs CAP_WAKE_ALARM in this process. Options: (1) setcap 'cap_wake_alarm+eip' on the bellman-app binary, (2) run under a systemd user unit with AmbientCapabilities=CAP_WAKE_ALARM, or (3) install the udev rule below so the sysfs wakealarm fallback is group-writable. Plain XDG desktop autostart does not grant this capability on many desktops.";

  const wakeUnavailable = {
    statusLine:
      'Wake from sleep: OFF — no permission to arm a wake timer (missing CAP_WAKE_ALARM)',
    enabled: false,
    masterEnabled: false,
    platformEnabled: false,
    platform: 'linux',
    fixHint: PLATFORM_FIX_HINT,
    fixAction: 'linux_udev',
    udevSnippet: null,
    powercfgCommand: null,
    loginItemsUrl: null,
  };

  it('the autostart hint no longer claims XDG autostart grants CAP_WAKE_ALARM', async () => {
    const { handlers } = wizardHandlers(demoInfoFixture(), wakeUnavailable);
    installTauriMock(handlers);
    const { target, component: c } = mountWizard();
    component = c;
    await flush();

    const hint = target.querySelector('[data-testid="wizard-autostart-hint"]');
    expect(hint, 'static autostart hint must exist').toBeTruthy();
    // The old false promise (XDG autostart "preserves the ambient
    // CAP_WAKE_ALARM lineage") must be gone — the only permitted statement
    // is the negative one, matching the platform module's own fix_hint.
    expect(hint.textContent).not.toContain('preserves the ambient');
    expect(hint.textContent).toContain('CAP_WAKE_ALARM');
    expect(hint.textContent).toMatch(/does\s+not/i);
  });

  it('renders the platform fix_hint verbatim when the probe says wake is unavailable', async () => {
    const { handlers } = wizardHandlers(demoInfoFixture(), wakeUnavailable);
    installTauriMock(handlers);
    const { target, component: c } = mountWizard();
    component = c;
    await flush();

    const el = target.querySelector('[data-testid="wizard-wake-fixhint"]');
    expect(el, 'probe fix-hint must render on the autostart step').toBeTruthy();
    expect(el.textContent).toBe(PLATFORM_FIX_HINT);
  });

  it('shows no fix-hint when the probe says wake is available', async () => {
    const wakeOk = {
      ...wakeUnavailable,
      statusLine: 'Wake from sleep: ON via timerfd',
      enabled: true,
      masterEnabled: true,
      platformEnabled: true,
      fixHint: null,
      fixAction: null,
    };
    const { handlers } = wizardHandlers(demoInfoFixture(), wakeOk);
    installTauriMock(handlers);
    const { target, component: c } = mountWizard();
    component = c;
    await flush();

    expect(target.querySelector('[data-testid="wizard-wake-fixhint"]')).toBeNull();
  });

  it('the wake step reflects the real probe result, not an assumption', async () => {
    const { handlers } = wizardHandlers(demoInfoFixture(), wakeUnavailable);
    installTauriMock(handlers);
    const { target, component: c } = mountWizard();
    component = c;
    await flush();

    clickButtonByText(target, 'Next');
    await flush();

    const probe = target.querySelector('[data-testid="wizard-wake-probe"]');
    expect(probe, 'wake step must show the live probe result').toBeTruthy();
    expect(probe.textContent).toContain('no permission to arm a wake timer');
  });
});
