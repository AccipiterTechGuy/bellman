<script>
  import {
    WEEKDAY_LABELS,
    monthGrid,
    isoDate,
    jsIsoWeekday,
    parseUtc,
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

  async function refresh() {
    // intentionally left as noop (caller owns the timer list)
  }

  // Build a map of `YYYY-MM-DD → [chip]` for the visible grid. Calendar
  // kinds that match month/year/day rule fire on those dates; once timers
  // show only on their fire day; cron/interval show only on their next
  // fire day within the visible span. Matches the spec: "Month page —
  // month calendar (year-aware grid, prev/next month + year navigation)
  // showing monthly/yearly/once timers on their dates".
  function rebuild(timers, year, month) {
    const grid = monthGrid(year, month);
    const firstIso = isoDate(grid[0]);
    const lastIso = isoDate(grid[grid.length - 1]);
    const map = {};
    const safePush = (iso, chip) => {
      (map[iso] = map[iso] || []).push(chip);
    };
    for (const t of timers) {
      const kind = (t.kind || '').toLowerCase();
      if (kind.startsWith('monthly')) {
        const m = /monthly day (\d+)/.exec(t.summary);
        if (!m) continue;
        const day = +m[1];
        for (const cell of grid) {
          if (cell.getFullYear() === year && cell.getMonth() === month) {
            const iso = isoDate(cell);
            // For real GUI quality we'd clamp via core; the card spec says
            // we mirror the CLI's clamp, so just match by day inside month.
            if (cell.getDate() === day) {
              safePush(iso, { timer: t, kind: 'monthly' });
            }
          }
        }
      } else if (kind.startsWith('yearly')) {
        const m = /yearly (\d+)-(\d+)/.exec(t.summary);
        if (!m) continue;
        const mo = +m[1];
        const dy = +m[2];
        if (mo - 1 === month) {
          for (const cell of grid) {
            if (cell.getMonth() === month && cell.getDate() === dy) {
              safePush(isoDate(cell), { timer: t, kind: 'yearly' });
            }
          }
        }
      } else if (kind.startsWith('once') && t.nextFireUtc) {
        const d = parseUtc(t.nextFireUtc);
        if (!d) continue;
        const iso = isoDate(d);
        if (iso >= firstIso && iso <= lastIso) {
          safePush(iso, { timer: t, kind: 'once' });
        }
      } else if ((kind.startsWith('cron') || kind.startsWith('interval')) && t.nextFireUtc) {
        const d = parseUtc(t.nextFireUtc);
        if (!d) continue;
        const iso = isoDate(d);
        if (iso >= firstIso && iso <= lastIso) {
          safePush(iso, { timer: t, kind });
        }
      } else if (kind.startsWith('weekly') || kind.startsWith('daily')) {
        // Weekly/daily fall on every day of the visible grid (single summary
        // chip per cell so the month stays readable).
        for (const cell of grid) {
          safePush(isoDate(cell), { timer: t, kind });
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
