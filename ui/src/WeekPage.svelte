<script>
  import {
    WEEKDAY_LABELS,
    isoWeekStart,
    addDays,
    isoDate,
    formatIsoWeekHeading,
    listCalendarTruth,
    listLogTail,
    isTauri,
  } from './api.js';
  import {
    buildClientTruthEntries,
    groupEntriesByWeekday,
    normaliseTruthWindow,
    sourceLabel,
    outcomeLabel,
  } from './calendar-truth.js';

  /** @type {(t:any) => void} */
  let { onEdit, onCreateDate, onToast, timers = [], tick = 0 } = $props();

  let weekAnchor = $state(new Date());
  /** @type {any[][]} */
  let cells = $state(WEEKDAY_LABELS.map(() => []));
  let loading = $state(false);

  $effect(() => {
    void tick;
    void timers;
    void weekAnchor;
    // Async rebuild — keep latest request only via generation counter.
    const gen = (rebuild.gen = (rebuild.gen || 0) + 1);
    rebuild(gen);
  });

  async function rebuild(gen) {
    const weekStart = isoWeekStart(weekAnchor);
    const weekEnd = addDays(weekStart, 6);
    const from = isoDate(weekStart);
    const to = isoDate(weekEnd);
    loading = true;
    try {
      let entries = [];
      if (isTauri()) {
        try {
          const win = normaliseTruthWindow(await listCalendarTruth(from, to));
          entries = win.entries;
        } catch (e) {
          // Fall back to client merge if command missing mid-upgrade.
          if (typeof onToast === 'function') {
            onToast(`Calendar truth: ${e}`, 'err');
          }
          entries = await clientFallback(from, to);
        }
      } else {
        entries = await clientFallback(from, to);
      }
      if (gen !== rebuild.gen) return;
      cells = groupEntriesByWeekday(entries, weekAnchor);
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

  function shiftWeek(delta) {
    const d = new Date(weekAnchor);
    d.setDate(d.getDate() + delta * 7);
    weekAnchor = d;
  }

  function jumpToday() {
    weekAnchor = new Date();
  }

  let weekLabel = $derived(formatIsoWeekHeading(weekAnchor));
  let todayIso = $derived(isoDate(new Date()));

  function dayIsoForCol(colIndex) {
    const weekStart = isoWeekStart(weekAnchor);
    return isoDate(addDays(weekStart, colIndex));
  }

  function isTodayCol(colIndex) {
    return dayIsoForCol(colIndex) === todayIso;
  }

  function onEmptyDayClick(colIndex) {
    if (typeof onCreateDate === 'function') {
      onCreateDate(dayIsoForCol(colIndex));
    }
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
</script>

<section class="section-title week-section-title">
  <span>Week</span>
  <span class="subtitle-meta week-identity" data-testid="week-identity">{weekLabel}</span>
</section>

<div class="week-toolbar">
  <button class="btn" onclick={() => shiftWeek(-1)} aria-label="previous week">◀ Prev</button>
  <button class="btn" onclick={jumpToday}>This week</button>
  <button class="btn" onclick={() => shiftWeek(1)} aria-label="next week">Next ▶</button>
  {#if loading}
    <span class="muted" aria-live="polite">Updating…</span>
  {/if}
</div>

<div class="week-grid">
  {#each WEEKDAY_LABELS as label, i}
    {@const colIsToday = isTodayCol(i)}
    <div
      class="week-col"
      class:today={colIsToday}
      aria-current={colIsToday ? 'date' : undefined}
      data-day={dayIsoForCol(i)}
    >
      <header>
        <span>{label}</span>
        <span class="day-count" title="{cells[i].length} fire(s) this day">
          {cells[i].length > 0 ? cells[i].length : ''}
        </span>
      </header>
      {#if cells[i].length === 0}
        <button type="button" class="empty-cell empty-day-create"
                title="Create timer on {dayIsoForCol(i)}"
                onclick={() => onEmptyDayClick(i)}>
          <span class="empty-day-hint">+ New</span>
          <span class="muted mono">{dayIsoForCol(i)}</span>
          {#if dayIsoForCol(i) < todayIso}
            <span class="empty-truth-hint">No recorded events</span>
          {/if}
        </button>
      {:else}
        {#each cells[i] as entry}
          <button
            type="button"
            class="chip truth-{entry.source} outcome-{entry.outcome}"
            class:interval={entry.kind === 'interval' || entry.kind === 'cron'}
            onclick={() => onChipClick(entry)}
            aria-label={chipAria(entry)}
            title={chipAria(entry)}
          >
            <div class="chip-meta">
              <span class="chip-source" data-source={entry.source}>{sourceLabel(entry.source)}</span>
              {#if entry.source === 'recorded'}
                <span class="chip-outcome outcome-{entry.outcome}">{outcomeLabel(entry.outcome)}</span>
              {/if}
            </div>
            <div class="chip-time">{entry.time}</div>
            <div class="chip-name name-text" title={entry.name}>{entry.name}</div>
          </button>
        {/each}
        <button type="button" class="empty-day-create subtle"
                title="Create another timer on {dayIsoForCol(i)}"
                onclick={() => onEmptyDayClick(i)}>+ New on this day</button>
      {/if}
    </div>
  {/each}
</div>
