<script>
  import {
    WEEKDAY_LABELS,
    monthGrid,
    isoDate,
    jsIsoWeekday,
    listCalendarTruth,
    listLogTail,
    isTauri,
  } from './api.js';
  import {
    buildClientTruthEntries,
    groupEntriesByDate,
    normaliseTruthWindow,
    sourceLabel,
    outcomeLabel,
  } from './calendar-truth.js';

  /** @type {(t:any) => void} */
  let { onEdit, onCreateDate, onToast, timers = [], tick = 0 } = $props();

  let year = $state(new Date().getFullYear());
  let month = $state(new Date().getMonth()); // 0-indexed

  /** @type {Record<string, any[]>} */
  let daysMap = $state({});
  let loading = $state(false);

  $effect(() => {
    void tick;
    void timers;
    void year;
    void month;
    const gen = (rebuild.gen = (rebuild.gen || 0) + 1);
    rebuild(gen);
  });

  async function rebuild(gen) {
    const grid = monthGrid(year, month);
    const firstIso = isoDate(grid[0]);
    const lastIso = isoDate(grid[grid.length - 1]);
    loading = true;
    try {
      let entries = [];
      if (isTauri()) {
        try {
          const win = normaliseTruthWindow(await listCalendarTruth(firstIso, lastIso));
          entries = win.entries;
        } catch (e) {
          if (typeof onToast === 'function') {
            onToast(`Calendar truth: ${e}`, 'err');
          }
          entries = await clientFallback(firstIso, lastIso);
        }
      } else {
        entries = await clientFallback(firstIso, lastIso);
      }
      if (gen !== rebuild.gen) return;
      daysMap = groupEntriesByDate(entries);
    } finally {
      if (gen === rebuild.gen) loading = false;
    }
  }

  async function clientFallback(from, to) {
    let events = [];
    if (isTauri()) {
      try {
        const log = await listLogTail(null, 2000);
        events = log.events || [];
      } catch {
        events = [];
      }
    }
    return buildClientTruthEntries({
      timers,
      events,
      from,
      to,
      now: new Date(),
    });
  }

  function shiftMonth(delta) {
    let m = month + delta;
    let y = year;
    while (m < 0) { m += 12; y -= 1; }
    while (m > 11) { m -= 12; y += 1; }
    month = m;
    year = y;
  }

  function shiftYear(delta) { year += delta; }
  function jumpToday() {
    const now = new Date();
    year = now.getFullYear();
    month = now.getMonth();
  }

  function onChipClick(entry) {
    if (!entry?.timerId || typeof onEdit !== 'function') return;
    const t = (timers || []).find((x) => x.id === entry.timerId);
    if (t) onEdit(t);
  }

  function chipAria(entry) {
    const src = sourceLabel(entry.source);
    const out = outcomeLabel(entry.outcome);
    return `${src}: ${entry.name}, ${entry.time}, ${out}`;
  }

  let monthLabel = $derived(
    new Date(year, month, 1).toLocaleString(undefined, { month: 'long', year: 'numeric' }),
  );
  let grid = $derived(monthGrid(year, month));
  let todayIso = $derived(isoDate(new Date()));
</script>

<section class="section-title">
  <span>Month</span>
  <span class="subtitle-meta">{monthLabel}</span>
</section>

<div class="month-toolbar">
  <button class="btn" onclick={() => shiftYear(-1)} aria-label="previous year">« Year</button>
  <button class="btn" onclick={() => shiftMonth(-1)} aria-label="previous month">◀ Month</button>
  <button class="btn" onclick={jumpToday}>Today</button>
  <button class="btn" onclick={() => shiftMonth(1)} aria-label="next month">Month ▶</button>
  <button class="btn" onclick={() => shiftYear(1)} aria-label="next year">Year »</button>
  {#if loading}
    <span class="muted" aria-live="polite">Updating…</span>
  {/if}
</div>

<div class="month-grid">
  {#each WEEKDAY_LABELS as label}
    <div class="month-head">{label}</div>
  {/each}
  {#each grid as d, i}
    {@const iso = isoDate(d)}
    {@const inMonth = d.getMonth() === month}
    {@const isToday = iso === todayIso}
    {@const chips = daysMap[iso] || []}
    {@const crowded = chips.length >= 3}
    <div class="month-cell"
         class:out={!inMonth}
         class:today={isToday}
         class:crowded={crowded}
         class:has-fires={chips.length > 0}>
      <div class="month-cell-head">
        <span class="day-num">{d.getDate()}</span>
        {#if chips.length > 0}
          <span class="day-fire-count" aria-label="{chips.length} fires">{chips.length}</span>
        {/if}
        {#if jsIsoWeekday(d) === 1 || d.getDate() === 1}
          <span class="day-month">{d.toLocaleString(undefined, { month: 'short' })}</span>
        {/if}
      </div>
      {#if chips.length > 0}
        <div class="month-chip-wrap">
          {#each chips.slice(0, 3) as entry}
            <button
              type="button"
              class="month-chip truth-{entry.source} outcome-{entry.outcome} kind-{entry.kind || 'unknown'} name-text"
              onclick={() => onChipClick(entry)}
              title={chipAria(entry)}
              aria-label={chipAria(entry)}
            >
              <span class="month-chip-source" data-source={entry.source}>{sourceLabel(entry.source)}</span>
              {entry.name}
              {#if entry.source === 'recorded'}
                <span class="month-chip-outcome">{outcomeLabel(entry.outcome)}</span>
              {/if}
            </button>
          {/each}
          {#if chips.length > 3}
            <div class="month-chip-more">+{chips.length - 3} more</div>
          {/if}
        </div>
      {:else if iso < todayIso}
        <div class="month-empty-past muted" aria-label="No recorded events">No recorded events</div>
      {/if}
      <!-- Real <button> so AT-SPI/WebKit exposes a clickable create path (role=div fails). -->
      <button type="button"
              class="month-day-create"
              class:empty={chips.length === 0}
              title="Create timer on {iso}"
              aria-label="Create timer on {iso}"
              onclick={() => { if (typeof onCreateDate === 'function') onCreateDate(iso); }}>
        {#if chips.length === 0}+ New{:else}+ Add{/if}
      </button>
    </div>
  {/each}
</div>
