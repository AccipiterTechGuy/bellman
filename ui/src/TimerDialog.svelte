<script>
  import { onDestroy, onMount, tick } from 'svelte';
  import {
    createTimer,
    updateTimer,
    deleteTimer,
    previewFires,
    isTauri,
  } from './api.js';
  import { buildInput as buildWireInput, weekdaysCsvToMap, weekdaysMapToCsv, WEEKDAY_CHIPS } from './dialog-build.js';
  import {
    isPlausibleCron,
    listTimeZones,
    parseClockTime,
    parseOnceFields,
    splitOnceAt,
    systemTimeZone,
  } from './datetime-input.js';

  /** Existing Timer when editing; null when creating. */
  let { timer = null, onClose, onToast } = $props();

  // Form state. Kind-specific free-text fields keep what the user typed;
  // buildInput normalizes to the wire ISO/CSV the Rust side expects.
  let form = $state(emptyForm());
  let preview = $state({ fires: [], warnings: [] });
  /** Server-side preview failures (invalid cron, bad tz, …) — NOT DST advisories. */
  let previewError = $state(null);
  let previewBusy = $state(false);
  let saveBusy = $state(false);
  let deleteBusy = $state(false);
  let showDeleteConfirm = $state(false);
  let previewToken = 0;
  let nameInputEl = $state(null);
  let tzFilter = $state('');

  const allTimeZones = listTimeZones();

  function emptyForm() {
    const sysTz = systemTimeZone();
    return {
      name: '',
      enabled: true,
      occurrence: {
        kind: 'daily',
        tz: sysTz,
        time: '09:00',
        onceAt: '',
        onceDate: '',
        onceTime: '09:00',
        everySecs: 300,
        intervalAnchor: null,
        days: 'mon,wed,fri',
        day: 1,
        month: 1,
        cronExpr: '',
      },
      // Default stays `none` — a new timer must not surprise the user with a
      // notification or launch. The dialog states that plainly under Wake action.
      actionType: 'none',
      launchCommand: '',
      launchArgs: '',
      launchWorkdir: '',
      notifyTitle: '',
      notifyBody: '',
    };
  }

  function loadFromTimer(t) {
    const occ = t.occurrence || {};
    const occKind = occ.occ || 'daily';
    const days = occ.days || {};
    const daysCsv = Object.keys(days)
      .filter((k) => days[k])
      .sort()
      .join(',');
    const clockOrEmpty = (v) => (typeof v === 'string' ? v : '');
    const timeStr = clockOrEmpty(occ.at) || '09:00:00';
    // Prefer HH:MM display when seconds are zero — typed input accepts both.
    const timeDisplay = timeStr.endsWith(':00') && timeStr.length === 8
      ? timeStr.slice(0, 5)
      : timeStr;
    const onceAtStr = occKind === 'once' ? clockOrEmpty(occ.onceAt) : '';
    const onceParts = splitOnceAt(onceAtStr);
    const intervalAnchorIso = occKind === 'interval' && occ.anchor ? occ.anchor : null;
    const everySecsVal = occKind === 'interval' ? (occ.everySecs ?? 60) : 60;
    const ak = t.actionKind || {};
    let actionType = 'none';
    let launchCommand = '';
    let launchArgs = '';
    let launchWorkdir = '';
    let notifyTitle = '';
    let notifyBody = '';
    if (ak.type === 'launch') {
      actionType = 'launch';
      launchCommand = ak.command || '';
      launchArgs = Array.isArray(ak.args) ? ak.args.join(' ') : '';
      launchWorkdir = typeof ak.workdir === 'string' ? ak.workdir : '';
    } else if (ak.type === 'notify') {
      actionType = 'notify';
      notifyTitle = ak.title || '';
      notifyBody = ak.body || '';
    }
    form = {
      name: t.name || '',
      enabled: !!t.enabled,
      occurrence: {
        kind: occKind,
        tz: occ.tz || systemTimeZone(),
        time: timeDisplay,
        onceAt: onceAtStr,
        onceDate: onceParts.date,
        onceTime: onceParts.time
          ? (onceParts.time.endsWith(':00') && onceParts.time.length === 8
              ? onceParts.time.slice(0, 5)
              : onceParts.time)
          : '09:00',
        everySecs: everySecsVal,
        intervalAnchor: intervalAnchorIso,
        days: daysCsv || 'mon,wed,fri',
        day: occ.day ?? 1,
        month: occ.month ?? 1,
        cronExpr: occ.expr || '',
      },
      actionType,
      launchCommand,
      launchArgs,
      launchWorkdir,
      notifyTitle,
      notifyBody,
    };
  }

  $effect(() => {
    if (timer && timer.id) loadFromTimer(timer);
    else if (!timer || !timer.id) form = emptyForm();
  });

  let isEdit = $derived(!!(timer && timer.id));

  // --- Normalized wire pieces from human-friendly fields -----------------
  let onceParsed = $derived.by(() => {
    if (form.occurrence.kind !== 'once') return null;
    return parseOnceFields(
      form.occurrence.onceDate,
      form.occurrence.onceTime,
      form.occurrence.tz || systemTimeZone(),
    );
  });

  let timeParsed = $derived.by(() => {
    if (!['daily', 'weekly', 'monthly', 'yearly'].includes(form.occurrence.kind)) {
      return null;
    }
    return parseClockTime(form.occurrence.time);
  });

  function wireOverrides() {
    const o = {};
    if (form.occurrence.kind === 'once' && onceParsed && onceParsed.ok) {
      o.onceAt = onceParsed.onceAt;
    } else if (form.occurrence.kind === 'once') {
      o.onceAt = null;
    }
    if (timeParsed && timeParsed.ok) {
      o.time = timeParsed.hhmmss;
    }
    return o;
  }

  function buildInput() {
    return buildWireInput(form, isEdit, wireOverrides());
  }

  function buildPatch() {
    const input = buildInput();
    return {
      name: input.name,
      enabled: input.enabled,
      occurrence: input.occurrence,
      actionKind: input.action,
    };
  }

  // --- Per-field validation (inline; blocks Create/Save) -----------------
  let fieldErrors = $derived.by(() => {
    /** @type {Record<string, string>} */
    const e = {};
    if (!form.name.trim()) e.name = 'Name is required';

    const kind = form.occurrence.kind;
    if (kind === 'once') {
      if (!onceParsed || !onceParsed.ok) {
        e.once = (onceParsed && onceParsed.error) || 'When date/time is required';
      }
    }
    if (['daily', 'weekly', 'monthly', 'yearly'].includes(kind)) {
      if (!timeParsed || !timeParsed.ok) {
        e.time = (timeParsed && timeParsed.error) || 'Time is required';
      }
    }
    if (kind === 'weekly') {
      const map = weekdaysCsvToMap(form.occurrence.days);
      if (!Object.values(map).some(Boolean)) {
        e.days = 'Pick at least one weekday';
      }
    }
    if (kind === 'interval') {
      const n = Number(form.occurrence.everySecs);
      if (!Number.isFinite(n) || n < 1) e.every = 'Every must be ≥ 1 second';
    }
    if (kind === 'monthly' || kind === 'yearly') {
      const d = Number(form.occurrence.day);
      if (!Number.isInteger(d) || d < 1 || d > 31) e.day = 'Day must be 1–31 (short months clamp)';
    }
    if (kind === 'yearly') {
      const m = Number(form.occurrence.month);
      if (!Number.isInteger(m) || m < 1 || m > 12) e.month = 'Month must be 1–12';
    }
    if (kind === 'cron') {
      const expr = form.occurrence.cronExpr.trim();
      if (!expr) {
        e.cron = 'Cron expression is required';
      } else if (!isPlausibleCron(expr)) {
        // Structural gate so "not a cron" never reaches Create (server only
        // parses on next-fire; invalid expr returns zero fires without Err).
        e.cron = 'Cron expression looks invalid (need 5 or 6 fields)';
      }
    }
    if (form.actionType === 'launch' && !form.launchCommand.trim()) {
      e.launch = 'Command is required for launch action';
    }
    if (form.actionType === 'notify' && !form.notifyTitle.trim()) {
      e.notify = 'Notification title is required';
    }
    return e;
  });

  let saveBlockedReason = $derived.by(() => {
    const keys = Object.keys(fieldErrors);
    if (keys.length > 0) return fieldErrors[keys[0]];
    if (previewError) return previewError;
    if (previewBusy) return 'Waiting for preview…';
    return '';
  });

  // Gate Create on local field errors AND live preview failures (invalid tz,
  // server reject). While preview is in flight, keep Create disabled so a
  // stale "ok" window cannot click through between keystrokes.
  let canSave = $derived(
    Object.keys(fieldErrors).length === 0 &&
      !saveBusy &&
      !previewError &&
      !(isTauri() && previewBusy),
  );

  // Filtered timezone list for the searchable dropdown under the input.
  // System zone first when unfiltered so the preselected value is visible.
  let filteredZones = $derived.by(() => {
    const q = tzFilter.trim().toLowerCase();
    const sys = systemTimeZone();
    if (!q) {
      const rest = allTimeZones.filter((z) => z !== sys).slice(0, 39);
      return sys ? [sys, ...rest] : rest;
    }
    return allTimeZones.filter((z) => z.toLowerCase().includes(q)).slice(0, 60);
  });

  let dayMap = $derived(weekdaysCsvToMap(form.occurrence.days));

  function toggleDay(key) {
    const map = weekdaysCsvToMap(form.occurrence.days);
    map[key] = !map[key];
    form.occurrence.days = weekdaysMapToCsv(map);
  }

  function onNativeDate(e) {
    const v = e.currentTarget.value; // YYYY-MM-DD when set
    if (v) form.occurrence.onceDate = v;
  }

  function onNativeTimeOnce(e) {
    const v = e.currentTarget.value; // HH:MM or HH:MM:SS
    if (v) form.occurrence.onceTime = v.length === 5 ? v : v;
  }

  function onNativeTimeWall(e) {
    const v = e.currentTarget.value;
    if (v) form.occurrence.time = v;
  }

  /** Value for <input type=date> — only when onceDate is valid ISO. */
  let nativeDateValue = $derived.by(() => {
    if (onceParsed && onceParsed.ok) return onceParsed.isoDate;
    const m = (form.occurrence.onceDate || '').match(/^(\d{4}-\d{2}-\d{2})$/);
    return m ? m[1] : '';
  });

  let nativeOnceTimeValue = $derived.by(() => {
    const t = parseClockTime(form.occurrence.onceTime || '');
    if (!t.ok) return '';
    // type=time wants HH:MM or HH:MM:SS
    return t.s === 0 ? `${String(t.h).padStart(2, '0')}:${String(t.m).padStart(2, '0')}` : t.hhmmss;
  });

  let nativeWallTimeValue = $derived.by(() => {
    if (!timeParsed || !timeParsed.ok) return '';
    return timeParsed.s === 0
      ? `${String(timeParsed.h).padStart(2, '0')}:${String(timeParsed.m).padStart(2, '0')}`
      : timeParsed.hhmmss;
  });

  function previewTimeout() {
    const my = ++previewToken;
    // Don't hit the backend with known-invalid local state.
    if (Object.keys(fieldErrors).length > 0) {
      preview = { fires: [], warnings: [] };
      previewError = null;
      previewBusy = false;
      return;
    }
    previewBusy = true;
    previewFires(buildInput().occurrence, 5)
      .then((r) => {
        if (my !== previewToken) return;
        preview = { fires: r.fires || [], warnings: r.warnings || [] };
        previewError = null;
      })
      .catch((e) => {
        if (my !== previewToken) return;
        preview = { fires: [], warnings: [] };
        previewError = String(e);
      })
      .finally(() => {
        if (my === previewToken) previewBusy = false;
      });
  }

  let previewTimer = null;
  function schedulePreview() {
    if (previewTimer) clearTimeout(previewTimer);
    previewTimer = setTimeout(previewTimeout, 250);
  }
  $effect(() => {
    JSON.stringify(form.occurrence);
    form.name;
    form.actionType;
    if (isTauri()) schedulePreview();
  });

  onDestroy(() => {
    if (previewTimer) clearTimeout(previewTimer);
  });

  onMount(async () => {
    await tick();
    // Focus Name so typing starts immediately and Escape reaches the dialog
    // (handler is on the backdrop; events bubble from the focused field).
    nameInputEl?.focus();
  });

  async function save() {
    if (!canSave) {
      onToast(saveBlockedReason || 'Fix the highlighted fields', 'err');
      return;
    }
    saveBusy = true;
    try {
      const input = buildInput();
      if (isEdit) {
        const updated = await updateTimer(timer.id, timer.revision, buildPatch());
        onToast(`Updated "${updated.name}"`);
      } else {
        const created = await createTimer(input);
        onToast(`Created "${created.name}"`);
      }
      onClose(true);
    } catch (e) {
      onToast(String(e), 'err');
    } finally {
      saveBusy = false;
    }
  }

  async function doDelete() {
    deleteBusy = true;
    try {
      await deleteTimer(timer.id);
      onToast(`Deleted "${timer.name}"`);
      onClose(true);
    } catch (e) {
      onToast(String(e), 'err');
      deleteBusy = false;
    }
  }

  function pickZone(z) {
    form.occurrence.tz = z;
    tzFilter = '';
  }
