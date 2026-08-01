<script>
  import { onMount } from 'svelte';
  import {
    wizardStatus,
    wizardSetChoice,
    wakeStatus,
    wakeReprobe,
    dependencyCheck,
    wakeEnrollMacos,
    demoInfo,
  } from './api.js';
  import DemoPanel from './DemoPanel.svelte';

  let { onDone } = $props();

  /** Steps: 0=autostart, 1=wake yes/no, 2=confirm/deps, 3=done path */
  let step = $state(0);
  let autostart = $state(true);
  let startMinimized = $state(false);
  let wakeEnabled = $state(false);
  let demo = $state(false);
  let demoData = $state(null);
  let busy = $state(false);
  let statusLine = $state('');
  let deps = $state([]);
  let probeNote = $state('');
  let wake = $state(null);

  onMount(async () => {
    try {
      const s = await wizardStatus();
      autostart = s?.defaults?.autostart ?? autostart;
      startMinimized = s?.defaults?.startMinimized ?? startMinimized;
      wakeEnabled = s?.defaults?.wakeEnabled ?? wakeEnabled;
      demo = s?.defaults?.demo ?? false;
    } catch {}
    // Live probe, so the wizard never asserts a wake capability the
    // platform does not have (SHIP1-B).
    try {
      wake = await wakeStatus();
    } catch {
      wake = null;
    }
  });

  async function finish(withWake) {
    busy = true;
    try {
      wakeEnabled = !!withWake;
      await wizardSetChoice({ autostart, startMinimized, wakeEnabled, demo });
      if (demo) {
        // The panel renders only what this machine can actually run.
        try {
          demoData = await demoInfo();
        } catch {
          demoData = null;
        }
      } else {
        demoData = null;
      }
      if (withWake) {
        // Per-OS setup: macOS enrolls the helper; others just probe.
        try {
          if (typeof navigator !== 'undefined' && /Mac/i.test(navigator.platform || '')) {
            try { await wakeEnrollMacos(); } catch { /* optional */ }
          }
        } catch { /* ignore */ }
        try {
          const w = await wakeReprobe();
          statusLine = w?.statusLine || '';
          probeNote = w?.enabled
            ? 'Wake capability is available on this machine.'
            : 'Wake is optional — timers still fire after resume via the misfire pass. Use Settings fix-it if you want to enable it later.';
        } catch {
          statusLine = '';
        }
      } else {
        statusLine = 'Wake from sleep: OFF — wake from sleep is turned off in Settings';
        probeNote = 'You can enable it later from Settings.';
      }
      try {
        const d = await dependencyCheck();
        deps = d?.items || [];
      } catch {
        deps = [];
      }
      step = 2;
      busy = false;
    } catch (e) {
      busy = false;
      alert(`Could not save: ${e}`);
    }
  }

  function closeWizard() {
    onDone();
  }
</script>

<div class="wizard-backdrop" role="dialog" aria-modal="true">
  <div class="wizard">
    <h2>Welcome to Bellman</h2>

    {#if step === 0}
      <p>
        A few quick choices before we start scheduling. You can change any of
        these from Settings later.
      </p>

      <div class="q">
        <label for="w-auto" class="checkbox-label">
          <input id="w-auto" type="checkbox" bind:checked={autostart} />
          <span>Launch Bellman automatically when I log in?</span>
        </label>
      </div>
      <p class="hint" data-testid="wizard-autostart-hint">
        Autostart only launches Bellman at login. On Linux it does
        <strong>not</strong> by itself grant the CAP_WAKE_ALARM permission used
        for wake-from-sleep — if the probe below says wake is unavailable, the
        real fixes are setcap, a systemd user unit with
        AmbientCapabilities=CAP_WAKE_ALARM, or the udev rule (also in Settings).
      </p>
      {#if wake && !wake.platformEnabled && wake.fixHint}
        <p class="hint" data-testid="wizard-wake-fixhint">{wake.fixHint}</p>
      {/if}

      <div class="q">
        <label for="w-hidden" class="checkbox-label">
          <input id="w-hidden" type="checkbox" bind:checked={startMinimized} />
          <span>Start hidden in the system tray.</span>
        </label>
      </div>

      <div class="q">
        <label for="w-demo" class="checkbox-label">
          <input id="w-demo" type="checkbox" data-testid="demo-tick" bind:checked={demo} />
          <span>Show me the demo — watch a timer wake a real application</span>
        </label>
      </div>
      <p class="hint">
        A tiny example app (a lightbulb) that Bellman wakes on a schedule. It
        talks to Bellman exactly the way your own applications can — over
        plain JSON files, no plugin and no shared code. Optional, and it
        changes nothing about your setup.
      </p>

      <div class="actions">
        <button class="btn primary" disabled={busy} onclick={() => (step = 1)}>
          Next
        </button>
      </div>
    {:else if step === 1}
      <p><strong>Do you want to set up automatic wake-up from sleep?</strong></p>
      <p class="hint">
        When enabled, Bellman programs a single next RTC wake so timers can fire
        on time even if the machine was asleep. Optional — declining is fine;
        the misfire-on-resume pass covers the gap.
      </p>
      {#if wake}
        <p class="hint" data-testid="wizard-wake-probe">
          {#if wake.platformEnabled}
            {wake.statusLine || 'Wake capability is available on this machine.'}
          {:else}
            Probe: {wake.statusLine || 'wake is unavailable on this machine'} —
            answering yes turns the feature on, but the permission fix above is
            still needed for it to work.
          {/if}
        </p>
      {/if}
      <div class="actions">
        <button class="btn primary" disabled={busy} onclick={() => finish(true)}>
          {busy ? 'Working…' : 'Yes, set up wake'}
        </button>
        <button class="btn" disabled={busy} onclick={() => finish(false)}>
          No thanks
        </button>
      </div>
    {:else}
      <h3>Setup complete</h3>
      {#if statusLine}
        <p class="status-line" data-testid="wizard-wake-status">{statusLine}</p>
      {/if}
      {#if probeNote}
        <p class="hint">{probeNote}</p>
      {/if}

      {#if deps.length}
        <h4>Dependency check</h4>
        <ul class="dep-list">
          {#each deps as d}
            <li class:ok={d.ok} class:miss={!d.ok}>
              {d.ok ? '✓' : '○'} {d.name}
              {#if d.hint}<span class="hint"> — {d.hint}</span>{/if}
            </li>
          {/each}
        </ul>
      {/if}

      {#if demo && demoData}
        <h4>The demo</h4>
        <DemoPanel info={demoData} />
      {/if}

      <div class="actions">
        <button class="btn primary" onclick={closeWizard}>Continue</button>
      </div>
    {/if}
  </div>
</div>
