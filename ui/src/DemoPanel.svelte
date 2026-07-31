<script>
  // WIZ1 demo panel — the whole surface of the demo offer, shared by the
  // wizard's completion step and the Settings demo section. It only ever
  // explains and launches; the demo app creates its own timer through the
  // slot protocol (Bellman never provisions it).
  import { demoLaunch, demoOpenDocs } from './api.js';

  let { info, onToast = null } = $props();

  let busy = $state(false);
  let copied = $state(false);

  async function copyCommand() {
    if (!info?.command) return;
    try {
      await navigator.clipboard.writeText(info.command);
      copied = true;
      onToast?.('Demo command copied');
      setTimeout(() => (copied = false), 2000);
    } catch (e) {
      onToast?.(String(e), 'err');
    }
  }

  async function runDemo() {
    busy = true;
    try {
      const pid = await demoLaunch();
      onToast?.(pid ? `Demo launched (pid ${pid})` : 'Demo launched');
    } catch (e) {
      onToast?.(String(e), 'err');
    } finally {
      busy = false;
    }
  }

  async function openDocs() {
    try {
      await demoOpenDocs();
    } catch (e) {
      onToast?.(String(e), 'err');
    }
  }
</script>

<div class="demo-panel" data-testid="demo-panel">
  <p>
    The lightbulb is a tiny example application that Bellman wakes on a
    schedule — set a time in its window and watch the bulb light when the
    timer fires. It talks to Bellman exactly the way your own applications
    can: over plain JSON files, with no plugin and no shared code.
  </p>

  {#if info?.command}
    <div class="demo-command">
      <input
        class="demo-command-input"
        data-testid="demo-command"
        type="text"
        readonly
        value={info.command}
        onfocus={(e) => e.target.select()}
      />
      <button class="btn" onclick={copyCommand} disabled={busy}>
        {copied ? 'Copied' : 'Copy'}
      </button>
    </div>
  {/if}

  {#if info?.canLaunch}
    <div class="demo-actions">
      <button
        class="btn primary"
        data-testid="demo-run"
        onclick={runDemo}
        disabled={busy}
      >
        {busy ? 'Launching…' : 'Run the demo'}
      </button>
    </div>
  {:else if info?.available && info?.python3Present && !info?.tkinterPresent}
    <p class="hint" data-testid="demo-tk-note">
      Running the demo needs Tkinter, packaged separately on Ubuntu/Debian:
      <code>sudo apt install python3-tk</code> — or copy the command above and
      run it yourself.
    </p>
  {:else if info?.available && !info?.python3Present}
    <p class="hint" data-testid="demo-tk-note">
      Running the demo needs Python 3 (<code>python3</code>, standard library
      only) — or copy the command above and run it yourself.
    </p>
  {:else if !info?.available}
    <p class="hint" data-testid="demo-unavailable">
      The demo files are not installed on this machine — they ship with the
      Linux package under <code>/usr/share/bellman/testing_apps/</code>.
    </p>
  {/if}

  {#if info?.docsPath}
    <button class="btn" data-testid="demo-docs" onclick={openDocs} disabled={busy}>
      Connect your own application (docs)
    </button>
  {/if}
</div>
