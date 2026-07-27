<script>
  import { onMount, onDestroy } from 'svelte';
  import { isTauri, getPauseAll, setPauseAll, wizardStatus, appInfo, listen } from './api.js';
  import TimerList from './TimerList.svelte';
  import Wizard from './Wizard.svelte';
  import StubPage from './StubPage.svelte';

  let page = $state('all');          // 'all' | 'week' | 'month' | 'history'
  let pauseAll = $state(false);
  let info = $state(null);
  let wizardOpen = $state(false);
  let toasts = $state([]);

  function pushToast(text, kind = 'info', ttl = 3500) {
    const id = Math.random().toString(36).slice(2);
    toasts = [...toasts, { id, text, kind }];
    setTimeout(() => {
      toasts = toasts.filter((t) => t.id !== id);
    }, ttl);
  }

  async function refresh() {
    try {
      pauseAll = await getPauseAll();
      info = await appInfo();
    } catch (e) {
      pushToast(String(e), 'err');
    }
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
    pushToast('Settings saved');
    refresh();
  }

  onMount(async () => {
    await refresh();
    await checkWizard();
    // Subscribe to the pause-all event so the top-bar pill stays in sync
    // with whichever surface flipped the flag (window toggle OR tray
    // menu). Payload is a bool — consistent with the Tauri command.
    const unsubPause = listen('pause-all-changed', (e) => {
      const next = e?.payload;
      if (typeof next === 'boolean') {
        pauseAll = next;
      }
    });
    onDestroy(unsubPause);
  });
</script>

<div class="topbar">
  <div class="tabs">
    <button class="tab" class:active={page==='all'} onclick={() => page = 'all'}>All timers</button>
    <button class="tab" class:active={page==='week'} onclick={() => page = 'week'}>Week</button>
    <button class="tab" class:active={page==='month'} onclick={() => page = 'month'}>Month</button>
    <button class="tab" class:active={page==='history'} onclick={() => page = 'history'}>Run history</button>
  </div>
  <div class="topbar-right">
    {#if !isTauri}
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
    <TimerList onToast={pushToast} onPauseChange={(p) => pauseAll = p} />
  {:else if page === 'week'}
    <StubPage title="Week" hint="Weekly grid view lands in C8." />
  {:else if page === 'month'}
    <StubPage title="Month" hint="Monthly grid view lands in C8." />
  {:else if page === 'history'}
    <StubPage title="Run history" hint="Run-history view lands in C8." />
  {/if}
</main>

{#if wizardOpen}
  <Wizard onDone={onWizardDone} />
{/if}

<div class="toasts">
  {#each toasts as t (t.id)}
    <div class="toast" class:err={t.kind === 'err'}>{t.text}</div>
  {/each}
</div>
