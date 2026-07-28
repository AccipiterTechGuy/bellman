<script>
  import { onDestroy } from 'svelte';
  import {
    createTimer,
    updateTimer,
    deleteTimer,
    previewFires,
    isTauri,
  } from './api.js';

  /** Existing Timer when editing; null when creating. */
  let { timer = null, onClose, onToast } = $props();

  // Form state. We keep a normalized shape so the kind-specific fields are
  // empty when irrelevant (matches the Rust OccurrenceInput builder).
  let form = $state(emptyForm());
  let preview = $state({ fires: [], warnings: [] });
  let previewBusy = $state(false);
  let saveBusy = $state(false);
  let deleteBusy = $state(false);
  let showDeleteConfirm = $state(false);
  let previewToken = 0;

  function emptyForm() {
    // Plan v1: tz defaults to system local (matches the CLI default).
    // We resolve Intl.DateTimeFormat once per form rather than on every
    // keystroke so the user can freely edit without it bouncing.
    let sysTz = '';
    try {
      sysTz = Intl.DateTimeFormat().resolvedOptions().timeZone;
    } catch {
      sysTz = '';
    }
    return {
      name: '',
      enabled: true,
      occurrence: {
        kind: 'daily',
        tz: sysTz,
        time: '09:00:00',
        onceAt: '',
        everySecs: 300,
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
  }

  // Pull structured fields off the timer JSON. The Rust TimerDto surfaces
  // the deliberate flat `WebOccurrenceDto` (tag `occ`) plus the tagged
  // `WebActionDto` under `actionKind`. We read those directly and never
  // parse the pretty summary. The wire shape is pinned by
  // `src-tauri/src/web_testdata/weekly_dto.json` and the integration
  // test `tauri_create_update_via_real_ipc_json` in
  // `src-tauri/tests/`.
  function loadFromTimer(t) {
    const occ = t.occurrence || {};
    const occKind = occ.occ || 'daily';
    // weekly: {occ:"weekly", days:{mon:true,...}, at:"08:00:00", tz:"UTC", ...}
    // chrono serializes Weekdays as a bitmask object — see web::encode_days.
    const days = occ.days || {};
    const daysCsv = Object.keys(days)
      .filter((k) => days[k])
      .sort()
      .join(',');
    // NaiveTime serialized by chrono as HH:MM:SS for daily/weekly/monthly/yearly.
    // `once` uses `onceAt` (a NaiveDateTime ISO 8601).
    const clockOrEmpty = (v) => (typeof v === 'string' ? v : '');
    const timeStr = clockOrEmpty(occ.at) || '09:00:00';
    const onceAtStr = occKind === 'once' ? clockOrEmpty(occ.onceAt) : '';
    // Interval anchor (chrono DateTime<Utc>) → ISO string. Preserve verbatim
    // so Edit+Save of a stable-anchor interval does NOT reset the anchor.
    const intervalAnchorIso = occKind === 'interval' && occ.anchor ? occ.anchor : null;
    const everySecsVal = occKind === 'interval' ? (occ.everySecs ?? 60) : 60;
    // Pretty action label: derive a ui-side radio default from the
    // structured action_kind enum (tagged `type`). Preserve every
    // field, including `Launch.workdir`, which the previous build
    // dropped on save.
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
        tz: occ.tz || '',
        time: timeStr,
        onceAt: onceAtStr,
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
    if (timer) loadFromTimer(timer);
    else form = emptyForm();
  });

  let isEdit = $derived(!!timer);

  // Build the GUI-shaped flat `WebOccurrenceDto` the IPC commands expect.
  // Mirrors `src-tauri/src/web.rs::WebOccurrenceDto` field-by-field.
  function buildInput() {
    const occ = form.occurrence;
    // Plan v1: tz is system-local. The dialog form starts with an empty
    // string (placeholder placeholder). To match the CLI default
    // (resolves to system local at persistence time) and to keep the
    // IPC payload concrete, fill in Intl.DateTimeFormat().resolvedOptions
    // .timeZone when blank. On this box that is "Europe/Helsinki"; on
    // the auditor's machine "America/Los_Angeles". The user can still
    // override by typing an IANA name explicitly.
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
    const o = {
      occ: occ.kind,
      tz: tzName,
      // Encode weekly days as a `{mon:true, wed:false,...}` record so
      // the chrono core can round-trip. Other kinds null.
      days: occ.kind === 'weekly' ? weekdaysCsvToMap(occ.days) : null,
      // NaiveTime "HH:MM:SS" applies to daily/weekly/monthly/yearly.
      at:
        occ.kind === 'daily' ||
        occ.kind === 'weekly' ||
        occ.kind === 'monthly' ||
        occ.kind === 'yearly'
          ? occ.time || '09:00:00'
          : null,
      onceAt: occ.kind === 'once' ? occ.onceAt || null : null,
      everySecs: occ.kind === 'interval' ? occ.everySecs ?? 60 : null,
      // Preserve a stable-anchor interval verbatim. Null for new timers
      // (Rust falls back to `Utc::now()` inside `Interval` build).
      // `isEdit` is a top-level $derived(!!timer) — `form` has no such
      // property, so referencing `form.isEdit` here would always be
      // falsy and silently null out every Save, regressing the
      // stable anchor the auditor flagged in rework #3.
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
        args: form.launchArgs
          ? form.launchArgs.split(/\s+/).filter(Boolean)
          : [],
        // Persist workdir if the user typed one; null on no-op so the
        // core's Option<...> stays empty.
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

  // Convert `"mon,wed,fri"` -> `{ mon:true, tue:false, ..., sun:false }`.
  // Mirrors `src-tauri/src/web.rs::decode_days`.
  function weekdaysCsvToMap(csv) {
    const map = { mon: false, tue: false, wed: false, thu: false, fri: false, sat: false, sun: false };
    if (typeof csv !== 'string') return map;
    for (const tok of csv.split(/[,\s]+/)) {
      const k = tok.trim().toLowerCase();
      if (k in map) map[k] = true;
    }
    return map;
  }

  function buildPatch() {
    const input = buildInput();
    return {
      name: input.name,
      enabled: input.enabled,
      occurrence: input.occurrence,
      // Match the Rust `WebTimerPatchDto` wire contract exactly:
      // `actionKind`, not `action`.
      actionKind: input.action,
    };
  }

  function previewTimeout() {
    const my = ++previewToken;
    previewBusy = true;
    previewFires(buildInput().occurrence, 5)
      .then((r) => {
        if (my !== previewToken) return; // stale
        preview = r;
      })
      .catch((e) => {
        if (my !== previewToken) return;
        preview = { fires: [], warnings: [`Preview: ${e}` || ''] };
      })
      .finally(() => {
        if (my === previewToken) previewBusy = false;
      });
  }

  // Debounced live preview — fires ~250 ms after the user stops typing.
  let previewTimer = null;
  function schedulePreview() {
    if (previewTimer) clearTimeout(previewTimer);
    previewTimer = setTimeout(previewTimeout, 250);
  }
  $effect(() => {
    // Re-run preview whenever any relevant field changes. Serializing the
    // whole occurrence into JSON gives a stable dep-tracking key.
    JSON.stringify(form.occurrence);
    if (isTauri()) schedulePreview();
  });

  onDestroy(() => {
    if (previewTimer) clearTimeout(previewTimer);
  });

  async function save() {
    if (!form.name.trim()) {
      onToast('Name is required', 'err');
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
        <div class="form-row">
          <label for="td-name">Name</label>
          <input id="td-name" bind:value={form.name} placeholder="e.g. morning backup" />
        </div>

        <div class="form-row">
          <label for="td-kind">Occurrence kind</label>
          <!-- Short labels so the closed select value is fully readable at
               960×640 and 1280 (P4b S3). Kind-specific fields below carry the detail. -->
          <select id="td-kind" bind:value={form.occurrence.kind}>
            <option value="once">once</option>
            <option value="interval">interval</option>
            <option value="daily">daily</option>
            <option value="weekly">weekly</option>
            <option value="monthly">monthly</option>
            <option value="yearly">yearly</option>
            <option value="cron">cron</option>
          </select>
        </div>

        <div class="form-row">
          <label for="td-tz">Timezone (IANA, blank = system)</label>
          <input id="td-tz" bind:value={form.occurrence.tz} placeholder="Europe/Helsinki" />
        </div>

        {#if form.occurrence.kind === 'once'}
          <div class="form-row">
            <label for="td-once">When (YYYY-MM-DDTHH:MM:SS in tz)</label>
            <input id="td-once" bind:value={form.occurrence.onceAt} placeholder="2026-12-31T23:55:00" />
          </div>
        {/if}

        {#if form.occurrence.kind === 'interval'}
          <div class="form-row">
            <label for="td-every">Every (seconds)</label>
            <input id="td-every" type="number" min="1" bind:value={form.occurrence.everySecs} />
          </div>
        {/if}

        {#if ['daily', 'weekly', 'monthly', 'yearly'].includes(form.occurrence.kind)}
          <div class="form-row">
            <label for="td-time">Wall-clock time (HH:MM:SS)</label>
            <input id="td-time" bind:value={form.occurrence.time} placeholder="09:00:00" />
          </div>
        {/if}

        {#if form.occurrence.kind === 'weekly'}
          <div class="form-row">
            <label for="td-days">Weekdays (csv: mon,tue,wed,thu,fri,sat,sun)</label>
            <input id="td-days" bind:value={form.occurrence.days} placeholder="mon,wed,fri" />
          </div>
        {/if}

        {#if form.occurrence.kind === 'monthly'}
          <div class="form-row">
            <label for="td-day">Day of month (1–31, will clamp)</label>
            <input id="td-day" type="number" min="1" max="31" bind:value={form.occurrence.day} />
          </div>
        {/if}

        {#if form.occurrence.kind === 'yearly'}
          <div class="form-row">
            <label for="td-month">Month (1–12)</label>
            <input id="td-month" type="number" min="1" max="12" bind:value={form.occurrence.month} />
          </div>
          <div class="form-row">
            <label for="td-day2">Day of month (1–31, will clamp / Feb 29 leap-only)</label>
            <input id="td-day2" type="number" min="1" max="31" bind:value={form.occurrence.day} />
          </div>
        {/if}

        {#if form.occurrence.kind === 'cron'}
          <div class="form-row">
            <label for="td-cron">Cron expression (5- or 6-field)</label>
            <input id="td-cron" bind:value={form.occurrence.cronExpr} placeholder="*/5 * * * *" />
          </div>
        {/if}

        <fieldset class="action-set">
          <legend>Wake action</legend>
          <label class="radio">
            <input type="radio" bind:group={form.actionType} value="none" /> none
          </label>
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
        {#if preview.warnings && preview.warnings.length > 0}
          <div class="dst-warning">
            {#each preview.warnings as w, i}
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
                  <!-- Date+time in one cell so the preview needs only 3 value
                       columns and full UTC/offset stay readable (P4b R1). -->
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
        <button class="btn" onclick={() => onClose(false)} disabled={saveBusy}>Cancel</button>
        <button class="btn primary" onclick={save} disabled={saveBusy}>
          {saveBusy ? 'Saving…' : isEdit ? 'Save' : 'Create'}
        </button>
      </div>
    </footer>
  </div>
</div>
