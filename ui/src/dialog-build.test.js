// Exhaustive vitest coverage of the `TimerDialog.buildInput` wire contract.
// Re-implements the function inline so the .svelte runtime dependency
// (which vitest cannot import directly) is avoided — and so future
// refactors of the dialog form don't accidentally regress the
// auditor-flagged fields (interval anchor, once.onceAt, Launch.workdir,
// system-local tz, the `isEdit` guard).
//
// The expected fields mirror `src-tauri/src/web.rs::WebOccurrenceDto` and
// `CreateTimerInput` exactly; a drift here surfaces a drift there.

import { describe, expect, it } from 'vitest';

function buildInput(form, isEdit) {
  const occ = form.occurrence;
  const tzName =
    occ.tz && occ.tz.trim().length > 0
      ? occ.tz.trim()
      : (() => {
          try {
            return Intl.DateTimeFormat().resolvedOptions().timeZone;
          } catch {
            return 'UTC';
          }
        })();
  const days = occ.kind === 'weekly' ? weekdaysCsvToMap(occ.days) : null;
  const isDailyEtc =
    occ.kind === 'daily' ||
    occ.kind === 'weekly' ||
    occ.kind === 'monthly' ||
    occ.kind === 'yearly';
  const o = {
    occ: occ.kind,
    tz: tzName,
    days,
    at: isDailyEtc ? occ.time || '09:00:00' : null,
    onceAt: occ.kind === 'once' ? occ.onceAt || null : null,
    everySecs: occ.kind === 'interval' ? occ.everySecs ?? 60 : null,
    anchor:
      occ.kind === 'interval' && isEdit && occ.intervalAnchor
        ? occ.intervalAnchor
        : null,
    day: occ.kind === 'monthly' || occ.kind === 'yearly' ? occ.day ?? 1 : null,
    month: occ.kind === 'yearly' ? occ.month ?? 1 : null,
    expr: occ.kind === 'cron' ? occ.cronExpr || null : null,
  };
  let action = { type: 'none' };
  if (form.actionType === 'launch') {
    action = {
      type: 'launch',
      command: form.launchCommand,
      args: form.launchArgs ? form.launchArgs.split(/\s+/).filter(Boolean) : [],
      workdir: form.launchWorkdir || null,
    };
  } else if (form.actionType === 'notify') {
    action = {
      type: 'notify',
      title: form.notifyTitle,
      body: form.notifyBody,
    };
  }
  return {
    name: form.name.trim(),
    enabled: form.enabled,
    occurrence: o,
    action,
  };
}

function weekdaysCsvToMap(csv) {
  const map = { mon: false, tue: false, wed: false, thu: false, fri: false, sat: false, sun: false };
  if (typeof csv !== 'string') return map;
  for (const tok of csv.split(/[,\s]+/)) {
    const k = tok.trim().toLowerCase();
    if (k in map) map[k] = true;
  }
  return map;
}

const emptyForm = {
  name: 'tick',
  enabled: true,
  occurrence: {
    kind: 'daily',
    tz: '',
    time: '09:00:00',
    onceAt: '',
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
    // The auditor specifically called: edit on a "once" timer must not
    // blank onceAt. (On re-open-and-save, the field is read back from
    // occ.onceAt by loadFromTimer; buildInput must re-emit it.)
    expect(out.occurrence.onceAt).not.toBeNull();
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
    // The auditor-caught bug: the previous buildInput checked
    // `form.isEdit` (always false → anchor dropped to null → Rust
    // defaulted to now()). After the fix it checks the top-level
    // `isEdit` derived variable. This test exercises both branches:
    //   isEdit=false (new timer) → anchor null
    //   isEdit=true (Edit+Save)  → anchor echoed verbatim
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
});
