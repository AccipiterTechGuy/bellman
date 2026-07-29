/**
 * Calendar truth helpers for Week / Month views.
 *
 * Past cells never show fabricated recurrence. Prefer the backend
 * `list_calendar_truth` payload; these pure functions organise entries and
 * provide a client-side fallback that still refuses to project into the past
 * when Tauri is unavailable (vite dev / fixtures).
 */

import {
  addDays,
  clockToSeconds,
  daysInMonth,
  isoDate,
  isoWeekStart,
  jsIsoWeekday,
  kindFromOccurrence,
  parseUtc,
  weeklyDaysFromOccurrence,
} from './api.js';

/** Accessible source label. */
export function sourceLabel(source) {
  if (source === 'recorded') return 'Recorded';
  if (source === 'upcoming') return 'Upcoming';
  return source || '';
}

/** Human outcome label for chip badges. */
export function outcomeLabel(outcome) {
  switch (outcome) {
    case 'delivered': return 'delivered';
    case 'failed': return 'failed';
    case 'skipped': return 'skipped';
    case 'late': return 'late';
    case 'coalesced': return 'coalesced';
    case 'upcoming': return 'upcoming';
    default: return outcome || '';
  }
}

/**
 * Group truth entries by local civil date `YYYY-MM-DD`.
 * @param {Array<{date: string}>} entries
 * @returns {Record<string, Array>}
 */
export function groupEntriesByDate(entries) {
  const map = {};
  for (const e of entries || []) {
    if (!e || !e.date) continue;
    (map[e.date] = map[e.date] || []).push(e);
  }
  for (const k of Object.keys(map)) {
    map[k].sort((a, b) => {
      const ta = a.timeSecs != null ? a.timeSecs : clockToSeconds(a.time || '00:00:00');
      const tb = b.timeSecs != null ? b.timeSecs : clockToSeconds(b.time || '00:00:00');
      if (ta !== tb) return ta - tb;
      return String(a.name || '').localeCompare(String(b.name || ''));
    });
  }
  return map;
}

/**
 * Bucket entries into ISO weekday columns (Mon=0 … Sun=6) for a week.
 * @param {Array} entries
 * @param {Date} weekAnchor any day inside the ISO week
 */
export function groupEntriesByWeekday(entries, weekAnchor) {
  const weekStart = isoWeekStart(weekAnchor);
  const cols = [[], [], [], [], [], [], []];
  const startIso = isoDate(weekStart);
  const end = addDays(weekStart, 7);
  const endIso = isoDate(end);
  for (const e of entries || []) {
    if (!e?.date || e.date < startIso || e.date >= endIso) continue;
    // Parse civil date as local midnight.
    const [y, m, d] = e.date.split('-').map(Number);
    const cell = new Date(y, m - 1, d);
    const col = jsIsoWeekday(cell) - 1;
    cols[col].push(e);
  }
  for (const col of cols) {
    col.sort((a, b) => {
      const ta = a.timeSecs != null ? a.timeSecs : clockToSeconds(a.time || '00:00:00');
      const tb = b.timeSecs != null ? b.timeSecs : clockToSeconds(b.time || '00:00:00');
      if (ta !== tb) return ta - tb;
      return String(a.name || '').localeCompare(String(b.name || ''));
    });
  }
  return cols;
}

/**
 * Build a reconciliation key for duplicate suppression.
 * timerId + scheduled second (ms truncated to second).
 */
export function dedupeKey(timerId, scheduledForIso) {
  if (!timerId || !scheduledForIso) return '';
  const t = Date.parse(scheduledForIso);
  if (Number.isNaN(t)) return `${timerId}|${scheduledForIso}`;
  return `${timerId}|${Math.floor(t / 1000)}`;
}

/**
 * Pure client-side truth merge used when the backend command is unavailable
 * or for unit tests. Mirrors the core rules:
 *  - past (< now): only `events` / recorded
 *  - future (> now): project from timers; suppress duplicates of recorded
 *  - never fabricate past recurrence from the schedule alone
 *
 * @param {{
 *   timers: any[],
 *   events: any[],
 *   from: string,  // YYYY-MM-DD
 *   to: string,
 *   now?: Date,
 * }} args
 * @returns {Array}
 */
