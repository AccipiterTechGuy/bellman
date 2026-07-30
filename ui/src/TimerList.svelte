<script>
  import { onMount } from 'svelte';
  import { listTimers, setEnabled, runNow, listLogTail } from './api.js';

  /** @type {(text: string, kind?: 'info'|'err') => void} */
  let { onToast, onPauseChange, onEdit, onCreate } = $props();

  let timers = $state([]);
  let selectedId = $state(null);
  let log = $state({ events: [], total: 0, skipped: 0 });
  let loading = $state(true);
  let pollHandle = null;

  /** @type {'next' | 'name'} */
  let sortBy = $state('next');
  let searchQ = $state('');
  /** @type {'' | 'once' | 'interval' | 'daily' | 'weekly' | 'monthly' | 'yearly' | 'cron'} */
  let kindFilter = $state('');
  /** @type {'' | 'enabled' | 'disabled'} */
  let enabledFilter = $state('');

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

  /** Truncate next-fire ISO to the UTC second for density grouping. */
  function fireKey(iso) {
    if (!iso) return null;
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return null;
    return Math.floor(d.getTime() / 1000);
  }

  /** Map fire-second → list of timer names sharing it (enabled only). */
  let densityMap = $derived.by(() => {
    /** @type {Map<number, string[]>} */
    const m = new Map();
    for (const t of timers) {
      if (!t.enabled) continue;
      const k = fireKey(t.nextFireUtc);
      if (k == null) continue;
      const arr = m.get(k) || [];
      arr.push(t.name);
      m.set(k, arr);
    }
    return m;
  });

  function collisionPeers(t) {
    if (!t.enabled) return [];
    const k = fireKey(t.nextFireUtc);
    if (k == null) return [];
    const names = densityMap.get(k) || [];
    return names.filter((n) => n !== t.name);
  }

  let filteredSorted = $derived.by(() => {
    const q = searchQ.trim().toLowerCase();
    let list = timers.slice();
    if (q) {
      list = list.filter((t) => (t.name || '').toLowerCase().includes(q));
    }
    if (kindFilter) {
      list = list.filter((t) => (t.kind || '') === kindFilter);
    }
    if (enabledFilter === 'enabled') {
      list = list.filter((t) => t.enabled);
    } else if (enabledFilter === 'disabled') {
      list = list.filter((t) => !t.enabled);
    }
    list.sort((a, b) => {
      if (sortBy === 'name') {
        const c = (a.name || '').localeCompare(b.name || '');
        return c !== 0 ? c : (a.id || '').localeCompare(b.id || '');
      }
      // Default: next fire ascending; nulls last; stable by name then id.
      const ta = a.nextFireUtc ? new Date(a.nextFireUtc).getTime() : Number.POSITIVE_INFINITY;
      const tb = b.nextFireUtc ? new Date(b.nextFireUtc).getTime() : Number.POSITIVE_INFINITY;
      if (ta !== tb) return ta - tb;
      const c = (a.name || '').localeCompare(b.name || '');
      return c !== 0 ? c : (a.id || '').localeCompare(b.id || '');
    });
    return list;
  });

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
  <span class="header-actions">
    <span>{filteredSorted.length} of {timers.length} timer{timers.length === 1 ? '' : 's'}</span>
    <button class="btn primary" onclick={onCreate}>+ New timer</button>
  </span>
</section>

<div class="list-toolbar" role="search">
  <label class="toolbar-field">
    <span class="toolbar-label">Search</span>
    <input type="search" bind:value={searchQ} placeholder="Filter by name…"
           aria-label="Filter timers by name" />
  </label>
  <label class="toolbar-field">
    <span class="toolbar-label">Kind</span>
    <select bind:value={kindFilter} aria-label="Filter by occurrence kind">
      <option value="">All kinds</option>
      <option value="once">once</option>
      <option value="interval">interval</option>
      <option value="daily">daily</option>
      <option value="weekly">weekly</option>
      <option value="monthly">monthly</option>
      <option value="yearly">yearly</option>
      <option value="cron">cron</option>
    </select>
  </label>
  <label class="toolbar-field">
    <span class="toolbar-label">Enabled</span>
    <select bind:value={enabledFilter} aria-label="Filter by enabled state">
      <option value="">All</option>
      <option value="enabled">Enabled</option>
      <option value="disabled">Disabled</option>
    </select>
  </label>
  <label class="toolbar-field">
    <span class="toolbar-label">Sort</span>
    <select bind:value={sortBy} aria-label="Sort timers">
      <option value="next">Next fire (default)</option>
      <option value="name">Name</option>
    </select>
  </label>
</div>

{#if loading}
  <div class="empty">Loading…</div>
{:else if timers.length === 0}
  <div class="empty">
    <p>No timers yet.</p>
    <p>Use the <code>bellman add</code> CLI to create one, or click <strong>+ New timer</strong>.</p>
  </div>
{:else if filteredSorted.length === 0}
  <div class="empty">
    <p>No timers match the current filters.</p>
  </div>
{:else}
  <table class="timer-table">
    <thead>
      <tr>
        <th>Name</th>
        <th>Kind / Summary</th>
        <th>Action</th>
        <th>Next fire (UTC)</th>
        <th>Density</th>
        <th>Enabled</th>
        <th class="col-controls">Controls</th>
      </tr>
    </thead>
    <tbody>
      {#each filteredSorted as t (t.id)}
        {@const peers = collisionPeers(t)}
        <tr class:selected={selectedId === t.id}
            class:row-collision={peers.length > 0}
            onclick={() => select(t)}>
          <td class="col-name">
            <span class="name-text">{t.name}</span>
          </td>
          <td class="col-summary">{t.kind} — {t.summary}</td>
          <td class="col-action">{t.action}</td>
          <td class="col-next">{fmtTime(t.nextFireUtc)} {#key _tick}{liveCountdown(t.nextFireUtc)}{/key}</td>
          <td class="col-density">
            {#if peers.length > 0}
              <span class="density-badge" title="Shares next fire second with: {peers.join(', ')}">
                ⚠ +{peers.length}
              </span>
              <span class="density-names name-text" title={peers.join(', ')}>{peers.join(', ')}</span>
            {:else}
              <span class="muted">—</span>
            {/if}
          </td>
          <td>
            <span class="toggle" class:on={t.enabled} role="switch" aria-checked={t.enabled}
                  aria-label={`Toggle enabled state for timer ${t.name}`}
                  onclick={(e) => toggle(t, e)}
                  onkeydown={(e) => { if (e.key === ' ' || e.key === 'Enter') { e.preventDefault(); toggle(t, e); } }}
                  tabindex="0"></span>
          </td>
          <td class="col-controls">
            <button class="btn" onclick={(e) => openEdit(t, e)}>Edit</button>
            <button class="btn" onclick={(e) => { e.stopPropagation(); select(t); }}
                    title="Show event log for this timer"
                    aria-pressed={selectedId === t.id}>
              {selectedId === t.id ? 'Hide log' : 'Log'}
            </button>
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
            <span class="ts">{e.logged_at ? new Date(e.logged_at).toLocaleTimeString() : '—'}</span>
            <span class="kind {kindClass(e.kind)}">{e.kind}</span>
            <span class="msg">{e.message || (e.run_id ? `run_id=${String(e.run_id).slice(0,8)}…` : '')}</span>
          </div>
        {/each}
      {/if}
    </div>
  {/if}
{/if}
