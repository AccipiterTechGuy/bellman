import { describe, expect, it } from 'vitest';
import {
  formatEcho,
  isPlausibleCron,
  parseClockTime,
  parseDateOnly,
  parseOnceFields,
  splitDateAndTime,
  splitOnceAt,
} from './datetime-input.js';

describe('parseClockTime', () => {
  it('accepts HH:MM and pads seconds', () => {
    const r = parseClockTime('09:00');
    expect(r.ok).toBe(true);
    expect(r.hhmmss).toBe('09:00:00');
  });
  it('accepts HH:MM:SS', () => {
    expect(parseClockTime('23:59:59').hhmmss).toBe('23:59:59');
  });
  it('rejects garbage', () => {
    expect(parseClockTime('noon').ok).toBe(false);
  });
});

describe('parseDateOnly', () => {
  it('day-first dots: 24.12.2026', () => {
    const r = parseDateOnly('24.12.2026');
    expect(r).toMatchObject({ ok: true, y: 2026, m: 12, d: 24 });
  });
  it('ISO year-first: 2026-12-24', () => {
    const r = parseDateOnly('2026-12-24');
    expect(r).toMatchObject({ ok: true, y: 2026, m: 12, d: 24 });
  });
  it('day-first dashes: 24-12-2026', () => {
    const r = parseDateOnly('24-12-2026');
    expect(r).toMatchObject({ ok: true, y: 2026, m: 12, d: 24 });
  });
  it('slash unambiguous day: 24/12/2026', () => {
    const r = parseDateOnly('24/12/2026');
    expect(r).toMatchObject({ ok: true, y: 2026, m: 12, d: 24 });
  });
  it('slash unambiguous month: 12/24/2026', () => {
    const r = parseDateOnly('12/24/2026');
    expect(r).toMatchObject({ ok: true, y: 2026, m: 12, d: 24 });
    expect(r.note).toMatch(/month\/day/);
  });
  it('slash ambiguous day-first: 01/02/2026', () => {
    const r = parseDateOnly('01/02/2026');
    expect(r).toMatchObject({ ok: true, y: 2026, m: 2, d: 1 });
    expect(r.note).toMatch(/ambiguous/);
  });
  it('rejects invalid calendar date', () => {
    expect(parseDateOnly('31.02.2026').ok).toBe(false);
  });
});

describe('parseOnceFields', () => {
  it('24.12.2026 + 09:00 → wire ISO + echo', () => {
    const r = parseOnceFields('24.12.2026', '09:00', 'Europe/Helsinki');
    expect(r.ok).toBe(true);
    expect(r.onceAt).toBe('2026-12-24T09:00:00');
    expect(r.echo).toBe('Thursday 24 December 2026, 09:00 Europe/Helsinki');
  });
  it('combined 24.12.2026 09:00:00', () => {
    const r = parseOnceFields('24.12.2026 09:00:00', '', 'UTC');
    expect(r.ok).toBe(true);
    expect(r.onceAt).toBe('2026-12-24T09:00:00');
  });
  it('ISO 2026-12-24T09:00:00', () => {
    const r = parseOnceFields('2026-12-24T09:00:00', '', 'UTC');
    expect(r.ok).toBe(true);
    expect(r.onceAt).toBe('2026-12-24T09:00:00');
  });
  it('date without time fails visibly', () => {
    const r = parseOnceFields('24.12.2026', '', 'UTC');
    expect(r.ok).toBe(false);
    expect(r.error).toMatch(/Time is required/);
  });
});

describe('isPlausibleCron', () => {
  it('accepts 5- and 6-field numeric expressions', () => {
    expect(isPlausibleCron('0 9 * * 1-5')).toBe(true);
    expect(isPlausibleCron('*/5 * * * *')).toBe(true);
    expect(isPlausibleCron('0 0 9 * * 1-5')).toBe(true);
    expect(isPlausibleCron('0 9 * * 1#2')).toBe(true);
    expect(isPlausibleCron('0 9 * * 5L')).toBe(true);
  });
  it('accepts named weekday/month fields the engine (croner 2) accepts', () => {
    expect(isPlausibleCron('0 9 * * MON-FRI')).toBe(true);
    expect(isPlausibleCron('0 0 1 JAN *')).toBe(true);
    expect(isPlausibleCron('0 9 * * mon,wed,fri')).toBe(true);
    expect(isPlausibleCron('30 14 1 JAN-MAR *')).toBe(true);
  });
  // F7b: name + # / L / step — all parsed by croner 2; gate must not reject.
  it('accepts named fields with #nth, L, and /step (croner-accepted)', () => {
    const engineAccepted = [
      '0 9 * * MON#2',
      '0 9 * * mon#2',
      '0 9 * * SUN#1',
      '0 9 * * SAT#5',
      '0 9 * * MON-FRI#2',
      '0 9 * * TUE,THU#1',
      '0 9 * * FRIL',
      '0 9 * * MONL',
      '0 9 * * MON/2',
      '0 9 * JAN/2 *',
      '0 9 * JAN-DEC/3 *',
    ];
    for (const e of engineAccepted) {
      expect(isPlausibleCron(e), e).toBe(true);
    }
  });
  it('accepts croner @macros deliberately (not @reboot)', () => {
    expect(isPlausibleCron('@daily')).toBe(true);
    expect(isPlausibleCron('@hourly')).toBe(true);
    expect(isPlausibleCron('@weekly')).toBe(true);
    expect(isPlausibleCron('@monthly')).toBe(true);
    expect(isPlausibleCron('@yearly')).toBe(true);
    expect(isPlausibleCron('@annually')).toBe(true);
    expect(isPlausibleCron('@reboot')).toBe(false);
  });
  it('rejects free-text garbage (Create must stay disabled)', () => {
    expect(isPlausibleCron('not a cron')).toBe(false);
    expect(isPlausibleCron('')).toBe(false);
    expect(isPlausibleCron('only four * * *')).toBe(false);
    expect(isPlausibleCron('a b c d e')).toBe(false);
  });
  it('rejects names in the wrong field position', () => {
    // MON in minute field — not substituted → fails numeric charset
    expect(isPlausibleCron('MON 9 * * 1-5')).toBe(false);
    // JAN in hour field
    expect(isPlausibleCron('0 JAN 1 * *')).toBe(false);
  });
});

describe('split helpers', () => {
  it('splitDateAndTime on ISO T', () => {
    expect(splitDateAndTime('2026-12-24T09:00:00')).toEqual({
      datePart: '2026-12-24',
      timePart: '09:00:00',
    });
  });
  it('splitOnceAt', () => {
    expect(splitOnceAt('2026-12-24T09:00:00')).toEqual({
      date: '2026-12-24',
      time: '09:00:00',
    });
  });
  it('formatEcho has weekday words', () => {
    expect(formatEcho({ y: 2026, m: 12, d: 24, h: 9, mi: 0, s: 0 }, 'UTC')).toMatch(
      /Thursday 24 December 2026/,
    );
  });
});