export function buildClientTruthEntries({ timers = [], events = [], from, to, now = new Date() }) {
  const nowMs = now.getTime();
  const entries = [];
  const seen = new Set();

  // ── Recorded from events ──
  for (const ev of events || []) {
    const kind = ev.kind || '';
    if (!isOutcomeKind(kind)) continue;
    const whenIso = ev.scheduled_for || ev.scheduledFor || ev.ts;
    const when = parseUtc(whenIso);
    if (!when) continue;
    if (when.getTime() >= nowMs) continue;
    const date = isoDate(when);
    if (date < from || date > to) continue;
    const timerId = ev.timer_id || ev.timerId || null;
    const runId = ev.run_id || ev.runId || null;
    const key = runId
      ? `run:${runId}`
      : dedupeKey(timerId, when.toISOString());
    if (seen.has(key)) {
      // Merge outcome priority into existing entry.
      const existing = entries.find((e) => e._key === key);
      if (existing) {
        existing.outcome = mergeOutcome(existing.outcome, kindToOutcome(kind));
      }
      continue;
    }
    seen.add(key);
    if (timerId) seen.add(dedupeKey(timerId, when.toISOString()));
    const name = ev.timer_name || ev.timerName || fallbackName(timerId, timers);
    entries.push({
      _key: key,
      timerId,
      runId,
      name,
      scheduledFor: when.toISOString(),
      date,
      time: formatLocalTime(when),
      timeSecs: when.getHours() * 3600 + when.getMinutes() * 60 + when.getSeconds(),
      source: 'recorded',
      outcome: kindToOutcome(kind),
      kind: null,
      enabled: null,
    });
  }

  // ── Upcoming projections (strictly after now) ──
  for (const t of timers || []) {
    const projected = projectTimerInRange(t, from, to, now);
    for (const p of projected) {
      const key = dedupeKey(t.id, p.scheduledFor);
      if (key && seen.has(key)) continue;
      // Also check second-level against recorded.
      if (key && [...seen].some((s) => s === key)) continue;
      if (timerHasRecordedAt(seen, t.id, p.scheduledFor)) continue;
      entries.push({
        timerId: t.id,
        runId: null,
        name: t.name,
        scheduledFor: p.scheduledFor,
        date: p.date,
        time: p.time,
        timeSecs: p.timeSecs,
        source: 'upcoming',
        outcome: 'upcoming',
        kind: kindFromOccurrence(t.occurrence || {}),
        enabled: t.enabled,
      });
    }
  }

  entries.sort((a, b) => {
    if (a.date !== b.date) return a.date < b.date ? -1 : 1;
    if (a.timeSecs !== b.timeSecs) return a.timeSecs - b.timeSecs;
    return String(a.name).localeCompare(String(b.name));
  });
  // Strip internal keys.
  return entries.map(({ _key, ...rest }) => rest);
}

function timerHasRecordedAt(seen, timerId, scheduledForIso) {
  const k = dedupeKey(timerId, scheduledForIso);
  return k && seen.has(k);
}

function isOutcomeKind(kind) {
  return [
    'fired', 'fired_late', 'skipped_misfire', 'coalesced',
    'wake_delivered', 'wake_failed', 'no_ack',
  ].includes(kind);
}

function kindToOutcome(kind) {
  switch (kind) {
    case 'wake_failed':
    case 'no_ack':
      return 'failed';
    case 'skipped_misfire':
      return 'skipped';
    case 'coalesced':
      return 'coalesced';
    case 'fired_late':
      return 'late';
    case 'fired':
    case 'wake_delivered':
    default:
      return 'delivered';
  }
}

const OUTCOME_RANK = {
  failed: 5,
  skipped: 4,
  coalesced: 3,
  late: 2,
  delivered: 1,
  upcoming: 0,
};

function mergeOutcome(a, b) {
  return (OUTCOME_RANK[b] || 0) >= (OUTCOME_RANK[a] || 0) ? b : a;
}

function fallbackName(timerId, timers) {
  if (!timerId) return '(unknown)';
  const t = (timers || []).find((x) => x.id === timerId);
  if (t) return t.name;
  return String(timerId).slice(0, 8) + '…';
}

function formatLocalTime(d) {
  return [d.getHours(), d.getMinutes(), d.getSeconds()]
    .map((n) => String(n).padStart(2, '0'))
    .join(':');
}

