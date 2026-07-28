import { describe, expect, it } from 'vitest';
import {
  formatEcho,
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
