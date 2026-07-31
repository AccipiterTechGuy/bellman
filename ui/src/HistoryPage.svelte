<script>
  import { onMount, onDestroy } from 'svelte';
  import { listTimers, listLogTail, listRunStates, onRunStatusChanged } from './api.js';
  import { runRowDisplay, eventKindDisplay, isNonTerminal } from './run-status.js';

  /** @type {(text: string, kind?: 'info'|'err') => void} */
  let { onToast } = $props();

  let timers = $state([]);
  let timerId = $state(''); // '' = all
  let kind = $state(''); // '' = all
  let log = $state({ events: [], totalRecords: 0, skipped: 0 });
  let loading = $state(true);
  let pollHandle = null;

  /** IK5: timerId → current run DTO (integration-owned timers only). */
  let runStates = $state({});
  /** Advances elapsed/overdue text; ticking only while a run is open. */
  let liveTick = $state(0);

  const KINDS = ['fired', 'fired_late', 'coalesced', 'skipped_misfire',
    'registered', 'wake_delivered', 'wake_failed', 'no_ack',
    'acknowledged', 'running', 'completed', 'failed',
    'cancelled', 'superseded', 'reply_rejected',
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

  /** IK5: full run-state map (initial load / resync). */
  async function refreshRunStates() {
    try {
      const list = await listRunStates();
      const map = {};
      for (const r of list) map[r.timerId] = r;
      runStates = map;
    } catch (e) {
      onToast(String(e), 'err');
    }
  }

  /** IK5: `run-status-changed` carries only the timer id — refetch that row. */
  async function applyRunStateUpdate(id) {
    if (!id) { await refreshRunStates(); return; }
    try {
      const list = await listRunStates(id);
      const next = { ...runStates };
      if (list.length === 0) delete next[id];
      else next[id] = list[0];
      runStates = next;
    } catch (e) {
      onToast(String(e), 'err');
    }
  }

  /** Current NON-TERMINAL runs, pinned above "what happened" (IK5). */
  let liveRuns = $derived(
    Object.values(runStates)
      .filter((r) => isNonTerminal(r.state))
      .filter((r) => (timerId === '' ? true : r.timerId === timerId))
      .sort((a, b) => Date.parse(b.firedAt) - Date.parse(a.firedAt)),
  );

  // The seconds-rate timer exists ONLY to advance elapsed/overdue text
  // while an owned run is non-terminal. With nothing running it does not
  // exist — and stopping it never hides a state change (those arrive via
  // `run-status-changed`, not via this tick).
  $effect(() => {
    if (liveRuns.length === 0) return;
    const t = setInterval(() => { liveTick++; }, 1000);
    return () => clearInterval(t);
  });

  let unlisten = () => {};
  let dead = false;
  onMount(async () => {
    await refreshTimers();
    await refreshLog();
    await refreshRunStates();
    pollHandle = setInterval(refreshLog, 5000);
    onRunStatusChanged((id) => { applyRunStateUpdate(id); }).then((u) => {
      if (dead) u(); else unlisten = u;
    });
  });

  onDestroy(() => {
    dead = true;
    unlisten();
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
    — 30-day history (configurable)
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

{#if liveRuns.length > 0}
  <div class="live-runs">
    <div class="live-runs-title">Happening now</div>
    {#key liveTick}
      {#each liveRuns as r (r.runId)}
        {@const disp = runRowDisplay(r, Date.now())}
        <div class="log-line live">
          <span class="run-state tone-{disp.tone}">{disp.dot}</span>
          <span class="msg">
            <strong title={r.timerId}>{r.timerName}</strong>
            <span class="run-state tone-{disp.tone}"> · {disp.text}</span>
          </span>
        </div>
      {/each}
    {/key}
  </div>
{/if}

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
      {@const kd = eventKindDisplay(e)}
      <div class="log-line">
        <span class="ts">{fmtTs(e.logged_at)}</span>
        <span class="kind {kd ? kd.cls : kindClass(e.kind)}">{kd ? kd.label : e.kind}</span>
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
