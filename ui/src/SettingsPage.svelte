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
    wakeFixPowercfg,
    wakeEnrollMacos,
    wakeOpenLoginItems,
    getMisfireDefaults,
    setMisfireDefaults,
  } from './api.js';

  let { onToast, onRerunWizard } = $props();

  let status = $state(null);
  let info = $state(null);
  let pauseAll = $state(false);
  let busy = $state(false);
  let maxConcurrent = $state(16);
  let udevCopied = $state(false);
  let powercfgCopied = $state(false);
  let misfirePolicy = $state('coalesce');
  let misfireGrace = $state(3600);

  async function refresh() {
    try {
      status = await wakeStatus();
      info = await appInfo();
      pauseAll = await getPauseAll();
      maxConcurrent = info?.maxConcurrentActions ?? 16;
      try {
        const m = await getMisfireDefaults();
        misfirePolicy = m?.policy ?? 'coalesce';
        misfireGrace = m?.graceSecs ?? 3600;
      } catch {
        misfirePolicy = info?.defaultMisfirePolicy ?? 'coalesce';
        misfireGrace = info?.defaultMisfireGraceSecs ?? 3600;
      }
    } catch (e) {
      status = {
        statusLine: 'Wake from sleep: (no backend)',
        enabled: false,
        masterEnabled: false,
        platformEnabled: false,
        platform: 'dev',
        fixHint: null,
        fixAction: null,
        udevSnippet: null,
        powercfgCommand: null,
        loginItemsUrl: null,
      };
    }
  }

  onMount(refresh);

  /** Master toggle is greyed when the *platform* cannot wake (BUILD_PLAN). */
  const masterGreyed = $derived(status && status.platformEnabled === false);
  const wakeDisabledReason = $derived(
    status && !status.platformEnabled ? status.statusLine : null,
  );

  async function toggleWake() {
    if (!status || masterGreyed) return;
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

  async function saveMisfire() {
    busy = true;
    try {
      const m = await setMisfireDefaults(misfirePolicy, Number(misfireGrace) || 3600);
      misfirePolicy = m.policy;
      misfireGrace = m.graceSecs;
      onToast?.(`Misfire default: ${m.policy} / ${m.graceSecs}s`);
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

  async function copyPowercfg() {
    if (!status?.powercfgCommand) return;
    try {
      await navigator.clipboard.writeText(status.powercfgCommand);
      powercfgCopied = true;
      onToast?.('powercfg command copied');
      setTimeout(() => (powercfgCopied = false), 2000);
    } catch (e) {
      onToast?.(String(e), 'err');
    }
  }

  async function runPowercfg() {
    busy = true;
    try {
      status = await wakeFixPowercfg('ac');
      onToast?.('Elevated powercfg finished — check status line');
    } catch (e) {
      onToast?.(String(e), 'err');
    } finally {
      busy = false;
    }
  }

  async function enrollMacos() {
    busy = true;
    try {
      status = await wakeEnrollMacos();
      onToast?.('macOS wake helper enroll requested');
    } catch (e) {
      onToast?.(String(e), 'err');
    } finally {
      busy = false;
    }
  }

  async function openLoginItems() {
    busy = true;
    try {
      status = await wakeOpenLoginItems();
      onToast?.('Opened Login Items');
    } catch (e) {
      onToast?.(String(e), 'err');
    } finally {
      busy = false;
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
</script>

<div class="settings-page">
  <h2>Settings</h2>

  <section class="settings-section">
    <h3>Wake from sleep</h3>
    <p class="status-line" data-testid="wake-status">{status?.statusLine ?? '…'}</p>

    <div class="settings-row">
      <label for="s-wake" class="checkbox-label" class:disabled-opt={masterGreyed}>
        <input
          id="s-wake"
          type="checkbox"
          checked={!!status?.masterEnabled}
          disabled={busy || masterGreyed}
          onchange={toggleWake}
        />
        <span>
          Allow Bellman to wake this machine
          {#if masterGreyed && wakeDisabledReason}
            <span class="hint-inline">— {wakeDisabledReason}</span>
          {/if}
        </span>
      </label>
    </div>

    {#if status?.fixHint}
      <div class="fix-it">
        <p class="hint">{status.fixHint}</p>

        {#if status.fixAction === 'linux_udev' && status.udevSnippet}
          <pre class="snippet">{status.udevSnippet}</pre>
          <button class="btn" onclick={copyUdev} disabled={busy}>
            {udevCopied ? 'Copied' : 'Copy udev rule'}
          </button>
        {/if}

        {#if status.fixAction === 'windows_powercfg' || status.powercfgCommand}
          {#if status.powercfgCommand}
            <pre class="snippet">{status.powercfgCommand}</pre>
          {/if}
          <div class="settings-actions">
            <button class="btn primary" onclick={runPowercfg} disabled={busy}>
              Enable wake timers (elevated powercfg)
            </button>
            <button class="btn" onclick={copyPowercfg} disabled={busy || !status.powercfgCommand}>
              {powercfgCopied ? 'Copied' : 'Copy powercfg'}
            </button>
          </div>
        {/if}

        {#if status.fixAction === 'macos_enroll'}
          <div class="settings-actions">
            <button class="btn primary" onclick={enrollMacos} disabled={busy}>
              Enroll wake helper
            </button>
          </div>
        {/if}

        {#if status.fixAction === 'macos_login_items'}
          <div class="settings-actions">
            <button class="btn primary" onclick={openLoginItems} disabled={busy}>
              Open Login Items
            </button>
          </div>
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
      Launch on login. On Linux this uses an XDG desktop file. Note: plain desktop
      autostart does <strong>not</strong> grant CAP_WAKE_ALARM on many desktops —
      see the wake fix-it options above if wake is Disabled.
    </p>
    <div class="settings-row">
      <label for="s-auto" class="checkbox-label">
        <input
          id="s-auto"
          type="checkbox"
          checked={!!info?.autostartEnabled}
          disabled={busy || !isTauri()}
          onchange={toggleAutostart}
        />
        <span>Launch Bellman when I log in</span>
      </label>
    </div>
  </section>

  <section class="settings-section">
    <h3>Misfire defaults</h3>
    <p class="hint">
      Applied to new calendar timers when the dialog does not set a policy.
      Interval timers still default to skip.
    </p>
    <div class="settings-row">
      <label for="s-misfire-policy">Policy</label>
      <select id="s-misfire-policy" bind:value={misfirePolicy} disabled={busy}>
        <option value="coalesce">coalesce</option>
        <option value="skip">skip</option>
        <option value="catch_up">catch_up</option>
      </select>
    </div>
    <div class="settings-row">
      <label for="s-misfire-grace">Grace (seconds)</label>
      <input
        id="s-misfire-grace"
        type="number"
        min="0"
        bind:value={misfireGrace}
        disabled={busy}
        class="input-narrow"
      />
      <button class="btn" onclick={saveMisfire} disabled={busy}>Save</button>
    </div>
  </section>

  <section class="settings-section">
    <h3>Engine</h3>
    <div class="settings-row">
      <label for="s-pause" class="checkbox-label">
        <input id="s-pause" type="checkbox" checked={pauseAll} disabled={busy} onchange={togglePause} />
        <span>Pause all timers (vacation mode)</span>
      </label>
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
        class="input-narrow"
      />
      <button class="btn" onclick={saveMaxConcurrent} disabled={busy}>Save</button>
    </div>
  </section>

  <section class="settings-section">
    <h3>Setup</h3>
    <button class="btn" onclick={rerunWizard} disabled={busy}>Run setup again</button>
  </section>
</div>