/**
 * Simplified future projection matching the previous Week/Month expansion,
 * but only for instants strictly after `now` inside [from, to].
 */
function projectTimerInRange(timer, from, to, now) {
  const occ = timer.occurrence || {};
  const occKind = kindFromOccurrence(occ);
  const out = [];
  const nowMs = now.getTime();

  const pushIfFuture = (dateObj, timeStr) => {
    const iso = isoDate(dateObj);
    if (iso < from || iso > to) return;
    const [hh, mm, ss] = (timeStr || '00:00:00').split(':').map((x) => Number(x) || 0);
    const local = new Date(
      dateObj.getFullYear(),
      dateObj.getMonth(),
      dateObj.getDate(),
      hh,
      mm,
      ss || 0,
    );
    if (local.getTime() <= nowMs) return;
    out.push({
      date: iso,
      time: formatLocalTime(local),
      timeSecs: hh * 3600 + mm * 60 + (ss || 0),
      scheduledFor: local.toISOString(),
    });
  };

  const timeStr = typeof occ.at === 'string' ? occ.at : '00:00:00';

  // Iterate civil days in range.
  const [fy, fm, fd] = from.split('-').map(Number);
  const [ty, tm, td] = to.split('-').map(Number);
  let cursor = new Date(fy, fm - 1, fd);
  const end = new Date(ty, tm - 1, td);

  if (occKind === 'daily') {
    while (cursor <= end) {
      pushIfFuture(cursor, timeStr);
      cursor = addDays(cursor, 1);
    }
  } else if (occKind === 'weekly') {
    const days = new Set(weeklyDaysFromOccurrence(occ));
    while (cursor <= end) {
      if (days.has(jsIsoWeekday(cursor))) pushIfFuture(cursor, timeStr);
      cursor = addDays(cursor, 1);
    }
  } else if (occKind === 'monthly') {
    const day = Number(occ.day || 0);
    if (day) {
      while (cursor <= end) {
        const y = cursor.getFullYear();
        const m = cursor.getMonth();
        const clamped = Math.min(day, daysInMonth(y, m));
        if (cursor.getDate() === clamped) pushIfFuture(cursor, timeStr);
        cursor = addDays(cursor, 1);
      }
    }
  } else if (occKind === 'yearly') {
    const mo = Number(occ.month || 0);
    const day = Number(occ.day || 0);
    if (mo && day) {
      while (cursor <= end) {
        if (cursor.getMonth() + 1 === mo) {
          const clamped = Math.min(day, daysInMonth(cursor.getFullYear(), cursor.getMonth()));
          if (cursor.getDate() === clamped) pushIfFuture(cursor, timeStr);
        }
        cursor = addDays(cursor, 1);
      }
    }
  } else if (timer.nextFireUtc) {
    const d = parseUtc(timer.nextFireUtc);
    if (d && d.getTime() > nowMs) {
      const iso = isoDate(d);
      if (iso >= from && iso <= to) {
        out.push({
          date: iso,
          time: formatLocalTime(d),
          timeSecs: d.getHours() * 3600 + d.getMinutes() * 60 + d.getSeconds(),
          scheduledFor: d.toISOString(),
        });
      }
    }
  }
  return out;
}

/**
 * Normalise a backend TruthWindow entry (camelCase) for the pages.
 */
export function normaliseTruthEntry(e) {
  if (!e) return null;
  return {
    timerId: e.timerId ?? e.timer_id ?? null,
    runId: e.runId ?? e.run_id ?? null,
    name: e.name,
    scheduledFor: e.scheduledFor ?? e.scheduled_for,
    date: e.date,
    time: e.time,
    timeSecs: e.timeSecs ?? e.time_secs ?? clockToSeconds(e.time || '00:00:00'),
    source: e.source,
    outcome: e.outcome,
    kind: e.kind ?? null,
    enabled: e.enabled ?? null,
  };
}

export function normaliseTruthWindow(win) {
  if (!win) return { from: '', to: '', entries: [], warnings: [] };
  return {
    from: win.from,
    to: win.to,
    timezone: win.timezone,
    nowUtc: win.nowUtc ?? win.now_utc,
    entries: (win.entries || []).map(normaliseTruthEntry).filter(Boolean),
    warnings: win.warnings || [],
  };
}
