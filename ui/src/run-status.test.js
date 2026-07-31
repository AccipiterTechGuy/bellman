// IK5 exit-gate arithmetic, asserted at both thresholds and both anchors.
import { describe, it, expect } from 'vitest';
import {
  isNonTerminal,
  isOverdue,
  elapsedSecs,
  formatElapsed,
  runRowDisplay,
  eventKindDisplay,
} from './run-status.js';

const T = Date.parse('2026-07-31T05:00:00Z');

function run(overrides = {}) {
  return {
    timerId: 't-1',
    timerName: 'bulb-test',
    runId: 'r-1',
    state: 'running',
    appName: 'lightbulb',
    firedAt: new Date(T).toISOString(),
    ...overrides,
  };
}

const at = (secs) => T + secs * 1000;

describe('isNonTerminal', () => {
  it('treats the open states as non-terminal only', () => {
    for (const s of ['fired', 'fired_late', 'acknowledged', 'running']) {
      expect(isNonTerminal(s)).toBe(true);
    }
    for (const s of ['completed', 'failed', 'no_ack', 'cancelled', 'superseded']) {
      expect(isNonTerminal(s)).toBe(false);
    }
  });
});

describe('overdue — 1× the estimate, anchored on fired_at', () => {
  it('fires at 1× expected_secs, not before, while still running', () => {
    const r = run({ expectedSecs: 900 });
    expect(isOverdue(r, at(899))).toBe(false);
    expect(isOverdue(r, at(900))).toBe(false); // strictly greater
    expect(isOverdue(r, at(901))).toBe(true);
    // …and the run is still `running`: the label never changes the state.
    expect(r.state).toBe('running');
    expect(runRowDisplay(r, at(901)).text).toContain('running');
  });

  it('anchors on fired_at: acked at T+5m, expected 900 → overdue at T+15m, NOT T+20m', () => {
    const r = run({ expectedSecs: 900, acknowledgedAt: new Date(at(300)).toISOString() });
    expect(isOverdue(r, at(901))).toBe(true);
    expect(isOverdue(r, at(1199))).toBe(true); // would still be false if anchored on ack
  });

  it('a heartbeat does NOT move the label (it only rearms an opted-in watchdog)', () => {
    const r = run({ expectedSecs: 900, heartbeatAt: new Date(at(840)).toISOString() });
    expect(isOverdue(r, at(901))).toBe(true);
  });

  it('a re-sent expected_secs replaces the old one (backend folds in the latest)', () => {
    const r = run({ expectedSecs: 3600 }); // the DTO already carries the new value
    expect(isOverdue(r, at(901))).toBe(false);
    expect(isOverdue(r, at(3601))).toBe(true);
  });

  it('never fires without expected_secs, and never on a terminal run', () => {
    expect(isOverdue(run({}), at(999999))).toBe(false);
    expect(isOverdue(run({ expectedSecs: 0 }), at(999999))).toBe(false);
    expect(isOverdue(run({ state: 'completed', expectedSecs: 1 }), at(999999))).toBe(false);
    expect(isOverdue(run({ state: 'failed', expectedSecs: 1 }), at(999999))).toBe(false);
    expect(isOverdue(run({ state: 'no_ack', expectedSecs: 1 }), at(999999))).toBe(false);
  });
});

describe('elapsed + formatting', () => {
  it('measures from fired_at, clamped at zero', () => {
    const r = run({});
    expect(elapsedSecs(r, at(7))).toBe(7);
    expect(elapsedSecs(r, at(-5))).toBe(0);
  });

  it('formats compact durations', () => {
    expect(formatElapsed(7)).toBe('7s');
    expect(formatElapsed(59)).toBe('59s');
    expect(formatElapsed(60)).toBe('1m');
    expect(formatElapsed(74 * 60)).toBe('74m');
    expect(formatElapsed(2 * 3600 + 5 * 60)).toBe('125m');
    expect(formatElapsed(3 * 86400 + 4 * 3600)).toBe('3d 4h');
  });
});

describe('runRowDisplay — the All-timers row / pinned live entry', () => {
  it('running with NO heartbeat and NO progress shows state + elapsed and nothing else', () => {
    const d = runRowDisplay(run({}), at(7));
    expect(d.dot).toBe('●');
    expect(d.tone).toBe('live');
    expect(d.text).toBe('running · 7s');
    // Absence is not a state: no placeholder text anywhere.
    expect(d.text).not.toContain('never');
    expect(d.text).not.toContain('—');
    expect(d.text).not.toContain('heartbeat');
    expect(d.text).not.toContain('expected');
  });

  it('renders progress when the app sends it', () => {
    const d = runRowDisplay(run({ progress: 'bulb on, 7s elapsed' }), at(7));
    expect(d.text).toBe('running · 7s · bulb on, 7s elapsed');
  });

  it('adds the overdue suffix at 1× — with the estimate, from fired_at', () => {
    const d = runRowDisplay(run({ expectedSecs: 600 }), at(74 * 60));
    expect(d.tone).toBe('warn');
    expect(d.text).toBe('running · 74m · overdue (expected ~10m)');
  });

  it('shows each terminal state distinctly', () => {
    const completed = runRowDisplay(run({ state: 'completed' }), at(15));
    const failedReported = runRowDisplay(
      run({ state: 'failed', failureKind: 'reported', reason: 'GPIO write refused' }),
      at(15),
    );
    const failedTimeout = runRowDisplay(
      run({ state: 'failed', failureKind: 'timed_out' }),
      at(15),
    );
    const noAck = runRowDisplay(run({ state: 'no_ack' }), at(60));

    const texts = [completed.text, failedReported.text, failedTimeout.text, noAck.text];
    expect(new Set(texts).size).toBe(4); // all visually distinguishable
    expect(completed).toMatchObject({ dot: '✓', tone: 'ok', text: 'completed' });
    expect(failedReported.text).toBe('failed · reported');
    expect(failedTimeout.text).toBe('failed · timed out');
    expect(noAck.text).toBe('no ack');
  });

  it('returns null when there is no run to show', () => {
    expect(runRowDisplay(null, at(1))).toBeNull();
    expect(runRowDisplay({}, at(1))).toBeNull();
  });
});

describe('eventKindDisplay — Run history terminal distinction', () => {
  it('tells reported from timed_out from no_ack from superseded', () => {
    expect(eventKindDisplay({ kind: 'failed', detail: { failure_kind: 'reported' } }))
      .toMatchObject({ label: 'failed · reported', cls: 'err' });
    expect(eventKindDisplay({ kind: 'failed', detail: { failure_kind: 'timed_out' } }))
      .toMatchObject({ label: 'failed · timed out', cls: 'err' });
    expect(eventKindDisplay({ kind: 'no_ack' }))
      .toMatchObject({ label: 'no ack', cls: 'err' });
    expect(eventKindDisplay({ kind: 'superseded' }))
      .toMatchObject({ label: 'superseded', cls: 'warn' });
    expect(eventKindDisplay({ kind: 'completed' }))
      .toMatchObject({ label: 'completed', cls: 'ok' });
    expect(eventKindDisplay({ kind: 'fired' })).toBeNull(); // caller fallback
  });
});
