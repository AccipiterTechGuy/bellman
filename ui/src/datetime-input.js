/**
 * Human-friendly date/time parsing for TimerDialog.
 *
 * Wire format stays ISO (YYYY-MM-DDTHH:MM[:SS] for once; HH:MM[:SS] for wall clock).
 * This module only lives in the UI — the Rust side still receives the same DTO.
 *
 * Rules (card C8d):
 * - Dot- and dash-separated numeric dates are day-first (24.12.2026, 24-12-2026).
 * - ISO year-first (2026-12-24 / 2026-12-24T09:00:00) is recognized when the
 *   first component is a 4-digit year.
 * - Slash-separated dates are resolved explicitly (if one side > 12 that side
 *   is the day; if both ≤ 12 → day-first) and the interpretation is always
 *   echoed in words so a wrong reading is visible before Create.
 * - Never silently guess: unparseable input returns an error string.
 * - Seconds are optional everywhere.
 */

const MONTHS = [
  'January', 'February', 'March', 'April', 'May', 'June',
  'July', 'August', 'September', 'October', 'November', 'December',
];
const WEEKDAYS = [
  'Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday',
];

/**
 * @param {number} y
 * @param {number} m 1-12
 * @param {number} d
 */
export function isValidYmd(y, m, d) {
  if (!Number.isInteger(y) || !Number.isInteger(m) || !Number.isInteger(d)) return false;
  if (m < 1 || m > 12 || d < 1 || d > 31 || y < 1 || y > 9999) return false;
  const dt = new Date(Date.UTC(y, m - 1, d));
  return dt.getUTCFullYear() === y && dt.getUTCMonth() === m - 1 && dt.getUTCDate() === d;
}

/**
 * Pad time to HH:MM:SS. Accepts HH:MM or HH:MM:SS.
 * @param {string} s
 * @returns {{ ok: true, hhmmss: string, h: number, m: number, s: number } | { ok: false, error: string }}
 */
export function parseClockTime(s) {
  if (s == null || typeof s !== 'string') {
    return { ok: false, error: 'Time is required (HH:MM or HH:MM:SS)' };
  }
  const t = s.trim();
  if (!t) return { ok: false, error: 'Time is required (HH:MM or HH:MM:SS)' };
  let m = t.match(/^(\d{1,2}):(\d{2})(?::(\d{2}))?$/);
  if (!m) return { ok: false, error: `Invalid time '${t}' (expected HH:MM or HH:MM:SS)` };
  const h = Number(m[1]);
  const mi = Number(m[2]);
  const se = m[3] != null ? Number(m[3]) : 0;
  if (h > 23 || mi > 59 || se > 59) {
    return { ok: false, error: `Invalid time '${t}' (out of range)` };
  }
  const hhmmss = `${String(h).padStart(2, '0')}:${String(mi).padStart(2, '0')}:${String(se).padStart(2, '0')}`;
  return { ok: true, hhmmss, h, m: mi, s: se };
}

/**
 * Split a free-text once value that may contain both date and time.
 * Returns { datePart, timePart } where timePart may be ''.
 * @param {string} raw
 */
export function splitDateAndTime(raw) {
  const s = (raw || '').trim();
  if (!s) return { datePart: '', timePart: '' };
  // ISO with T
  const tIdx = s.indexOf('T');
  if (tIdx > 0 && /^\d{4}-\d{2}-\d{2}T/.test(s)) {
    return { datePart: s.slice(0, tIdx), timePart: s.slice(tIdx + 1) };
  }
  // Space-separated trailing time HH:MM[:SS]
  const sp = s.match(/^(.*\S)\s+(\d{1,2}:\d{2}(?::\d{2})?)$/);
  if (sp) return { datePart: sp[1].trim(), timePart: sp[2] };
  return { datePart: s, timePart: '' };
}

/**
 * Parse a date-only string into Y/M/D.
 * @param {string} raw
 * @returns {{ ok: true, y: number, m: number, d: number, note?: string } | { ok: false, error: string }}
 */