</script>

<div class="wizard-backdrop" role="dialog" aria-modal="true" tabindex="-1"
     onclick={(e) => { if (e.target.classList.contains('wizard-backdrop')) onClose(false); }}
     onkeydown={(e) => { if (e.key === 'Escape') onClose(false); }}>
  <div class="wizard timer-dialog">
    <header>
      <h2>{isEdit ? `Edit "${timer.name}"` : 'New timer'}</h2>
      <button class="btn" onclick={() => onClose(false)} aria-label="close">×</button>
    </header>

    <div class="dialog-body">
      <div class="form-col">
        <div class="form-row" class:has-error={!!fieldErrors.name}>
          <label for="td-name">Name</label>
          <div class="field-stack">
            <input id="td-name" bind:this={nameInputEl} bind:value={form.name}
                   placeholder="e.g. morning backup" autocomplete="off" />
            {#if fieldErrors.name}
              <div class="field-error" role="alert">{fieldErrors.name}</div>
            {/if}
          </div>
        </div>

        <div class="form-row">
          <label for="td-kind">Occurrence kind</label>
          <div class="field-stack">
            <select id="td-kind" bind:value={form.occurrence.kind}>
              <option value="once">once</option>
              <option value="interval">interval</option>
              <option value="daily">daily</option>
              <option value="weekly">weekly</option>
              <option value="monthly">monthly</option>
              <option value="yearly">yearly</option>
              <option value="cron">cron</option>
            </select>
            <div class="field-hint">once · interval · daily · weekly · monthly · yearly · cron (power-user)</div>
          </div>
        </div>

        <div class="form-row">
          <label for="td-tz">Timezone</label>
          <div class="field-stack">
            <input id="td-tz" list="td-tz-datalist" bind:value={form.occurrence.tz}
                   placeholder={systemTimeZone()}
                   oninput={(e) => { tzFilter = e.currentTarget.value; }}
                   autocomplete="off" />
            <datalist id="td-tz-datalist">
              {#each allTimeZones as z}
                <option value={z}></option>
              {/each}
            </datalist>
            <div class="tz-picker" role="listbox" aria-label="Timezone suggestions">
              {#each filteredZones as z}
                <!-- tabindex=-1: list is mouse/filter assist; Tab must reach
                     wall-clock / Create for keyboard-only creation (F5). -->
                <button type="button" class="tz-option" class:active={z === form.occurrence.tz}
                        role="option" aria-selected={z === form.occurrence.tz}
                        tabindex="-1"
                        onclick={() => pickZone(z)}>{z}</button>
              {/each}
            </div>
            <div class="field-hint">System default pre-filled; type any IANA name or pick from the list.</div>
          </div>
        </div>

        {#if form.occurrence.kind === 'once'}
          <div class="form-row stacked" class:has-error={!!fieldErrors.once}>
            <span class="row-label" id="td-once-label">When</span>
            <div class="field-stack" role="group" aria-labelledby="td-once-label">
              <div class="picker-row">
                <input id="td-once-date" class="grow" bind:value={form.occurrence.onceDate}
                       placeholder="24.12.2026" autocomplete="off" aria-label="Once date" />
                <input type="date" class="native-picker" value={nativeDateValue}
                       oninput={onNativeDate} aria-label="Once date picker" />
                <input id="td-once-time" class="time-text" bind:value={form.occurrence.onceTime}
                       placeholder="09:00" autocomplete="off" aria-label="Once time" />
                <input type="time" class="native-picker" step="1" value={nativeOnceTimeValue}
                       oninput={onNativeTimeOnce} aria-label="Once time picker" />
              </div>
              {#if onceParsed && onceParsed.ok}
                <div class="field-echo" id="td-once-echo" data-testid="once-echo">
                  {onceParsed.echo}{#if onceParsed.note} · {onceParsed.note}{/if}
                </div>
              {:else if form.occurrence.onceDate || form.occurrence.onceTime}
                <div class="field-echo muted">Type a date (e.g. 24.12.2026) and time (09:00)</div>
              {/if}
              {#if fieldErrors.once}
                <div class="field-error" role="alert">{fieldErrors.once}</div>
              {/if}
              <div class="field-hint">Accepts 24.12.2026, 24.12.2026 09:00, ISO 2026-12-24T09:00:00. Seconds optional. Day-first for dots/dashes.</div>
            </div>
          </div>
        {/if}

        {#if form.occurrence.kind === 'interval'}
          <div class="form-row" class:has-error={!!fieldErrors.every}>
            <label for="td-every">Every (seconds)</label>
            <div class="field-stack">
              <input id="td-every" type="number" min="1" bind:value={form.occurrence.everySecs} />
              {#if fieldErrors.every}
                <div class="field-error" role="alert">{fieldErrors.every}</div>
              {/if}
            </div>
          </div>
        {/if}

        {#if ['daily', 'weekly', 'monthly', 'yearly'].includes(form.occurrence.kind)}
          <div class="form-row" class:has-error={!!fieldErrors.time}>
            <label for="td-time">Wall-clock time</label>
            <div class="field-stack">
              <div class="picker-row">
                <input id="td-time" class="time-text grow" bind:value={form.occurrence.time}
                       placeholder="09:00" autocomplete="off" />
                <input type="time" class="native-picker" step="1" value={nativeWallTimeValue}
                       oninput={onNativeTimeWall} aria-label="Time picker" />
              </div>
              {#if fieldErrors.time}
                <div class="field-error" role="alert">{fieldErrors.time}</div>
              {/if}
              <div class="field-hint">HH:MM or HH:MM:SS — seconds optional.</div>
            </div>
          </div>
        {/if}

        {#if form.occurrence.kind === 'weekly'}
          <div class="form-row stacked" class:has-error={!!fieldErrors.days}>
            <span class="row-label" id="td-days-label">Weekdays</span>
            <div class="field-stack" role="group" aria-labelledby="td-days-label">
              <div class="weekday-chips">
                {#each WEEKDAY_CHIPS as chip}
                  <button type="button"
                          class="weekday-chip"
                          class:on={dayMap[chip.key]}
                          aria-pressed={dayMap[chip.key]}
                          onclick={() => toggleDay(chip.key)}>
                    {chip.label}
                  </button>
                {/each}
              </div>
              {#if fieldErrors.days}
                <div class="field-error" role="alert">{fieldErrors.days}</div>
              {/if}
            </div>
          </div>
        {/if}

        {#if form.occurrence.kind === 'monthly'}
          <div class="form-row" class:has-error={!!fieldErrors.day}>
            <label for="td-day">Day of month</label>
            <div class="field-stack">
              <input id="td-day" type="number" min="1" max="31" bind:value={form.occurrence.day} />
              <div class="field-hint">1–31. Short months clamp (e.g. 31 → last day of month).</div>
              {#if fieldErrors.day}
                <div class="field-error" role="alert">{fieldErrors.day}</div>
              {/if}
            </div>
          </div>
        {/if}

        {#if form.occurrence.kind === 'yearly'}
          <div class="form-row" class:has-error={!!fieldErrors.month}>
            <label for="td-month">Month</label>
            <div class="field-stack">
              <input id="td-month" type="number" min="1" max="12" bind:value={form.occurrence.month} />
              <div class="field-hint">1–12.</div>
              {#if fieldErrors.month}
                <div class="field-error" role="alert">{fieldErrors.month}</div>
              {/if}
            </div>
          </div>
          <div class="form-row" class:has-error={!!fieldErrors.day}>
            <label for="td-day2">Day of month</label>
            <div class="field-stack">
              <input id="td-day2" type="number" min="1" max="31" bind:value={form.occurrence.day} />
              <div class="field-hint">1–31; clamps on short months. Feb 29 only fires on leap years.</div>
              {#if fieldErrors.day}
                <div class="field-error" role="alert">{fieldErrors.day}</div>
              {/if}
            </div>
          </div>
        {/if}

        {#if form.occurrence.kind === 'cron'}
          <div class="form-row" class:has-error={!!fieldErrors.cron}>
            <label for="td-cron">Cron expression</label>
            <div class="field-stack">
              <input id="td-cron" bind:value={form.occurrence.cronExpr} placeholder="*/5 * * * *"
                     spellcheck="false" autocomplete="off" />
              <div class="field-hint">Raw 5- or 6-field expression (power-user escape hatch — no builder).</div>
              {#if fieldErrors.cron}
                <div class="field-error" role="alert">{fieldErrors.cron}</div>
              {/if}
            </div>
          </div>
        {/if}

        <fieldset class="action-set">
          <legend>Wake action</legend>
          <label class="radio">
            <input type="radio" bind:group={form.actionType} value="none" /> none
          </label>
          <div class="field-hint action-none-hint">
            none (default) — the timer fires on schedule but does not notify or launch anything.
            Choose “desktop notification” or “launch command” if you want a visible effect.
          </div>
          <label class="radio">
            <input type="radio" bind:group={form.actionType} value="launch" /> launch command
          </label>
          {#if form.actionType === 'launch'}
            <div class="form-row">
              <label for="td-cmd">Command</label>
              <input id="td-cmd" bind:value={form.launchCommand} placeholder="/usr/bin/notify-send" />
            </div>
            <div class="form-row">
              <label for="td-args">Args (space-separated)</label>
              <input id="td-args" bind:value={form.launchArgs} placeholder="hello world" />
            </div>
            <div class="form-row">
              <label for="td-workdir">Working directory</label>
              <input id="td-workdir" bind:value={form.launchWorkdir} placeholder="/tmp" />
            </div>
            {#if fieldErrors.launch}
              <div class="field-error" role="alert">{fieldErrors.launch}</div>
            {/if}
          {/if}
          <label class="radio">
            <input type="radio" bind:group={form.actionType} value="notify" /> desktop notification
          </label>
          {#if form.actionType === 'notify'}
            <div class="form-row">
              <label for="td-title">Title</label>
              <input id="td-title" bind:value={form.notifyTitle} />
            </div>
            <div class="form-row">
              <label for="td-body">Body</label>
              <input id="td-body" bind:value={form.notifyBody} />
            </div>
            {#if fieldErrors.notify}
              <div class="field-error" role="alert">{fieldErrors.notify}</div>
            {/if}
          {/if}
        </fieldset>

        <div class="form-row checkbox-row">
          <label><input type="checkbox" bind:checked={form.enabled} /> Enabled</label>
        </div>
      </div>

      <aside class="preview-pane">
        <header>
          <span>Next 5 fires</span>
          {#if previewBusy}<span class="muted">updating…</span>{/if}
        </header>
        {#if previewError}
          <div class="preview-error" role="alert">
            <span class="banner-kind">Error</span>
            <div class="banner-body">{previewError}</div>
          </div>
        {/if}
        {#if preview.warnings && preview.warnings.length > 0}
          <div class="dst-warning" role="status">
            <span class="banner-kind">Advisory</span>
            {#each preview.warnings as w}
              <div class="dst-line">{w}</div>
            {/each}
          </div>
        {/if}
        {#if preview.fires.length === 0}
          <div class="empty">No preview. Fill in the required fields to see the next fires.</div>
        {:else}
          <table class="preview-table">
            <thead>
              <tr>
                <th>#</th>
                <th>Local</th>
                <th>UTC</th>
                <th>Offset / tz</th>
              </tr>
            </thead>
            <tbody>
              {#each preview.fires as f, i}
                <tr>
                  <td>{i + 1}</td>
                  <td class="mono">{f.localDate} {f.localTime}</td>
                  <td class="mono">{f.utc ? new Date(f.utc).toISOString().replace(/\.\d+Z$/, 'Z') : ''}</td>
                  <td class="mono">{f.offset} {f.tzName}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </aside>
    </div>

    <footer>
      <div class="footer-left">
        {#if isEdit && !showDeleteConfirm}
          <button class="btn danger" onclick={() => showDeleteConfirm = true}>Delete…</button>
        {/if}
        {#if isEdit && showDeleteConfirm}
          <button class="btn danger" disabled={deleteBusy} onclick={doDelete}>
            {deleteBusy ? 'Deleting…' : 'Confirm delete'}
          </button>
          <button class="btn" onclick={() => showDeleteConfirm = false} disabled={deleteBusy}>Cancel</button>
        {/if}
      </div>
      <div class="footer-right">
        {#if !canSave && saveBlockedReason}
          <span class="save-reason" title={saveBlockedReason}>{saveBlockedReason}</span>
        {/if}
        <button class="btn" onclick={() => onClose(false)} disabled={saveBusy}>Cancel</button>
        <button class="btn primary" onclick={save} disabled={!canSave}
                title={!canSave ? saveBlockedReason : (isEdit ? 'Save' : 'Create')}>
          {saveBusy ? 'Saving…' : isEdit ? 'Save' : 'Create'}
        </button>
      </div>
    </footer>
  </div>
</div>
