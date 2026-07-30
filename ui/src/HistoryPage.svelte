<script>
  import { onMount, onDestroy } from 'svelte';
  import { listTimers, listLogTail } from './api.js';

  /** @type {(text: string, kind?: 'info'|'err') => void} */
  let { onToast } = $props();

  let timers = $state([]);
  let timerId = $state(''); // '' = all
  let kind = $state(''); // '' = all
  let log = $state({ events: [], totalRecords: 0, skipped: 0 });
  let loading = $state(true);
  let pollHandle = null;

  const KINDS = ['fired', 'fired_late', 'coalesced', 'skipped_misfire',
    'registered', 'wake_delivered', 'wake_failed', 'no_ack',
    'pruned', 'year_recalibrate'];

  async function refreshTimers() {
    try {
      timers = await listTimers();
    } catch (e) {
      onToast(String(e), 'err');
      timers = [];
    }
  }

  async function refreshLog() {
    try {
      log = await listLogTail(timerId || null, 500);
    } catch (e) {
      onToast(String(e), 'err');
    } finally {
      loading = false;
    }
  }

  onMount(async () => {
    await refreshTimers();
    await refreshLog();
    pollHandle = setInterval(refreshLog, 5000);
  });

  onDestroy(() => {
    if (pollHandle) clearInterval(pollHandle);
  });

  function kindClass(k) {
    if (['wake_failed', 'no_ack', 'pruned'].includes(k)) return 'err';
    if (['fired_late', 'skipped_misfire', 'coalesced', 'year_recalibrate'].includes(k)) return 'warn';
    return '';
  }

  let filteredEvents = $derived(
    log.events.filter((e) => (kind === '' ? true : e.kind === kind)),
  );

  function fmtTs(iso) {
    try { return new Date(iso).toLocaleString(); } catch { return iso; }
  }

  function shortTimerName(timerId) {
    if (!timerId) return '';
    const t = timers.find((x) => x.id === timerId);
    return t ? t.name : '';
  }
</script>

<section class="section-title">
  <span>Run history</span>
  <span class="subtitle-meta">
    {log.totalRecords} record{log.totalRecords === 1 ? '' : 's'}
    {log.skipped ? `, ${log.skipped} skipped` : ''}
  </span>
</section>

<div class="history-toolbar">
  <label>
    Timer:
    <select bind:value={timerId} onchange={refreshLog}>
      <option value="">All</option>
      {#each timers as t}
        <option value={t.id}>{t.name}</option>
      {/each}
    </select>
  </label>
  <label>
    Kind:
    <select bind:value={kind}>
      <option value="">All</option>
      {#each KINDS as k}
        <option value={k}>{k}</option>
      {/each}
    </select>
  </label>
  <button class="btn" onclick={refreshLog}>Refresh</button>
</div>

{#if loading}
  <div class="empty">Loading…</div>
{:else if filteredEvents.length === 0}
  <div class="empty">
    <p>No events match the filter.</p>
    <p>Fires appear here once any timer triggers. The live tail polls every 5 s.</p>
  </div>
{:else}
  <div class="history-list">
    {#each filteredEvents.slice().reverse() as e (e.event_id)}
      <div class="log-line">
        <span class="ts">{fmtTs(e.logged_at)}</span>
        <span class="kind {kindClass(e.kind)}">{e.kind}</span>
        <span class="msg">
          {#if e.timer_name}
            <strong title={e.timer_id}>{e.timer_name}</strong>
          {:else if e.timer_id}
            <strong>{shortTimerName(e.timer_id) || e.timer_id}</strong>
          {/if}
          {#if e.scheduled_for}<span class="sched"> @ {fmtTs(e.scheduled_for)}</span>{/if}
          {#if e.message}<span class="msg-text"> · {e.message}</span>{/if}
          {#if e.error}<span class="msg-text err"> · {e.error}</span>{/if}
          {#if e.duration_ms != null}<span class="msg-text"> · {e.duration_ms} ms</span>{/if}
        </span>
      </div>
    {/each}
  </div>
{/if}
