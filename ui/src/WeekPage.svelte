<script>
  import {
    WEEKDAY_LABELS,
    isoWeekStart,
    addDays,
    isoDate,
    clockToSeconds,
    parseUtc,
  } from './api.js';

  /** @type {(t:any) => void} */
  let { onEdit, onToast, timers = [], tick = 0 } = $props();

  let weekAnchor = $state(new Date());

  // ISO weekday → array of timed buckets in that column for the current week.
  let cells = $state(WEEKDAY_LABELS.map(() => []));

  $effect(() => {
    void tick;
    rebuild(timers, weekAnchor);
  });

  // Fill `cells` with a per-weekday list of timer chips. Daily timers that
  // fire every day show up in every column; weekly timers only on their
  // weekdays; interval/cron show in their anchor weekday at their local time;
  // once/monthly/yearly show in today's column if `nextFireUtc` falls inside
  // the displayed week (so the user can see what's coming). Matches the
  // spec's "Week page — 7-column DOW grid showing weekly repeating timers".
  function rebuild(timers, anchor) {
    const weekStart = isoWeekStart(anchor);
    const weekEnd = addDays(weekStart, 7);
    const next = WEEKDAY_LABELS.map(() => []);
    for (const t of timers) {
      const kind = (t.kind || '').toLowerCase();
      if (kind.startsWith('weekly')) {
        const days = parseWeeklyDaysFromSummary(t);
        const time = parseTimeFromSummary(t, 'weekly') || '00:00:00';
        for (const dow of days) {
          next[dow - 1].push({ timer: t, localTime: time });
        }
      } else if (kind.startsWith('daily')) {
        const time = parseTimeFromSummary(t, 'daily') || '00:00:00';
        for (let d = 0; d < 7; d++) {
          next[d].push({ timer: t, localTime: time });
        }
      } else if (kind.startsWith('interval') || kind.startsWith('cron')) {
        const offset = parseOffsetFromKind(kind);
        const now = new Date();
        const dailyAt = now.toLocaleTimeString();
        // Map the timer onto today's column using its DOW at "now".
        const dow = ((now.getDay() + 6) % 7) + 1; // ISO: Mon=1..Sun=7
        next[dow - 1].push({ timer: t, localTime: dailyAt, intervalBadge: true });
      } else if (t.nextFireUtc) {
        const d = parseUtc(t.nextFireUtc);
        if (d && d >= weekStart && d < weekEnd) {
          const dow = ((d.getDay() + 6) % 7) + 1;
          const hh = String(d.getHours()).padStart(2, '0');
          const mm = String(d.getMinutes()).padStart(2, '0');
          const ss = String(d.getSeconds()).padStart(2, '0');
          next[dow - 1].push({ timer: t, localTime: `${hh}:${mm}:${ss}` });
        }
      }
    }
    for (const col of next) {
      col.sort((a, b) => clockToSeconds(a.localTime) - clockToSeconds(b.localTime));
    }
    cells = next;
  }

  // Summary strings come from the CLI/Tauri TimerDto: "weekly mon,wed 09:30 UTC".
  function parseWeeklyDaysFromSummary(t) {
    const sum = t.summary || '';
    const m = /^weekly\s+([a-z,]+)\s/i.exec(sum);
    if (!m) return [];
    return m[1]
      .split(',')
      .map((s) => s.trim())
      .filter(Boolean)
      .map((name) => {
        switch (name) {
          case 'mon': return 1;
          case 'tue': return 2;
          case 'wed': return 3;
          case 'thu': return 4;
          case 'fri': return 5;
          case 'sat': return 6;
          case 'sun': return 7;
          default: return null;
        }
      })
      .filter(Boolean);
  }

  function parseTimeFromSummary(t, prefix) {
    const sum = t.summary || '';
    const re = new RegExp(`^${prefix}\\s+\\S+\\s+(\\d{2}:\\d{2}(?::\\d{2})?)\\s`, 'i');
    const m = re.exec(sum);
    return m ? m[1] : null;
  }

  function parseOffsetFromKind(kind) {
    return kind.startsWith('interval')
      ? /interval\s*\((\d+)s\)/.exec(kind)?.[1] || null
      : null;
  }

  function shiftWeek(delta) {
    const d = new Date(weekAnchor);
    d.setDate(d.getDate() + delta * 7);
    weekAnchor = d;
  }

  function jumpToday() {
    weekAnchor = new Date();
  }

  let weekLabel = $derived(`${isoDate(weekAnchor)} – ${isoDate(addDays(weekAnchor, 6))}`);
</script>

<section class="section-title">
  <span>Week</span>
  <span style="font-weight: 400; text-transform: none; letter-spacing: 0;">{weekLabel}</span>
</section>

<div class="week-toolbar">
  <button class="btn" onclick={() => shiftWeek(-1)} aria-label="previous week">◀ Prev</button>
  <button class="btn" onclick={jumpToday}>This week</button>
  <button class="btn" onclick={() => shiftWeek(1)} aria-label="next week">Next ▶</button>
</div>

{#if timers.length === 0}
  <div class="empty">
    <p>No timers yet.</p>
    <p>Create a weekly or daily timer from <em>All timers</em> and it will appear here.</p>
  </div>
{:else}
  <div class="week-grid">
    {#each WEEKDAY_LABELS as label, i}
      <div class="week-col">
        <header>{label}</header>
        {#if cells[i].length === 0}
          <div class="empty-cell">—</div>
        {:else}
          {#each cells[i] as chip}
            <button class="chip" class:interval={chip.intervalBadge} onclick={() => onEdit(chip.timer)}>
              <div class="chip-time">{chip.localTime}</div>
              <div class="chip-name" title={chip.timer.name}>{chip.timer.name}</div>
              <div class="chip-summary">{chip.timer.summary}</div>
            </button>
          {/each}
        {/if}
      </div>
    {/each}
  </div>
{/if}
