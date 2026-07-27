<script>
  import { onMount } from 'svelte';
  import { listTimers, getTimer, setEnabled, runNow, listLogTail } from './api.js';

  /** @type {(text: string, kind?: 'info'|'err') => void} */
  let { onToast, onPauseChange, onEdit, onCreate } = $props();

  let timers = $state([]);
  let selectedId = $state(null);
  let log = $state({ events: [], total: 0, skipped: 0 });
  let loading = $state(true);
  let pollHandle = null;

  async function refresh() {
    try {
      timers = await listTimers();
    } catch (e) {
      onToast(String(e), 'err');
      timers = [];
    } finally {
      loading = false;
    }
  }

  async function refreshLog() {
    if (!selectedId) { log = { events: [], total: 0, skipped: 0 }; return; }
    try {
      log = await listLogTail(selectedId, 200);
    } catch (e) {
      onToast(String(e), 'err');
    }
  }

  async function toggle(t, e) {
    e.stopPropagation();
    try {
      const updated = await setEnabled(t.id, !t.enabled, t.revision);
      timers = timers.map((x) => (x.id === updated.id ? updated : x));
      onToast(`${t.name}: ${updated.enabled ? 'enabled' : 'disabled'}`);
    } catch (err) {
      onToast(String(err), 'err');
    }
  }

  async function fireNow(t, e) {
    e.stopPropagation();
    try {
      const r = await runNow(t.id);
      onToast(`${t.name} fired: ${r.message}`);
      await refresh();
      if (selectedId === t.id) await refreshLog();
    } catch (err) {
      onToast(String(err), 'err');
    }
  }

  function openEdit(t, e) {
    e.stopPropagation();
    onEdit && onEdit(t);
  }

  function select(t) {
    selectedId = (selectedId === t.id) ? null : t.id;
    refreshLog();
  }

  function fmtTime(iso) {
    if (!iso) return '—';
    try {
      return new Date(iso).toLocaleString();
    } catch { return iso; }
  }

  function liveCountdown(iso) {
    if (!iso) return '';
    const ms = new Date(iso) - Date.now();
    if (ms <= 0) return 'due';
    const s = Math.floor(ms / 1000);
    if (s < 60) return `${s}s`;
    if (s < 3600) return `${Math.floor(s/60)}m ${s%60}s`;
    if (s < 86400) return `${Math.floor(s/3600)}h ${Math.floor((s%3600)/60)}m`;
    return `${Math.floor(s/86400)}d ${Math.floor((s%86400)/3600)}h`;
  }

  let _tick = $state(0);
  onMount(() => {
    refresh();
    pollHandle = setInterval(refresh, 5000);
    const tick = setInterval(() => { _tick++; }, 1000);
    return () => {
      if (pollHandle) clearInterval(pollHandle);
      clearInterval(tick);
    };
  });

  function kindClass(k) {
    if (k === 'wake_failed' || k === 'no_ack' || k === 'pruned') return 'err';
    if (k === 'fired_late' || k === 'skipped_misfire' || k === 'coalesced' || k === 'year_recalibrate') return 'warn';
    return '';
  }
</script>

<section class="section-title">
  <span>All timers</span>
  <span style="font-weight: 400; text-transform: none; letter-spacing: 0; display: flex; gap: 12px; align-items: center;">
    <span>{timers.length} timer{timers.length === 1 ? '' : 's'}</span>
    <button class="btn primary" onclick={onCreate}>+ New timer</button>
  </span>
</section>

{#if loading}
  <div class="empty">Loading…</div>
{:else if timers.length === 0}
  <div class="empty">
    <p>No timers yet.</p>
    <p>Use the <code>bellman add</code> CLI to create one, or click a "Run now" test fixture below.</p>
  </div>
{:else}
  <table class="timer-table">
    <thead>
      <tr>
        <th>Name</th>
        <th>Kind / Summary</th>
        <th>Action</th>
        <th>Next fire (UTC)</th>
        <th>Enabled</th>
        <th class="col-controls">Controls</th>
      </tr>
    </thead>
    <tbody>
      {#each timers as t (t.id)}
        <tr class:selected={selectedId === t.id} onclick={() => select(t)} style="cursor: pointer;">
          <td class="col-name">{t.name}</td>
          <td class="col-summary">{t.kind} — {t.summary}</td>
          <td class="col-action">{t.action}</td>
          <td class="col-next">{fmtTime(t.nextFireUtc)} {#key _tick}{liveCountdown(t.nextFireUtc)}{/key}</td>
          <td>
            <span class="toggle" class:on={t.enabled} role="switch" aria-checked={t.enabled}
                  onclick={(e) => toggle(t, e)} tabindex="0"></span>
          </td>
          <td class="col-controls">
            <button class="btn" onclick={(e) => openEdit(t, e)}>Edit</button>
            <button class="btn primary" onclick={(e) => fireNow(t, e)}>Run now</button>
          </td>
        </tr>
      {/each}
    </tbody>
  </table>

  {#if selectedId}
    <div class="log-panel">
      <header>
        <span>Event log tail (most recent first) — {log.totalRecords} record{log.totalRecords === 1 ? '' : 's'}{log.skipped ? `, ${log.skipped} skipped` : ''}</span>
        <button class="btn" onclick={() => { selectedId = null; log = { events: [], total: 0, skipped: 0 }; }}>Close</button>
      </header>
      {#if log.events.length === 0}
        <div class="empty">No events for this timer yet.</div>
      {:else}
        {#each log.events.slice().reverse() as e (e.event_id)}
          <div class="log-line">
            <span class="ts">{e.ts ? new Date(e.ts).toLocaleTimeString() : '—'}</span>
            <span class="kind {kindClass(e.kind)}">{e.kind}</span>
            <span class="msg">{e.message || (e.run_id ? `run_id=${String(e.run_id).slice(0,8)}…` : '')}</span>
          </div>
        {/each}
      {/if}
    </div>
  {/if}
{/if}
