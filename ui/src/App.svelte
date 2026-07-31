<script>
  import { onMount, onDestroy } from 'svelte';
  import { isTauri, getPauseAll, setPauseAll, wizardStatus, appInfo, listen, listTimers } from './api.js';
  import TimerList from './TimerList.svelte';
  import Wizard from './Wizard.svelte';
  import WeekPage from './WeekPage.svelte';
  import MonthPage from './MonthPage.svelte';
  import HistoryPage from './HistoryPage.svelte';
  import SettingsPage from './SettingsPage.svelte';
  import TimerDialog from './TimerDialog.svelte';

  let page = $state('all');          // 'all' | 'week' | 'month' | 'history' | 'settings'
  let pauseAll = $state(false);
  let info = $state(null);
  let wizardOpen = $state(false);
  /** Bumped when the wizard closes so a mounted Settings page remounts and
   *  re-reads choices the wizard just changed (e.g. the WIZ1 demo opt-in). */
  let settingsTick = $state(0);
  let toasts = $state([]);
  let editingTimer = $state(null);   // null = closed, {} = new, timer = edit
  /** YYYY-MM-DD when create was opened from a calendar day cell. */
  let createInitialDate = $state(null);
  let timers = $state([]);           // cached for week / month / dialog prefill
  let refreshTick = $state(0);

  async function refreshTimers() {
    try {
      timers = await listTimers();
    } catch (e) {
      // vite dev (no backend) — leave empty.
      timers = [];
    }
  }

  async function refresh() {
    try {
      pauseAll = await getPauseAll();
      info = await appInfo();
    } catch (e) {
      pushToast(String(e), 'err');
    }
  }

  function pushToast(text, kind = 'info', ttl = 3500) {
    const id = Math.random().toString(36).slice(2);
    toasts = [...toasts, { id, text, kind }];
    setTimeout(() => {
      toasts = toasts.filter((t) => t.id !== id);
    }, ttl);
  }

  async function togglePause() {
    const next = !pauseAll;
    pauseAll = next;
    try {
      await setPauseAll(next);
      pushToast(next ? 'All timers paused' : 'Timers resumed');
    } catch (e) {
      pauseAll = !next; // revert
      pushToast(String(e), 'err');
    }
  }

  async function checkWizard() {
    try {
      const s = await wizardStatus();
      if (!s.completed) wizardOpen = true;
    } catch (e) {
      // No Tauri (vite dev) — leave wizard closed.
    }
  }

  function onWizardDone() {
    wizardOpen = false;
    settingsTick++;
    pushToast('Settings saved');
    refresh();
  }

  function openCreate() {
    createInitialDate = null;
    editingTimer = {};
  }
  /** Open New timer pre-filled for a calendar day (YYYY-MM-DD). */
  function openCreateOnDate(isoDate) {
    createInitialDate = isoDate || null;
    editingTimer = {};
  }
  function openEdit(t) {
    createInitialDate = null;
    editingTimer = t;
  }
  async function dialogClosed(saved) {
    editingTimer = null;
    createInitialDate = null;
    if (saved) {
      await refreshTimers();
      refreshTick++;
    }
  }

  // Subscribe to backend events. We subscribe SYNCHRONOUSLY in onMount
  // (before any awaits) so the unsub closure is registered with
  // onDestroy before anything else can run; once the listen promise
  // settles we swap the placeholder for the real unsubscriber. If the
  // component is destroyed before the promise resolves, the placeholder
  // cancels the listener when it eventually arrives.
  const _unsubPausePlaceholder = { called: false, realUnsub: null };
  function unsubPause() {
    _unsubPausePlaceholder.called = true;
    if (_unsubPausePlaceholder.realUnsub) {
      const u = _unsubPausePlaceholder.realUnsub;
      _unsubPausePlaceholder.realUnsub = null;
      u();
    }
  }
  onDestroy(unsubPause);

  onMount(async () => {
    await refresh();
    await checkWizard();
    await refreshTimers();
    listen('pause-all-changed', (e) => {
      const next = e?.payload;
      if (typeof next === 'boolean') {
        pauseAll = next;
      }
    }).then((realUnsub) => {
      if (_unsubPausePlaceholder.called) {
        // Component already destroyed — tear the listener down.
        try { realUnsub(); } catch { /* ignore */ }
      } else {
        _unsubPausePlaceholder.realUnsub = realUnsub;
      }
    });
    // Fires whenever the scheduler lands a fire (RunNow triggers this too).
    listen('timer-fired', () => {
      refreshTimers().catch(() => {});
    }).then((u) => { if (_unsubPausePlaceholder.called) { try { u(); } catch {} } else { _unsubPausePlaceholder.realUnsub = u; } });
  });

  // Derived: when the active page is week/month, hand them the cached list
  // so they re-render without an extra IPC roundtrip. Tick is appended as
  // a $effect dep via `void tick` inside each page so refreshes propagate.
</script>

<div class="topbar">
  <div class="tabs">
    <button class="tab" class:active={page==='all'} onclick={() => page = 'all'}>All timers</button>
    <button class="tab" class:active={page==='week'} onclick={() => page = 'week'}>Week</button>
    <button class="tab" class:active={page==='month'} onclick={() => page = 'month'}>Month</button>
    <button class="tab" class:active={page==='history'} onclick={() => page = 'history'}>Run history</button>
    <button class="tab" class:active={page==='settings'} onclick={() => page = 'settings'}>Settings</button>
  </div>
  <div class="topbar-right">
    {#if !isTauri()}
      <span title="Tauri IPC not available">vite dev (no backend)</span>
    {/if}
    <button class="pause-toggle" class:paused={pauseAll} onclick={togglePause}>
      <span class="pause-dot"></span>
      {pauseAll ? 'Paused' : 'Running'}
    </button>
  </div>
</div>

<main>
  {#if page === 'all'}
    <TimerList
      onToast={pushToast}
      onPauseChange={(p) => pauseAll = p}
      onEdit={openEdit}
      onCreate={openCreate}
    />
  {:else if page === 'week'}
    <WeekPage onEdit={openEdit} onCreateDate={openCreateOnDate} onToast={pushToast} timers={timers} tick={refreshTick} />
  {:else if page === 'month'}
    <MonthPage onEdit={openEdit} onCreateDate={openCreateOnDate} onToast={pushToast} timers={timers} tick={refreshTick} />
  {:else if page === 'history'}
    <HistoryPage onToast={pushToast} />
  {:else if page === 'settings'}
    {#key settingsTick}
      <SettingsPage
        onToast={pushToast}
        onRerunWizard={() => { wizardOpen = true; }}
      />
    {/key}
  {/if}
</main>

{#if wizardOpen}
  <Wizard onDone={onWizardDone} />
{/if}

{#if editingTimer !== null}
  <TimerDialog
    timer={editingTimer && editingTimer.id ? editingTimer : null}
    initialDate={editingTimer && editingTimer.id ? null : createInitialDate}
    onClose={dialogClosed}
    onToast={pushToast}
  />
{/if}

<div class="toasts" role="status" aria-live="polite">
  {#each toasts as t (t.id)}
    <div class="toast" class:err={t.kind === 'err'} role={t.kind === 'err' ? 'alert' : 'status'}>
      <span class="toast-badge">{t.kind === 'err' ? '⚠ Error' : 'ℹ Info'}</span>
      <span class="toast-text">{t.text}</span>
    </div>
  {/each}
</div>
