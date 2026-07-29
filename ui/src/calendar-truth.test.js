/**
 * Calendar truth model unit tests — pure JS merge + grouping.
 * Mirrors core acceptance cases so UI never fabricates past recurrence.
 */
import { describe, expect, it } from 'vitest';
import {
  buildClientTruthEntries,
  dedupeKey,
  groupEntriesByDate,
  groupEntriesByWeekday,
  normaliseTruthEntry,
  normaliseTruthWindow,
  outcomeLabel,
  sourceLabel,
} from './calendar-truth.js';
import { isoDate, isoWeekStart, addDays } from './api.js';

function dailyTimer(id, name, at = '09:00:00') {
  return {
    id,
    name,
    enabled: true,
    occurrence: { occ: 'daily', at, tz: 'UTC', days: null },
  };
}

describe('source / outcome labels', () => {
  it('exposes accessible Recorded vs Upcoming labels', () => {
    expect(sourceLabel('recorded')).toBe('Recorded');
    expect(sourceLabel('upcoming')).toBe('Upcoming');
    expect(outcomeLabel('delivered')).toBe('delivered');
    expect(outcomeLabel('failed')).toBe('failed');
    expect(outcomeLabel('skipped')).toBe('skipped');
    expect(outcomeLabel('late')).toBe('late');
    expect(outcomeLabel('coalesced')).toBe('coalesced');
  });
});

