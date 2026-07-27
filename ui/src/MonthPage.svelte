<script>
  import {
    WEEKDAY_LABELS,
    monthGrid,
    isoDate,
    jsIsoWeekday,
    parseUtc,
    daysInMonth,
    weeklyDaysFromOccurrence,
    kindFromOccurrence,
  } from './api.js';

  /** @type {(t:any) => void} */
  let { onEdit, onToast, timers = [], tick = 0 } = $props();

  let year = $state(new Date().getFullYear());
  let month = $state(new Date().getMonth()); // 0-indexed

  // Per-day list of timers scheduled to fire that day (UTC date).
  let daysMap = $state({});

  $effect(() => {
    void tick;
    rebuild(timers, year, month);
  });

  // Mirror the core's `InvalidMonthDayPolicy::Clamp` semantics: a
  // monthly day-31 timer fires on the last valid day of short months
  // (Feb 28/29, Apr 30, etc). Implemented in JS via `daysInMonth`
  // so the GUI matches `bellman next` for these timers.
  function clampedDayOfMonth(year, month, day) {
    return Math.min(day, daysInMonth(year, month));
  }

  // Build a map of `YYYY-MM-DD → [chip]` for the visible grid. Calendar
  // kinds that match month/year/day rule fire on their clamped dates;
  // weekly/daily appear on every visible day; once/cron/interval
  // appear only on their next fire day inside the visible span. Matches
  // the spec: "Month page — month calendar (year-aware grid, prev/next
  // month + year navigation) showing monthly/yearly/once timers on
  // their dates".
  function rebuild(timers, year, month) {
    const grid = monthGrid(year, month);
    const firstIso = isoDate(grid[0]);
    const lastIso = isoDate(grid[grid.length - 1]);
    const map = {};
    const safePush = (iso, chip) => {
      (map[iso] = map[iso] || []).push(chip);
    };
    for (const t of timers) {
      const occ = t.occurrence || {};
      const occKind = kindFromOccurrence(occ);
      if (occKind === 'monthly') {
        const day = Number(occ.day || 0);
        if (!day) continue;
        const targetDay = clampedDayOfMonth(year, month, day);
        const targetIso = `${year}-${String(month + 1).padStart(2, '0')}-${String(targetDay).padStart(2, '0')}`;
        // Always render the chip on the clamped date even if it's outside
        // the visible 6×7 grid (so the user sees the timer in its month
        // and can navigate to it via Today/this-month buttons).
        safePush(targetIso, { timer: t, kind: 'monthly', monthDayClamped: targetDay !== day });
      } else if (occKind === 'yearly') {
        const mo = Number(occ.month || 0);
        const day = Number(occ.day || 0);
        if (!mo || !day || mo - 1 !== month) continue;
        const targetDay = clampedDayOfMonth(year, month, day);
        const targetIso = `${year}-${String(month + 1).padStart(2, '0')}-${String(targetDay).padStart(2, '0')}`;
        safePush(targetIso, { timer: t, kind: 'yearly' });
      } else if (occKind === 'once' && t.nextFireUtc) {
        const d = parseUtc(t.nextFireUtc);
        if (!d) continue;
        const iso = isoDate(d);
        if (iso >= firstIso && iso <= lastIso) {
          safePush(iso, { timer: t, kind: 'once' });
        }
      } else if ((occKind === 'cron' || occKind === 'interval') && t.nextFireUtc) {
        const d = parseUtc(t.nextFireUtc);
        if (!d) continue;
        const iso = isoDate(d);
        if (iso >= firstIso && iso <= lastIso) {
          safePush(iso, { timer: t, kind: occKind });
        }
      } else if (occKind === 'weekly' || occKind === 'daily') {
        // Weekly fires land on its chosen weekdays; daily fires on every
        // day of the visible grid. Single chip per cell keeps the grid
        // readable; multiple weekly timers accumulate.
        const matchingDays = new Set();
        if (occKind === 'weekly') {
          // Push the visible grid into the same ISO-week convention the
          // timer uses. Each cell IS an ISO date, but the timer fires in
          // its own tz: we conservatively place on the matching ISO weekday
          // here, matching what the Week page shows for the same window.
          const dowSet = weeklyDaysFromOccurrence(occ);
          for (const cell of grid) {
            if (dowSet.has(jsIsoWeekday(cell))) {
              matchingDays.add(isoDate(cell));
            }
          }
        } else {
          for (const cell of grid) matchingDays.add(isoDate(cell));
        }
        for (const iso of matchingDays) {
          safePush(iso, { timer: t, kind: occKind });
        }
      }
    }
    daysMap = map;
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

  let monthLabel = $derived(
    new Date(year, month, 1).toLocaleString(undefined, { month: 'long', year: 'numeric' }),
  );
  let grid = $derived(monthGrid(year, month));
</script>

<section class="section-title">
  <span>Month</span>
  <span style="font-weight: 400; text-transform: none; letter-spacing: 0;">{monthLabel}</span>
</section>

<div class="month-toolbar">
  <button class="btn" onclick={() => shiftYear(-1)} aria-label="previous year">« Year</button>
  <button class="btn" onclick={() => shiftMonth(-1)} aria-label="previous month">◀ Month</button>
  <button class="btn" onclick={jumpToday}>Today</button>
  <button class="btn" onclick={() => shiftMonth(1)} aria-label="next month">Month ▶</button>
  <button class="btn" onclick={() => shiftYear(1)} aria-label="next year">Year »</button>
</div>

<div class="month-grid">
  {#each WEEKDAY_LABELS as label}
    <div class="month-head">{label}</div>
  {/each}
  {#each grid as d, i}
    {@const iso = isoDate(d)}
    {@const inMonth = d.getMonth() === month}
    {@const isToday = iso === isoDate(new Date())}
    {@const chips = daysMap[iso] || []}
    <div class="month-cell" class:out={!inMonth} class:today={isToday}>
      <div class="month-cell-head">
        <span class="day-num">{d.getDate()}</span>
        {#if jsIsoWeekday(d) === 1 || d.getDate() === 1}
          <span class="day-month">{d.toLocaleString(undefined, { month: 'short' })}</span>
        {/if}
      </div>
      {#if chips.length > 0}
        <div class="month-chip-wrap">
          {#each chips.slice(0, 3) as chip}
            <button class="month-chip kind-{chip.kind}" onclick={() => onEdit(chip.timer)} title={chip.timer.name}>
              {chip.timer.name}
            </button>
          {/each}
          {#if chips.length > 3}
            <div class="month-chip-more">+{chips.length - 3} more</div>
          {/if}
        </div>
      {/if}
    </div>
  {/each}
</div>