export function parseDateOnly(raw) {
  if (raw == null || typeof raw !== 'string') {
    return { ok: false, error: 'Date is required' };
  }
  const s = raw.trim();
  if (!s) return { ok: false, error: 'Date is required' };

  // ISO year-first: YYYY-MM-DD
  let m = s.match(/^(\d{4})-(\d{1,2})-(\d{1,2})$/);
  if (m) {
    const y = Number(m[1]);
    const mo = Number(m[2]);
    const d = Number(m[3]);
    if (!isValidYmd(y, mo, d)) return { ok: false, error: `Invalid calendar date '${s}'` };
    return { ok: true, y, m: mo, d };
  }

  // Dot-separated day-first: D.M.YYYY or DD.MM.YYYY
  m = s.match(/^(\d{1,2})\.(\d{1,2})\.(\d{4})$/);
  if (m) {
    const d = Number(m[1]);
    const mo = Number(m[2]);
    const y = Number(m[3]);
    if (!isValidYmd(y, mo, d)) return { ok: false, error: `Invalid calendar date '${s}' (day-first)` };
    return { ok: true, y, m: mo, d };
  }

  // Dash-separated day-first (not year-first — year-first already handled): D-M-YYYY
  m = s.match(/^(\d{1,2})-(\d{1,2})-(\d{4})$/);
  if (m) {
    const d = Number(m[1]);
    const mo = Number(m[2]);
    const y = Number(m[3]);
    if (!isValidYmd(y, mo, d)) return { ok: false, error: `Invalid calendar date '${s}' (day-first)` };
    return { ok: true, y, m: mo, d };
  }

  // Slash-separated: resolve explicitly
  m = s.match(/^(\d{1,2})\/(\d{1,2})\/(\d{4})$/);
  if (m) {
    const a = Number(m[1]);
    const b = Number(m[2]);
    const y = Number(m[3]);
    let d;
    let mo;
    let note;
    if (a > 12 && b <= 12) {
      // must be D/M/Y
      d = a;
      mo = b;
      note = 'slash date read as day/month/year';
    } else if (b > 12 && a <= 12) {
      // must be M/D/Y
      mo = a;
      d = b;
      note = 'slash date read as month/day/year';
    } else if (a <= 12 && b <= 12) {
      // both plausible — product rule: day-first
      d = a;
      mo = b;
      note = 'slash date ambiguous; interpreted day/month/year';
    } else {
      return { ok: false, error: `Invalid slash date '${s}'` };
    }
    if (!isValidYmd(y, mo, d)) return { ok: false, error: `Invalid calendar date '${s}'` };
    return { ok: true, y, m: mo, d, note };
  }

  return {
    ok: false,
    error: `Unrecognized date '${s}' (try 24.12.2026, 2026-12-24, or 24/12/2026)`,
  };
}

/**
 * Format a parsed local wall time as words next to the field.
 * @param {{ y: number, m: number, d: number, h?: number, mi?: number, s?: number }} p
 * @param {string} tz
 */
export function formatEcho(p, tz) {
  const { y, m, d } = p;
  const h = p.h ?? 0;
  const mi = p.mi ?? 0;
  const se = p.s ?? 0;
  // Use UTC noon to avoid DST edge flipping the weekday when we only care about the civil date.
  const weekday = WEEKDAYS[new Date(Date.UTC(y, m - 1, d, 12, 0, 0)).getUTCDay()];
  const monthName = MONTHS[m - 1];
  const timeStr =
    se === 0
      ? `${String(h).padStart(2, '0')}:${String(mi).padStart(2, '0')}`
      : `${String(h).padStart(2, '0')}:${String(mi).padStart(2, '0')}:${String(se).padStart(2, '0')}`;
  const zone = (tz && tz.trim()) || 'system local';
  return `${weekday} ${d} ${monthName} ${y}, ${timeStr} ${zone}`;
}

/**
 * Parse once fields into wire onceAt + echo.
 * `dateRaw` may include a trailing time (`24.12.2026 09:00`).
 * `timeRaw` supplies the time when the date part has none; default 00:00:00 only if date alone is intentional — we require a time.
 *
 * @param {string} dateRaw
 * @param {string} timeRaw
 * @param {string} tz
 * @returns {{ ok: true, onceAt: string, isoDate: string, hhmmss: string, echo: string, note?: string }
 *         | { ok: false, error: string, echo?: string }}
 */
export function parseOnceFields(dateRaw, timeRaw, tz) {
  const { datePart, timePart: embeddedTime } = splitDateAndTime(dateRaw);
  const date = parseDateOnly(datePart);
  if (!date.ok) return { ok: false, error: date.error };

  const timeSource = (embeddedTime || timeRaw || '').trim();
  if (!timeSource) {
    return {
      ok: false,
      error: 'Time is required (HH:MM or HH:MM:SS)',
      echo: formatEcho({ y: date.y, m: date.m, d: date.d, h: 0, mi: 0, s: 0 }, tz) + ' — time missing',
    };
  }
  const time = parseClockTime(timeSource);
  if (!time.ok) return { ok: false, error: time.error };

  const isoDate = `${String(date.y).padStart(4, '0')}-${String(date.m).padStart(2, '0')}-${String(date.d).padStart(2, '0')}`;
  const onceAt = `${isoDate}T${time.hhmmss}`;
  const echo = formatEcho(
    { y: date.y, m: date.m, d: date.d, h: time.h, mi: time.m, s: time.s },
    tz,
  );
  const out = { ok: true, onceAt, isoDate, hhmmss: time.hhmmss, echo };
  if (date.note) out.note = date.note;
  return out;
}