describe('buildClientTruthEntries', () => {
  it('empty past history shows nothing (no fabricated recurrence)', () => {
    const now = new Date(2026, 6, 29, 12, 0, 0); // local Jul 29
    const t = dailyTimer('t1', 'morning');
    // Past week range relative to now.
    const from = '2026-07-20';
    const to = '2026-07-26';
    const entries = buildClientTruthEntries({ timers: [t], events: [], from, to, now });
    expect(entries).toEqual([]);
  });

  it('event + claim same run collapse (no delivered twin)', () => {
    const now = new Date(2026, 6, 29, 12, 0, 0);
    const tid = 'aaaaaaaa-aaaa-bbbb-bbbb-cccccccccccc';
    const runId = '11111111-2222-3333-4444-555555555555';
    const sched = new Date(2026, 6, 28, 15, 18, 11).toISOString();
    // Client merge only has events (claims are core-side). Two outcome events
    // for the same run_id must still collapse to one failed outcome.
    const events = [
      {
        kind: 'fired',
        timer_id: tid,
        timer_name: 'audit-duplicate',
        run_id: runId,
        scheduled_for: sched,
        ts: sched,
      },
      {
        kind: 'wake_failed',
        timer_id: tid,
        timer_name: 'audit-duplicate',
        run_id: runId,
        scheduled_for: sched,
        ts: sched,
      },
    ];
    const entries = buildClientTruthEntries({
      timers: [dailyTimer(tid, 'audit-duplicate')],
      events,
      from: '2026-07-27',
      to: '2026-07-29',
      now,
    });
    const rec = entries.filter((e) => e.source === 'recorded');
    expect(rec).toHaveLength(1);
    expect(rec[0].outcome).toBe('failed');
    expect(rec[0].name).toBe('audit-duplicate');
  });

  it('successful and failed recorded runs keep historical names', () => {
    const now = new Date(2026, 6, 29, 12, 0, 0);
    const tid = 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb';
    const events = [
      {
        kind: 'fired',
        timer_id: tid,
        timer_name: 'Old Name',
        run_id: 'r1',
        scheduled_for: new Date(2026, 6, 28, 9, 0, 0).toISOString(),
        ts: new Date(2026, 6, 28, 9, 0, 0).toISOString(),
      },
      {
        kind: 'wake_failed',
        timer_id: tid,
        timer_name: 'Old Name',
        run_id: 'r2',
        scheduled_for: new Date(2026, 6, 27, 9, 0, 0).toISOString(),
        ts: new Date(2026, 6, 27, 9, 0, 1).toISOString(),
      },
    ];
    const timers = [dailyTimer(tid, 'New Name')];
    const entries = buildClientTruthEntries({
      timers,
      events,
      from: '2026-07-27',
      to: '2026-07-29',
      now,
    });
    const rec = entries.filter((e) => e.source === 'recorded');
    expect(rec).toHaveLength(2);
    expect(rec.every((e) => e.name === 'Old Name')).toBe(true);
    expect(rec.some((e) => e.outcome === 'failed')).toBe(true);
    expect(rec.some((e) => e.outcome === 'delivered')).toBe(true);
  });

  it('pruned history does not paint other past days', () => {
    const now = new Date(2026, 6, 29, 12, 0, 0);
    const tid = 'cccccccc-cccc-cccc-cccc-cccccccccccc';
    const events = [
      {
        kind: 'fired',
        timer_id: tid,
        timer_name: 'daily',
        run_id: 'r1',
        scheduled_for: new Date(2026, 6, 25, 8, 0, 0).toISOString(),
        ts: new Date(2026, 6, 25, 8, 0, 0).toISOString(),
      },
    ];
    const entries = buildClientTruthEntries({
      timers: [dailyTimer(tid, 'daily', '08:00:00')],
      events,
      from: '2026-07-20',
      to: '2026-07-28',
      now,
    });
    const rec = entries.filter((e) => e.source === 'recorded');
    expect(rec).toHaveLength(1);
    expect(rec[0].date).toBe('2026-07-25');
    // No upcoming in the past range (all days <= 28 are <= today side of range before now for most hours).
    const upPast = entries.filter((e) => e.source === 'upcoming' && e.date <= '2026-07-28');
    // Client projects only after now; now is Jul 29 so upPast should be empty.
    expect(upPast).toHaveLength(0);
  });

  it('duplicate suppression: recorded hides same-second projection', () => {
    const now = new Date(2026, 6, 29, 8, 30, 0);
    const tid = 'dddddddd-dddd-dddd-dddd-dddddddddddd';
    const sched = new Date(2026, 6, 29, 8, 0, 0);
    const events = [
      {
        kind: 'fired_late',
        timer_id: tid,
        timer_name: 'late-one',
        run_id: 'r1',
        scheduled_for: sched.toISOString(),
        ts: new Date(2026, 6, 29, 8, 20, 0).toISOString(),
      },
    ];
    const entries = buildClientTruthEntries({
      timers: [dailyTimer(tid, 'late-one', '08:00:00')],
      events,
      from: '2026-07-29',
      to: '2026-07-29',
      now,
    });
    const at8 = entries.filter((e) => e.time.startsWith('08:00'));
    expect(at8).toHaveLength(1);
    expect(at8[0].source).toBe('recorded');
    expect(at8[0].outcome).toBe('late');
  });

  it('current-day past/future split', () => {
    const now = new Date(2026, 6, 29, 12, 0, 0);
    const tid = 'eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee';
    const morning = new Date(2026, 6, 29, 9, 0, 0);
    const events = [
      {
        kind: 'fired',
        timer_id: tid,
        timer_name: 'split',
        run_id: 'r1',
        scheduled_for: morning.toISOString(),
        ts: morning.toISOString(),
      },
    ];
    const entries = buildClientTruthEntries({
      timers: [dailyTimer(tid, 'split', '09:00:00')],
      events,
      from: '2026-07-29',
      to: '2026-07-30',
      now,
    });
    const todayRec = entries.filter((e) => e.date === '2026-07-29' && e.source === 'recorded');
    expect(todayRec).toHaveLength(1);
    const todayUp = entries.filter((e) => e.date === '2026-07-29' && e.source === 'upcoming');
    expect(todayUp).toHaveLength(0);
    const tom = entries.filter((e) => e.date === '2026-07-30' && e.source === 'upcoming');
    expect(tom).toHaveLength(1);
  });

  it('edited recurrence: past keeps old time, future uses new', () => {
    const now = new Date(2026, 6, 29, 12, 0, 0);
    const tid = 'ffffffff-ffff-ffff-ffff-ffffffffffff';
    const morning = new Date(2026, 6, 28, 9, 0, 0);
    const events = [
      {
        kind: 'fired',
        timer_id: tid,
        timer_name: 'edited',
        run_id: 'r1',
        scheduled_for: morning.toISOString(),
        ts: morning.toISOString(),
      },
    ];
    const entries = buildClientTruthEntries({
      timers: [dailyTimer(tid, 'edited', '15:00:00')],
      events,
      from: '2026-07-28',
      to: '2026-07-30',
      now,
    });
    const past = entries.find((e) => e.source === 'recorded');
    expect(past.time.startsWith('09:00')).toBe(true);
    const future = entries.filter((e) => e.source === 'upcoming');
    expect(future.length).toBeGreaterThan(0);
    expect(future.every((e) => e.time.startsWith('15:00'))).toBe(true);
  });

  it('deleted timer history retains recorded name without live timer', () => {
    const now = new Date(2026, 6, 29, 12, 0, 0);
    const events = [
      {
        kind: 'coalesced',
        timer_id: '12345678-1234-1234-1234-123456789abc',
        timer_name: 'gone-timer',
        run_id: 'r1',
        scheduled_for: new Date(2026, 6, 22, 7, 30, 0).toISOString(),
        ts: new Date(2026, 6, 22, 7, 30, 0).toISOString(),
      },
    ];
    const entries = buildClientTruthEntries({
      timers: [],
      events,
      from: '2026-07-20',
      to: '2026-07-28',
      now,
    });
    expect(entries).toHaveLength(1);
    expect(entries[0].name).toBe('gone-timer');
    expect(entries[0].outcome).toBe('coalesced');
    expect(entries[0].source).toBe('recorded');
  });

  it('browse past / current / future week shapes', () => {
    const now = new Date(2026, 6, 29, 12, 0, 0);
    const tid = '99999999-9999-9999-9999-999999999999';
    const t = dailyTimer(tid, 'browse', '11:00:00');

    const past = buildClientTruthEntries({
      timers: [t],
      events: [],
      from: '2026-07-20',
      to: '2026-07-26',
      now,
    });
    expect(past).toEqual([]);

    const events = [
      {
        kind: 'skipped_misfire',
        timer_id: tid,
        timer_name: 'browse',
        run_id: 'r1',
        scheduled_for: new Date(2026, 6, 28, 11, 0, 0).toISOString(),
        ts: new Date(2026, 6, 28, 11, 0, 0).toISOString(),
      },
    ];
    const cur = buildClientTruthEntries({
      timers: [t],
      events,
      from: '2026-07-27',
      to: '2026-08-02',
      now,
    });
    expect(cur.some((e) => e.source === 'recorded' && e.outcome === 'skipped')).toBe(true);
    expect(cur.filter((e) => e.source === 'upcoming').every((e) => Date.parse(e.scheduledFor) > now.getTime())).toBe(true);

    const fut = buildClientTruthEntries({
      timers: [t],
      events: [],
      from: '2026-08-03',
      to: '2026-08-09',
      now,
    });
    expect(fut.length).toBeGreaterThan(0);
    expect(fut.every((e) => e.source === 'upcoming')).toBe(true);
  });
});

