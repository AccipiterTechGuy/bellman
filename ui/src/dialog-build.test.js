// Exhaustive vitest coverage of the `TimerDialog.buildInput` wire contract.
// Imports the shared builder so refactors of the dialog form cannot
// accidentally regress the auditor-flagged fields (interval anchor,
// once.onceAt, Launch.workdir, system-local tz, the `isEdit` guard).
//
// The expected fields mirror `src-tauri/src/web.rs::WebOccurrenceDto` and
// `CreateTimerInput` exactly; a drift here surfaces a drift there.

import { describe, expect, it } from 'vitest';
import { buildInput } from './dialog-build.js';
import { parseOnceFields } from './datetime-input.js';

const emptyForm = {
  name: 'tick',
  enabled: true,
  occurrence: {
    kind: 'daily',
    tz: '',
    time: '09:00:00',
    onceAt: '',
    onceDate: '',
    onceTime: '',
    everySecs: 60,
    intervalAnchor: null,
    days: 'mon,wed,fri',
    day: 1,
    month: 1,
    cronExpr: '',
  },
  actionType: 'none',
  launchCommand: '',
  launchArgs: '',
  launchWorkdir: '',
  notifyTitle: '',
  notifyBody: '',
};

describe('TimerDialog buildInput — flat WebOccurrenceDto wire contract', () => {
  it('daily: blank tz resolves to system-local via Intl', () => {
    const out = buildInput({ ...emptyForm, name: 'morning' }, false);
    expect(out.occurrence.occ).toBe('daily');
    expect(out.occurrence.tz).toBe(Intl.DateTimeFormat().resolvedOptions().timeZone);
    expect(out.occurrence.tz).not.toBe('');
    expect(out.occurrence.at).toBe('09:00:00');
    expect(out.occurrence.days).toBeNull();
  });

  it('daily: explicit tz passes through unchanged', () => {
    const out = buildInput({ ...emptyForm, occurrence: { ...emptyForm.occurrence, tz: 'Europe/Helsinki' } }, false);
    expect(out.occurrence.tz).toBe('Europe/Helsinki');
  });

  it('once: onceAt round-trips, at null, everySecs null', () => {
    const out = buildInput(
      {
        ...emptyForm,
        occurrence: {
          ...emptyForm.occurrence,
          kind: 'once',
          onceAt: '2099-12-31T23:59:00',
          tz: 'UTC',
        },
      },
      true,
    );
    expect(out.occurrence.occ).toBe('once');
    expect(out.occurrence.onceAt).toBe('2099-12-31T23:59:00');
    expect(out.occurrence.at).toBeNull();
    expect(out.occurrence.everySecs).toBeNull();
    expect(out.occurrence.onceAt).not.toBeNull();
  });

  it('once: human date 24.12.2026 via overrides becomes wire ISO', () => {
    const parsed = parseOnceFields('24.12.2026', '09:00', 'Europe/Helsinki');
    expect(parsed.ok).toBe(true);
    const out = buildInput(
      {
        ...emptyForm,
        occurrence: {
          ...emptyForm.occurrence,
          kind: 'once',
          onceDate: '24.12.2026',
          onceTime: '09:00',
          tz: 'Europe/Helsinki',
        },
      },
      false,
      { onceAt: parsed.onceAt },
    );
    expect(out.occurrence.onceAt).toBe('2026-12-24T09:00:00');
  });

  it('weekly: CSV converts to a 7-key {mon..sun} map, days preserved', () => {
    const out = buildInput(
      {
        ...emptyForm,
        name: 'weekly-mwf',
        occurrence: { ...emptyForm.occurrence, kind: 'weekly', tz: 'Europe/Helsinki' },
      },
      false,
    );
    expect(out.occurrence.occ).toBe('weekly');
    expect(out.occurrence.days).toEqual({
      mon: true, tue: false, wed: true, thu: false, fri: true, sat: false, sun: false,
    });
    expect(out.occurrence.at).toBe('09:00:00');
    expect(out.occurrence.day).toBeNull();
    expect(out.occurrence.month).toBeNull();
  });

  it('interval edit+save: anchor preserved verbatim (rework #3 + #4)', () => {
    const editForm = {
      ...emptyForm,
      occurrence: {
        ...emptyForm.occurrence,
        kind: 'interval',
        everySecs: 60,
        intervalAnchor: '2026-06-01T12:00:00Z',
        tz: 'UTC',
      },
    };

    const newOut = buildInput(editForm, false);
    expect(newOut.occurrence.anchor).toBeNull();

    const editOut = buildInput(editForm, true);
    expect(editOut.occurrence.anchor).toBe('2026-06-01T12:00:00Z');
    expect(editOut.occurrence.everySecs).toBe(60);
  });

  it('monthly / yearly: day + month fields, at preserved', () => {
    const out = buildInput(
      {
        ...emptyForm,
        occurrence: {
          ...emptyForm.occurrence,
          kind: 'yearly',
          day: 29,
          month: 2,
          tz: 'UTC',
        },
      },
      false,
    );
    expect(out.occurrence.occ).toBe('yearly');
    expect(out.occurrence.day).toBe(29);
    expect(out.occurrence.month).toBe(2);
    expect(out.occurrence.at).toBe('09:00:00');
  });

  it('cron: expr round-trips, day/month null', () => {
    const out = buildInput(
      {
        ...emptyForm,
        occurrence: {
          ...emptyForm.occurrence,
          kind: 'cron',
          cronExpr: '*/5 * * * *',
          tz: 'UTC',
        },
      },
      false,
    );
    expect(out.occurrence.occ).toBe('cron');
    expect(out.occurrence.expr).toBe('*/5 * * * *');
    expect(out.occurrence.at).toBeNull();
    expect(out.occurrence.day).toBeNull();
    expect(out.occurrence.month).toBeNull();
  });

  it('launch action: command + args + workdir all preserved', () => {
    const out = buildInput(
      {
        ...emptyForm,
        actionType: 'launch',
        launchCommand: '/bin/echo',
        launchArgs: 'hello world',
        launchWorkdir: '/tmp',
      },
      false,
    );
    expect(out.action).toEqual({
      type: 'launch',
      command: '/bin/echo',
      args: ['hello', 'world'],
      workdir: '/tmp',
    });
  });

  it('launch action: empty workdir becomes null (Rust Option<...>)', () => {
    const out = buildInput(
      {
        ...emptyForm,
        actionType: 'launch',
        launchCommand: '/bin/sh',
        launchArgs: '',
        launchWorkdir: '',
      },
      false,
    );
    expect(out.action.workdir).toBeNull();
  });

  it('notify action: title + body', () => {
    const out = buildInput(
      {
        ...emptyForm,
        actionType: 'notify',
        notifyTitle: 'hello',
        notifyBody: 'world',
      },
      false,
    );
    expect(out.action).toEqual({ type: 'notify', title: 'hello', body: 'world' });
  });

  it('null actionType falls back to {type:"none"}', () => {
    const out = buildInput({ ...emptyForm, actionType: 'unknown' }, false);
    expect(out.action).toEqual({ type: 'none' });
  });

  it('name is trimmed + enabled honoured', () => {
    const out = buildInput({ ...emptyForm, name: '  trim-me  ', enabled: false }, false);
    expect(out.name).toBe('trim-me');
    expect(out.enabled).toBe(false);
  });

  it('daily time override pads HH:MM via overrides', () => {
    const out = buildInput(
      { ...emptyForm, occurrence: { ...emptyForm.occurrence, time: '09:00' } },
      false,
      { time: '09:00:00' },
    );
    expect(out.occurrence.at).toBe('09:00:00');
  });
});
