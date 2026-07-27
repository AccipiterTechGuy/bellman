<script>
  import { onMount } from 'svelte';
  import { wizardStatus, wizardSetChoice } from './api.js';

  let { onDone } = $props();

  let autostart = $state(true);
  let startMinimized = $state(false);
  let wakeEnabled = $state(false);
  let busy = $state(false);

  onMount(async () => {
    try {
      const s = await wizardStatus();
      autostart = s?.defaults?.autostart ?? autostart;
      startMinimized = s?.defaults?.start_minimized ?? startMinimized;
      wakeEnabled = s?.defaults?.wake_enabled ?? wakeEnabled;
    } catch {}
  });

  async function save() {
    busy = true;
    try {
      await wizardSetChoice({ autostart, startMinimized, wakeEnabled });
      onDone();
    } catch (e) {
      busy = false;
      alert(`Could not save: ${e}`);
    }
  }
</script>

<div class="wizard-backdrop" role="dialog" aria-modal="true">
  <div class="wizard">
    <h2>Welcome to Bellman</h2>
    <p>A few quick choices before we start scheduling. You can change any of these from the tray menu later.</p>

    <div class="q">
      <label for="w-auto">Launch Bellman automatically when I log in?</label>
      <input id="w-auto" type="checkbox" bind:checked={autostart} />
    </div>

    <div class="q">
      <label for="w-hidden">Start hidden in the system tray (window hidden until I click the tray icon).</label>
      <input id="w-hidden" type="checkbox" bind:checked={startMinimized} />
    </div>

    <div class="q">
      <label for="w-wake">Try to wake this machine from sleep so timers fire on time? (Requires OS permissions.)</label>
      <input id="w-wake" type="checkbox" bind:checked={wakeEnabled} />
    </div>

    <div class="actions">
      <button class="btn primary" disabled={busy} onclick={save}>
        {busy ? 'Saving…' : 'Save and continue'}
      </button>
    </div>
  </div>
</div>
