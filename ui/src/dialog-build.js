/**
 * Shared wire-contract builder for TimerDialog.
 * Mirrors `src-tauri/src/web.rs::WebOccurrenceDto` field-by-field.
 * Used by TimerDialog.svelte and dialog-build.test.js so the IPC shape
 * cannot drift between the UI and its unit tests.
 *
 * @param {object} form  dialog form state (see TimerDialog emptyForm)
 * @param {boolean} isEdit
 * @param {{ onceAt?: string|null, time?: string|null }} [overrides]
 *        Optional normalized values (e.g. parsed human onceAt / padded time)
 *        so the dialog can keep raw free-text in form state while the wire
 *        still emits ISO.
 */
export function buildInput(form, isEdit, overrides = {}) {
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
  const isDailyEtc =
    occ.kind === 'daily' ||
    occ.kind === 'weekly' ||
    occ.kind === 'monthly' ||
    occ.kind === 'yearly';

  const wireTime =
    overrides.time != null
      ? overrides.time
      : isDailyEtc
        ? occ.time || '09:00:00'
        : null;
  const wireOnceAt =
    overrides.onceAt !== undefined
      ? overrides.onceAt
      : occ.kind === 'once'
        ? occ.onceAt || null
        : null;

  const o = {
    occ: occ.kind,
    tz: tzName,
    days: occ.kind === 'weekly' ? weekdaysCsvToMap(occ.days) : null,
    at: isDailyEtc ? wireTime : null,
    onceAt: occ.kind === 'once' ? wireOnceAt : null,
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
    wakeMachine: !!form.wakeMachine,
    occurrence: o,
    action,
  };
}

/** Convert `"mon,wed,fri"` -> `{ mon:true, tue:false, ..., sun:false }`. */
export function weekdaysCsvToMap(csv) {
  const map = {
    mon: false,
    tue: false,
    wed: false,
    thu: false,
    fri: false,
    sat: false,
    sun: false,
  };
  if (typeof csv !== 'string') return map;
  for (const tok of csv.split(/[,\s]+/)) {
    const k = tok.trim().toLowerCase();
    if (k in map) map[k] = true;
  }
  return map;
}

/** Map → stable CSV (Mon→Sun order). */
export function weekdaysMapToCsv(map) {
  const order = ['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun'];
  return order.filter((k) => map && map[k]).join(',');
}

export const WEEKDAY_CHIPS = [
  { key: 'mon', label: 'Mon' },
  { key: 'tue', label: 'Tue' },
  { key: 'wed', label: 'Wed' },
  { key: 'thu', label: 'Thu' },
  { key: 'fri', label: 'Fri' },
  { key: 'sat', label: 'Sat' },
  { key: 'sun', label: 'Sun' },
];
