<script>
  import { invoke } from '@tauri-apps/api/core';
  import { open } from '@tauri-apps/plugin-dialog';
  let { isOpen = $bindable(false) } = $props();
  let streams = $state([]);
  let selected = $state([]);
  let directory = $state('');
  let label = $state('');
  let status = $state({ state: 'idle', committed_samples: 0, path: null, error: null });
  let error = $state('');
  let busy = $state(false);
  const active = $derived(['starting', 'recording', 'stopping'].includes(status.state));

  $effect(() => {
    if (!isOpen) return;
    let closed = false;
    let pending = false;
    async function refresh() {
      if (pending) return;
      pending = true;
      try {
        const next = await invoke('lsl_recording_status');
        if (!closed) status = next;
      } catch (e) {
        if (!closed) error = String(e);
      } finally { pending = false; }
    }
    refresh();
    const timer = setInterval(refresh, 1000);
    return () => { closed = true; clearInterval(timer); };
  });

  async function action(work) {
    busy = true; error = '';
    try { await work(); } catch (e) { error = String(e); }
    finally { busy = false; }
  }
  async function discover() {
    await action(async () => {
      streams = await invoke('discover_recording_streams');
      selected = selected.filter(uid => streams.some(s => s.uid === uid));
    });
  }
  async function chooseFolder() {
    await action(async () => {
      const choice = await open({ directory: true, multiple: false });
      if (typeof choice === 'string') directory = choice;
    });
  }
  async function start() {
    await action(async () => {
      status = await invoke('start_lsl_recording', { request: {
        stream_uids: [...selected], directory, context: { label: label || null },
      } });
    });
  }
  async function stop() {
    await action(async () => { status = await invoke('stop_lsl_recording'); });
  }
  async function recover() {
    await action(async () => {
      const path = await open({ multiple: false, filters: [{ name: 'Recording', extensions: ['sqlite'] }] });
      if (typeof path !== 'string') return;
      const saved = await invoke('recover_lsl_recording', { path });
      error = '';
      recoveredPath = saved;
    });
  }
  let recoveredPath = $state('');
</script>

{#if isOpen}
  <section class="recording-panel" aria-labelledby="recording-title">
    <div class="recording-heading">
      <h2 id="recording-title">Local recordings</h2>
      <button onclick={() => { isOpen = false; }}>Close</button>
    </div>
    <p>Save selected streams locally. Recording continues when this panel or the browser closes.</p>
    <p role="status"><strong>{status.state}</strong> · <span>{status.committed_samples.toLocaleString()} samples saved</span></p>
    {#if status.path}<p class="recording-path">{status.path}</p>{/if}
    {#if status.end_report}
      <p>File finalization and device timing are separate. Acquisition coverage is unverified.</p>
      {#if status.end_report.acquisition_coverage === 'not_requested'}
        <p>No source end boundary was requested; samples still in device processing may arrive after stop.</p>
      {/if}
      {#each status.end_report.streams as boundary (boundary.stream_uid)}
        <p class="recording-path">{boundary.stream_uid}: {boundary.outcome}</p>
      {/each}
    {/if}
    {#if error || status.error}<p role="alert">{error || status.error}</p>{/if}
    <label for="recording-directory">Recording folder</label>
    <div class="recording-row">
      <input id="recording-directory" bind:value={directory} disabled={active || busy} />
      <button onclick={chooseFolder} disabled={active || busy}>Choose folder</button>
    </div>
    <label for="recording-label">Recording label (optional)</label>
    <input id="recording-label" bind:value={label} disabled={active || busy} />
    <div class="recording-row">
      <button onclick={discover} disabled={active || busy}>Find streams</button>
      <span>{selected.length} selected</span>
    </div>
    <div class="recording-streams">
      {#each streams as stream (stream.uid)}
        <label class="recording-stream">
          <input type="checkbox" bind:group={selected} value={stream.uid} disabled={active || busy} />
          <span>{stream.name} <small>{stream.channels} channels · {stream.format} · {stream.nominal_rate} Hz</small></span>
        </label>
      {/each}
      {#if streams.length === 0}<p>Connect a device, then find its streams. Select event-marker streams too.</p>{/if}
    </div>
    <div class="recording-row">
      <button onclick={start} disabled={active || busy || !directory.trim() || !selected.length}>Start recording</button>
      <button onclick={stop} disabled={!active || busy}>Stop and save</button>
      <button onclick={recover} disabled={active || busy}>Recover interrupted recording</button>
    </div>
    {#if recoveredPath}<p class="recording-path">Recovered copy: {recoveredPath}</p>{/if}
    <p><small>Files stay on this computer. Cloud upload is not available yet.</small></p>
  </section>
{/if}

<style>
  .recording-panel { margin: 1rem; padding: 1.25rem; border: 1px solid var(--color-text-disabled); background: var(--color-surface); border-radius: 8px; }
  .recording-heading, .recording-row { display: flex; align-items: center; gap: .75rem; flex-wrap: wrap; margin: .75rem 0; }
  .recording-heading { justify-content: space-between; }
  .recording-streams { max-height: 20rem; overflow-y: auto; }
  .recording-stream { display: flex; align-items: center; gap: .5rem; padding: .4rem 0; }
  .recording-stream small { display: block; }
  .recording-path { overflow-wrap: anywhere; }
  #recording-directory { flex: 1; min-width: 15rem; }
  input:not([type='checkbox']) { padding: .5rem; color: inherit; background: transparent; border: 1px solid currentColor; border-radius: 4px; }
  button { padding: .5rem .75rem; cursor: pointer; }
  button:disabled { cursor: default; opacity: .55; }
</style>
