<script>
  import { onMount } from 'svelte';
  import {
    wakeStatus,
    wakeReprobe,
    setWakeEnabled,
    setAutostartEnabled,
    setMaxConcurrentActions,
    setPauseAll,
    getPauseAll,
    appInfo,
    wizardReRun,
    isTauri,
  } from './api.js';

  let { onToast, onRerunWizard } = $props();

  let status = $state(null);
  let info = $state(null);
  let pauseAll = $state(false);
  let busy = $state(false);
  let maxConcurrent = $state(16);
  let udevCopied = $state(false);

  async function refresh() {
    try {
      status = await wakeStatus();
      info = await appInfo();
      pauseAll = await getPauseAll();
      maxConcurrent = info?.maxConcurrentActions ?? 16;
    } catch (e) {
      // vite dev — leave empty
      status = {
        statusLine: 'Wake from sleep: (no backend)',
        enabled: false,
        masterEnabled: false,
        platform: 'dev',
        fixHint: null,
        udevSnippet: null,
      };
    }
  }

  onMount(refresh);

  async function toggleWake() {
    if (!status) return;
    busy = true;
    try {
      status = await setWakeEnabled(!status.masterEnabled);
      onToast?.(status.masterEnabled ? 'Wake from sleep enabled' : 'Wake from sleep disabled');
    } catch (e) {
      onToast?.(String(e), 'err');
    } finally {
      busy = false;
    }
  }

  async function doReprobe() {
    busy = true;
    try {
      status = await wakeReprobe();
      onToast?.('Wake capability re-probed');
    } catch (e) {
      onToast?.(String(e), 'err');
    } finally {
      busy = false;
    }
  }

  async function toggleAutostart() {
    if (!info) return;
    busy = true;
    try {
      const next = !info.autostartEnabled;
      await setAutostartEnabled(next);
      info = { ...info, autostartEnabled: next };
      onToast?.(next ? 'Autostart enabled' : 'Autostart disabled');
    } catch (e) {
      onToast?.(String(e), 'err');
    } finally {
      busy = false;
    }
  }

  async function togglePause() {
    busy = true;
    try {
      const next = !pauseAll;
      await setPauseAll(next);
      pauseAll = next;
      onToast?.(next ? 'All timers paused' : 'Timers resumed');
    } catch (e) {
      onToast?.(String(e), 'err');
    } finally {
      busy = false;
    }
  }

  async function saveMaxConcurrent() {
    busy = true;
    try {
      const v = await setMaxConcurrentActions(Number(maxConcurrent) || 16);
      maxConcurrent = v;
      onToast?.(`max_concurrent_actions = ${v}`);
    } catch (e) {
      onToast?.(String(e), 'err');
    } finally {
      busy = false;
    }
  }

  async function copyUdev() {
    if (!status?.udevSnippet) return;
    try {
      await navigator.clipboard.writeText(status.udevSnippet);
      udevCopied = true;
      onToast?.('udev rule copied');
      setTimeout(() => (udevCopied = false), 2000);
    } catch (e) {
      onToast?.(String(e), 'err');
    }
  }

  async function rerunWizard() {
    try {
      await wizardReRun();
      onRerunWizard?.();
    } catch (e) {
      onToast?.(String(e), 'err');
    }
  }

  const wakeDisabledReason = $derived(
    status && !status.enabled ? status.statusLine : null,
  );
</script>

<div class="settings-page">
  <h2>Settings</h2>

  <section class="settings-section">
    <h3>Wake from sleep</h3>
    <p class="status-line" data-testid="wake-status">{status?.statusLine ?? '…'}</p>

    <div class="settings-row">
      <label for="s-wake">
        Allow Bellman to wake this machine
        {#if status && !status.enabled && status.masterEnabled}
          <span class="hint-inline">({wakeDisabledReason})</span>
        {/if}
      </label>
      <input
        id="s-wake"
        type="checkbox"
        checked={!!status?.masterEnabled}
        disabled={busy || (status && !status.enabled && false)}
        onchange={toggleWake}
      />
    </div>

    {#if status?.fixHint}
      <div class="fix-it">
        <p class="hint">{status.fixHint}</p>
        {#if status.udevSnippet}
          <pre class="snippet">{status.udevSnippet}</pre>
          <button class="btn" onclick={copyUdev} disabled={busy}>
            {udevCopied ? 'Copied' : 'Copy udev rule'}
          </button>
        {/if}
      </div>
    {/if}

    <div class="settings-actions">
      <button class="btn" onclick={doReprobe} disabled={busy}>Re-probe</button>
    </div>
  </section>

  <section class="settings-section">
    <h3>Autostart</h3>
    <p class="hint">
      On Linux, XDG autostart also preserves the ambient CAP_WAKE_ALARM capability
      lineage needed for timerfd wake (systemd ≥ 254 desktop sessions).
    </p>
    <div class="settings-row">
      <label for="s-auto">Launch Bellman when I log in</label>
      <input
        id="s-auto"
        type="checkbox"
        checked={!!info?.autostartEnabled}
        disabled={busy || !isTauri()}
        onchange={toggleAutostart}
      />
    </div>
  </section>

  <section class="settings-section">
    <h3>Engine</h3>
    <div class="settings-row">
      <label for="s-pause">Pause all timers (vacation mode)</label>
      <input id="s-pause" type="checkbox" checked={pauseAll} disabled={busy} onchange={togglePause} />
    </div>
    <div class="settings-row">
      <label for="s-max">max_concurrent_actions</label>
      <input
        id="s-max"
        type="number"
        min="1"
        max="256"
        bind:value={maxConcurrent}
        disabled={busy}
        style="width: 5rem"
      />
      <button class="btn" onclick={saveMaxConcurrent} disabled={busy}>Save</button>
    </div>
  </section>

  <section class="settings-section">
    <h3>Setup</h3>
    <button class="btn" onclick={rerunWizard} disabled={busy}>Run setup again</button>
  </section>
</div>
