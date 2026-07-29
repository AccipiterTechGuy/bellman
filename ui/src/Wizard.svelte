<script>
  import { onMount } from 'svelte';
  import {
    wizardStatus,
    wizardSetChoice,
    wakeStatus,
    wakeReprobe,
    dependencyCheck,
    wakeEnrollMacos,
  } from './api.js';

  let { onDone } = $props();

  /** Steps: 0=autostart, 1=wake yes/no, 2=confirm/deps, 3=done path */
  let step = $state(0);
  let autostart = $state(true);
  let startMinimized = $state(false);
  let wakeEnabled = $state(false);
  let busy = $state(false);
  let statusLine = $state('');
  let deps = $state([]);
  let probeNote = $state('');

  onMount(async () => {
    try {
      const s = await wizardStatus();
      autostart = s?.defaults?.autostart ?? autostart;
      startMinimized = s?.defaults?.startMinimized ?? startMinimized;
      wakeEnabled = s?.defaults?.wakeEnabled ?? wakeEnabled;
    } catch {}
  });

  async function finish(withWake) {
    busy = true;
    try {
      wakeEnabled = !!withWake;
      await wizardSetChoice({ autostart, startMinimized, wakeEnabled });
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
      <p class="hint">
        On Linux, XDG autostart is also what preserves the ambient CAP_WAKE_ALARM
        lineage used for unprivileged RTC wake (systemd ≥ 254 desktop sessions).
      </p>

      <div class="q">
        <label for="w-hidden" class="checkbox-label">
          <input id="w-hidden" type="checkbox" bind:checked={startMinimized} />
          <span>Start hidden in the system tray.</span>
        </label>
      </div>

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

      <div class="actions">
        <button class="btn primary" onclick={closeWizard}>Continue</button>
      </div>
    {/if}
  </div>
</div>