describe('grouping', () => {
  it('groupEntriesByDate sorts by time', () => {
    const map = groupEntriesByDate([
      { date: '2026-07-29', time: '15:00:00', timeSecs: 54000, name: 'b' },
      { date: '2026-07-29', time: '09:00:00', timeSecs: 32400, name: 'a' },
      { date: '2026-07-30', time: '09:00:00', timeSecs: 32400, name: 'c' },
    ]);
    expect(map['2026-07-29']).toHaveLength(2);
    expect(map['2026-07-29'][0].name).toBe('a');
    expect(map['2026-07-30']).toHaveLength(1);
  });

  it('groupEntriesByWeekday places Mon–Sun columns for ISO week', () => {
    // Week of 2026-07-27 (Mon) … 2026-08-02 (Sun)
    const anchor = new Date(2026, 6, 29);
    const mon = isoDate(isoWeekStart(anchor));
    expect(mon).toBe('2026-07-27');
    const cols = groupEntriesByWeekday(
      [
        { date: '2026-07-27', time: '09:00:00', timeSecs: 32400, name: 'mon' },
        { date: '2026-07-29', time: '10:00:00', timeSecs: 36000, name: 'wed' },
        { date: '2026-08-02', time: '11:00:00', timeSecs: 39600, name: 'sun' },
      ],
      anchor,
    );
    expect(cols[0].map((e) => e.name)).toEqual(['mon']);
    expect(cols[2].map((e) => e.name)).toEqual(['wed']);
    expect(cols[6].map((e) => e.name)).toEqual(['sun']);
  });
});

describe('normaliseTruthWindow', () => {
  it('maps camelCase backend payload', () => {
    const win = normaliseTruthWindow({
      from: '2026-07-27',
      to: '2026-08-02',
      timezone: 'UTC',
      nowUtc: '2026-07-29T12:00:00Z',
      entries: [
        {
          timerId: 't1',
          name: 'x',
          scheduledFor: '2026-07-30T09:00:00Z',
          date: '2026-07-30',
          time: '09:00:00',
          timeSecs: 32400,
          source: 'upcoming',
          outcome: 'upcoming',
        },
      ],
    });
    expect(win.entries).toHaveLength(1);
    expect(win.entries[0].source).toBe('upcoming');
    expect(sourceLabel(win.entries[0].source)).toBe('Upcoming');
  });

  it('normaliseTruthEntry tolerates snake_case', () => {
    const e = normaliseTruthEntry({
      timer_id: 't1',
      run_id: 'r1',
      name: 'n',
      scheduled_for: '2026-07-28T09:00:00Z',
      date: '2026-07-28',
      time: '09:00:00',
      time_secs: 32400,
      source: 'recorded',
      outcome: 'delivered',
    });
    expect(e.timerId).toBe('t1');
    expect(e.runId).toBe('r1');
    expect(e.timeSecs).toBe(32400);
  });
});

describe('dedupeKey', () => {
  it('keys by timer + scheduled second', () => {
    const a = dedupeKey('t1', '2026-07-29T09:00:00.123Z');
    const b = dedupeKey('t1', '2026-07-29T09:00:00.999Z');
    expect(a).toBe(b);
    expect(dedupeKey('t2', '2026-07-29T09:00:00.123Z')).not.toBe(a);
  });
});