/**
 * Split an existing wire onceAt into date + time text for the form.
 * @param {string} onceAt
 */
export function splitOnceAt(onceAt) {
  if (!onceAt || typeof onceAt !== 'string') return { date: '', time: '' };
  const s = onceAt.trim();
  const m = s.match(/^(\d{4}-\d{2}-\d{2})[T ](\d{2}:\d{2}(?::\d{2})?)/);
  if (m) return { date: m[1], time: m[2].length === 5 ? `${m[2]}:00` : m[2] };
  return { date: s, time: '' };
}

/**
 * List of IANA time zones from the runtime, with a safe fallback.
 * @returns {string[]}
 */
export function listTimeZones() {
  try {
    if (typeof Intl !== 'undefined' && typeof Intl.supportedValuesOf === 'function') {
      return Intl.supportedValuesOf('timeZone');
    }
  } catch {
    /* fall through */
  }
  return [
    'UTC',
    'Europe/Helsinki',
    'Europe/London',
    'Europe/Berlin',
    'America/New_York',
    'America/Los_Angeles',
    'Asia/Tokyo',
    'Australia/Sydney',
  ];
}

/**
 * System IANA zone, or UTC.
 * @returns {string}
 */
export function systemTimeZone() {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
  } catch {
    return 'UTC';
  }
}

/**
 * Structural cron check aligned with croner 2 (the engine in
 * `bellman-core::occurrence::schedule::next_cron`):
 *   - 5- or 6-field expressions, OR a single @macro croner accepts
 *   - numeric / *,/, -,?,#,L,W tokens in all positions
 *   - month names (JAN–DEC) only in the month field
 *   - weekday names (SUN–SAT) only in the day-of-week field
 *   - lists/ranges of names: MON-FRI, mon,wed,fri
 *
 * Deliberately accepts @yearly/@annually/@monthly/@weekly/@daily/@hourly
 * (croner 2 parses them) and rejects @reboot (croner rejects it).
 * Rejects free-text like "not a cron" (wrong field count) without
 * blocking legitimate named-field power-user expressions.
 *
 * @param {string} expr
 * @returns {boolean}
 */
const CRON_MACROS = new Set([
  '@yearly',
  '@annually',
  '@monthly',
  '@weekly',
  '@daily',
  '@hourly',
]);
const CRON_MONTHS = 'JAN|FEB|MAR|APR|MAY|JUN|JUL|AUG|SEP|OCT|NOV|DEC';
const CRON_DOWS = 'SUN|MON|TUE|WED|THU|FRI|SAT';

function isNumericCronField(field) {
  return /^[\d*,/\-?#LW]+$/i.test(field);
}

/** Name atom with optional range, or comma-list of those (MON-FRI / mon,wed). */
function isCronNameField(field, names) {
  const atom = `(?:${names})(?:-(?:${names}))?`;
  return new RegExp(`^${atom}(?:,${atom})*$`, 'i').test(field);
}

export function isPlausibleCron(expr) {
  if (expr == null || typeof expr !== 'string') return false;
  const s = expr.trim();
  if (!s) return false;

  if (s.startsWith('@')) {
    return CRON_MACROS.has(s.toLowerCase());
  }

  const parts = s.split(/\s+/).filter(Boolean);
  // 5-field: min hour dom month dow
  // 6-field: sec min hour dom month dow
  if (parts.length !== 5 && parts.length !== 6) return false;

  const monthIdx = parts.length === 5 ? 3 : 4;
  const dowIdx = parts.length === 5 ? 4 : 5;

  for (let i = 0; i < parts.length; i++) {
    const p = parts[i];
    if (i === monthIdx) {
      if (!(isNumericCronField(p) || isCronNameField(p, CRON_MONTHS))) return false;
    } else if (i === dowIdx) {
      if (!(isNumericCronField(p) || isCronNameField(p, CRON_DOWS))) return false;
    } else if (!isNumericCronField(p)) {
      return false;
    }
  }
  return true;
}
