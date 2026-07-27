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
    return {
      name: '',
      enabled: true,
      occurrence: {
        kind: 'daily',
        tz: '',
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
      notifyTitle: '',
      notifyBody: '',
    };
  }

  function loadFromTimer(t) {
    const occ = t.occurrence || {};
    form = {
      name: t.name || '',
      enabled: !!t.enabled,
      occurrence: {
        kind: occ.kind || 'daily',
        tz: occ.tz || '',
        time: occ.time || '09:00:00',
        onceAt: occ.onceAt || '',
        everySecs: occ.everySecs ?? 300,
        intervalAnchor: occ.intervalAnchor || null,
        days: occ.days || 'mon,wed,fri',
        day: occ.day ?? 1,
        month: occ.month ?? 1,
        cronExpr: occ.cronExpr || '',
      },
      actionType: occ.actionType || 'none',
      launchCommand: occ.launchCommand || '',
      launchArgs: occ.launchArgs || '',
      notifyTitle: occ.notifyTitle || '',
      notifyBody: occ.notifyBody || '',
    };
  }

  $effect(() => {
    if (timer) loadFromTimer(timer);
    else form = emptyForm();
  });

  let isEdit = $derived(!!timer);

  // Build the Rust-shaped OccurrenceInput / CreateTimerInput the IPC expects.
  function buildInput() {
    const occ = form.occurrence;
    const o = {
      kind: occ.kind,
      tz: occ.tz || null,
      time: occ.time || null,
      onceAt: occ.onceAt || null,
      everySecs: occ.everySecs ?? null,
      intervalAnchor: null, // omit on edit; new timers use now()
      days: occ.days || null,
      day: occ.day ?? null,
      month: occ.month ?? null,
      cronExpr: occ.cronExpr || null,
    };
    let action = { type: 'none' };
    if (form.actionType === 'launch') {
      action = {
        type: 'launch',
        command: form.launchCommand,
        args: form.launchArgs
          ? form.launchArgs.split(/\s+/).filter(Boolean)
          : [],
        workdir: null,
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
      // Misfire/overlap/retry are intentionally left out — Rust defaults to
      // the product presets via NewTimer::new(); the dialog only exposes the
      // knobs users tend to change.
      misfire: null,
      overlap: null,
      retry: null,
      tags: [],
    };
  }

  function buildPatch() {
    const input = buildInput();
    return {
      name: input.name,
      enabled: input.enabled,
      occurrence: input.occurrence,
      action: input.action,
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
          <select id="td-kind" bind:value={form.occurrence.kind}>
            <option value="once">once — one-shot at a specific datetime</option>
            <option value="interval">interval — every N seconds</option>
            <option value="daily">daily — every day at a wall-clock time</option>
            <option value="weekly">weekly — chosen weekdays at a time</option>
            <option value="monthly">monthly — day-of-month at a time</option>
            <option value="yearly">yearly — month/day at a time</option>
            <option value="cron">cron — power-user expression</option>
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
                <th>Local time</th>
                <th>Date</th>
                <th>UTC</th>
                <th>Offset</th>
              </tr>
            </thead>
            <tbody>
              {#each preview.fires as f, i}
                <tr>
                  <td>{i + 1}</td>
                  <td>{f.localTime}</td>
                  <td>{f.localDate}</td>
                  <td class="mono">{new Date(f.utc).toLocaleString()}</td>
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
