<script>
  import {
    WEEKDAY_LABELS,
    WEEKDAY_FROM_KEY,
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

  // Pull weekly days out of the structured occurrence. chrono serializes
  // `Weekdays` as a bitmask object: { mon: true, wed: true, fri: true }
  // for each day set. Returns ISO weekday numbers (Mon=1..Sun=7).
  function weeklyDaysFromOccurrence(occ) {
    const days = occ && occ.days ? occ.days : {};
    const out = [];
    for (const k of Object.keys(days)) {
      if (days[k]) {
        const dow = WEEKDAY_FROM_KEY[k.toLowerCase()];
        if (dow) out.push(dow);
      }
    }
    return out.sort();
  }

  function timeStringFromOccurrence(occ) {
    return typeof occ.at === 'string' ? occ.at : '00:00:00';
  }

  // Fill `cells` with a per-weekday list of timer chips. Daily timers that
  // fire every day show up in every column; weekly timers only on their
  // weekdays; interval/cron show in their anchor weekday at their local time;
  // once/monthly/yearly show on the DOW of their next fire inside the
  // displayed week (so the user can see what's coming). Matches the
  // spec's "Week page — 7-column DOW grid showing weekly repeating timers".
  function rebuild(timers, anchor) {
    const weekStart = isoWeekStart(anchor);
    const weekEnd = addDays(weekStart, 7);
    const next = WEEKDAY_LABELS.map(() => []);
    for (const t of timers) {
      const occ = t.occurrence || {};
      const occKind = occ.occ || '';
      if (occKind === 'weekly') {
        const days = weeklyDaysFromOccurrence(occ);
        const time = timeStringFromOccurrence(occ);
        for (const dow of days) {
          if (dow >= 1 && dow <= 7) {
            next[dow - 1].push({ timer: t, localTime: time });
          }
        }
      } else if (occKind === 'daily') {
        const time = timeStringFromOccurrence(occ);
        for (let d = 0; d < 7; d++) {
          next[d].push({ timer: t, localTime: time });
        }
      } else if (occKind === 'interval') {
        // Show on today's column with the configured every_secs badge.
        const every = occ.everySecs || 60;
        const now = new Date();
        const dow = ((now.getDay() + 6) % 7) + 1;
        const mins = Math.floor(every / 60);
        const secs = every % 60;
        const badge = mins >= 1
          ? `every ${mins}m`
          : `every ${secs}s`;
        next[dow - 1].push({ timer: t, localTime: badge, intervalBadge: true });
      } else if (occKind === 'cron') {
        const now = new Date();
        const dow = ((now.getDay() + 6) % 7) + 1;
        next[dow - 1].push({
          timer: t,
          localTime: (occ.expr || 'cron').slice(0, 16),
          intervalBadge: true,
        });
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
