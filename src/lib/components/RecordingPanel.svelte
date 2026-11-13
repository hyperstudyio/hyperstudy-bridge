<script>
  import { invoke } from '@tauri-apps/api/core';
  import { open } from '@tauri-apps/plugin-dialog';
  import { documentDir, join } from '@tauri-apps/api/path';

  let { isOpen = $bindable(false) } = $props();

  const FOLDER_KEY = 'hyperstudy-bridge.recording-folder';
  const UPLOAD = {
    complete: { text: 'Uploaded', tone: 'done' },
    uploading: { text: 'Uploading', tone: 'busy' },
    verifying: { text: 'Checking upload', tone: 'busy' },
    pending: { text: 'Waiting to upload', tone: 'wait' },
    local: { text: 'On this Mac only', tone: 'quiet' },
  };

  let status = $state({ state: 'idle', committed_samples: 0, path: null, error: null, streams: 0 });
  let studies = $state([]);
  let streams = $state([]);
  let selected = $state([]);
  let directory = $state('');
  let label = $state('');
  let error = $state('');
  let pollError = $state('');
  let busy = $state(false);
  let searching = $state(false);
  let manualOpen = $state(false);
  let recoveredPath = $state('');

  // Study and manual recordings share one recorder, so its status alone cannot say whose recording it is. The
  // study registry can: a recorder file that belongs to a listed study is that study's.
  const active = $derived(['starting', 'recording', 'stopping'].includes(status.state));
  const liveStudy = $derived(
    studies.find(s => ['starting', 'recording', 'stopping'].includes(s.state))
  );
  const studyFile = $derived(!!status.path && studies.some(s => s.path === status.path));
  const manualActive = $derived(active && !liveStudy && !studyFile);
  const manualFile = $derived(!!status.path && !active && !studyFile);
  const newestFirst = $derived(
    [...studies].sort((a, b) => String(b.created_at).localeCompare(String(a.created_at)))
  );
  const canStart = $derived(!active && !busy && directory.trim() !== '' && selected.length > 0);

  $effect(() => {
    if (!isOpen) return;
    let closed = false;
    let pending = false;
    async function refresh() {
      if (pending) return;
      pending = true;
      try {
        const [next, sessions] = await Promise.all([
          invoke('lsl_recording_status'),
          invoke('list_study_recordings'),
        ]);
        if (!closed) {
          status = next;
          studies = sessions;
          pollError = '';
        }
      } catch (e) {
        if (!closed) pollError = String(e);
      } finally {
        pending = false;
      }
    }
    refresh();
    const timer = setInterval(refresh, 1000);
    return () => {
      closed = true;
      clearInterval(timer);
    };
  });

  // Fill the folder once; a field the user cleared must stay clear.
  let folderFilled = false;
  $effect(() => {
    if (!isOpen || folderFilled) return;
    folderFilled = true;
    defaultFolder().then(folder => {
      if (!directory) directory = folder;
    });
  });

  async function defaultFolder() {
    try {
      const saved = localStorage.getItem(FOLDER_KEY);
      if (saved) return saved;
    } catch {
      /* storage unavailable: fall through to the default */
    }
    try {
      return await join(await documentDir(), 'HyperStudy Bridge Recordings');
    } catch {
      return '';
    }
  }

  function rememberFolder(folder) {
    try {
      localStorage.setItem(FOLDER_KEY, folder);
    } catch {
      /* per-device convenience only */
    }
  }

  async function action(work) {
    busy = true;
    error = '';
    try {
      await work();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function findStreams() {
    searching = true;
    error = '';
    try {
      const found = await invoke('discover_recording_streams');
      streams = [...found].sort((a, b) => a.name.localeCompare(b.name));
      selected = selected.filter(uid => streams.some(s => s.uid === uid));
    } catch (e) {
      error = String(e);
    } finally {
      searching = false;
    }
  }

  function toggleManual(event) {
    manualOpen = event.currentTarget.open;
    if (manualOpen && !streams.length && !active) findStreams();
  }

  async function chooseFolder() {
    await action(async () => {
      const choice = await open({
        directory: true,
        multiple: false,
        defaultPath: directory || undefined,
      });
      if (typeof choice === 'string') {
        directory = choice;
        rememberFolder(choice);
      }
    });
  }

  async function start() {
    await action(async () => {
      rememberFolder(directory.trim());
      status = await invoke('start_lsl_recording', {
        request: {
          stream_uids: [...selected],
          directory: directory.trim(),
          context: { label: label || null },
        },
      });
    });
  }

  async function stop() {
    await action(async () => {
      status = await invoke('stop_lsl_recording');
    });
  }

  async function reveal(recordingId = null) {
    await action(() => invoke('reveal_recording', { recordingId }));
  }

  async function recover() {
    await action(async () => {
      const path = await open({
        multiple: false,
        filters: [{ name: 'Recording', extensions: ['sqlite'] }],
      });
      if (typeof path !== 'string') return;
      recoveredPath = await invoke('recover_lsl_recording', { path });
    });
  }

  function when(iso) {
    const date = new Date(iso);
    if (Number.isNaN(date.getTime())) return { day: '', time: '' };
    const sameYear = date.getFullYear() === new Date().getFullYear();
    return {
      day: date.toLocaleDateString(undefined, {
        month: 'short',
        day: 'numeric',
        ...(sameYear ? {} : { year: 'numeric' }),
      }),
      time: date.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' }),
    };
  }

  const short = (id, n = 8) => (id && id.length > n ? id.slice(0, n) : id || '');
  const tail = (id, n = 6) => (id && id.length > n ? id.slice(-n) : id || '');
  const count = n => Number(n || 0).toLocaleString();
  const plural = (n, word) => `${count(n)} ${word}${Number(n) === 1 ? '' : 's'}`;

  function upload(study) {
    if (study.state === 'failed' || study.state === 'interrupted')
      return { text: 'Recording stopped early', tone: 'fail' };
    return UPLOAD[study.upload_status] ?? { text: study.upload_status, tone: 'quiet' };
  }

  function close() {
    isOpen = false;
  }
</script>

{#if isOpen}
  <div
    class="rec-overlay"
    role="presentation"
    onclick={close}
    onkeydown={e => e.key === 'Escape' && close()}
  >
    <div
      class="rec-dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby="rec-title"
      tabindex="-1"
      onclick={e => e.stopPropagation()}
      onkeydown={e => e.key === 'Escape' && close()}
    >
      <header class="rec-header">
        <h2 id="rec-title">Recordings</h2>
        <button class="rec-close" aria-label="Close recordings" onclick={close}>
          <svg
            width="20"
            height="20"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            aria-hidden="true"
          >
            <line x1="18" y1="6" x2="6" y2="18"></line><line x1="6" y1="6" x2="18" y2="18"></line>
          </svg>
        </button>
      </header>

      {#if manualActive}
        <div class="rec-live" role="status">
          <span class="rec-live-dot" aria-hidden="true"></span>
          <div class="rec-live-text">
            <strong>{status.state === 'stopping' ? 'Saving recording' : 'Recording'}</strong>
            <span
              >{plural(status.committed_samples, 'sample')} saved from {plural(
                status.streams,
                'stream'
              )}</span
            >
          </div>
          <button class="rec-stop" onclick={stop} disabled={busy || status.state === 'stopping'}
            >Stop and save</button
          >
        </div>
      {:else if liveStudy}
        <div class="rec-live" role="status">
          <span class="rec-live-dot" aria-hidden="true"></span>
          <div class="rec-live-text">
            <strong>Study recording</strong>
            <span
              >{plural(liveStudy.committed_samples, 'sample')} saved. HyperStudy stops it when the study
              ends.</span
            >
          </div>
        </div>
      {/if}

      <div class="rec-body">
        {#if error || pollError || status.error}<p class="rec-alert" role="alert">
            {error || pollError || status.error}
          </p>{/if}

        <section aria-labelledby="study-title">
          <h3 id="study-title">Study recordings</h3>
          <p class="rec-lede">
            HyperStudy records connected devices during a study and uploads the file when the study
            ends. The file stays on this Mac too.
          </p>

          {#if newestFirst.length === 0}
            <p class="rec-empty">
              No study recordings yet. They appear here once a study records a connected device.
            </p>
          {:else}
            <ol class="study-list">
              {#each newestFirst as study (study.recording_id)}
                {@const at = when(study.created_at)}
                {@const state = upload(study)}
                <li class="study-row">
                  <time class="study-when" datetime={study.created_at}>
                    <span class="study-day">{at.day}</span>
                    <span class="study-time">{at.time}</span>
                  </time>
                  <div class="study-what">
                    <p class="study-name">
                      {study.context?.label || `Experiment ${short(study.context?.experiment_id)}`}
                    </p>
                    <p class="study-ids">
                      {#if study.context?.room_id}<span>Room {tail(study.context.room_id)}</span
                        >{/if}
                      {#if study.context?.participant_id}<span
                          >Participant {short(study.context.participant_id, 6)}</span
                        >{/if}
                    </p>
                    {#if study.upload_error || study.error}
                      <p class="study-note">{study.upload_error || study.error}</p>
                    {/if}
                  </div>
                  <div class="study-size">
                    <span>{plural(study.committed_samples, 'sample')}</span>
                    <span>{plural(study.streams, 'stream')}</span>
                  </div>
                  <div class="study-actions">
                    <span class="study-state" data-tone={state.tone}>{state.text}</span>
                    <button
                      class="rec-link"
                      onclick={() => reveal(study.recording_id)}
                      disabled={busy}>Show in Finder</button
                    >
                  </div>
                </li>
              {/each}
            </ol>
          {/if}
          {#if newestFirst.some(s => s.upload_status === 'pending')}
            <p class="rec-hint">
              To upload a waiting recording, open its study in HyperStudy again with Bridge running.
            </p>
          {/if}
        </section>

        <details class="manual" open={manualOpen || manualActive} ontoggle={toggleManual}>
          <summary>
            <span class="manual-title">Record streams yourself</span>
            <span class="manual-sub"
              >Save any LSL streams on this network to a file, without a study.</span
            >
          </summary>

          <div class="manual-body">
            <label class="rec-label" for="recording-directory">Recording folder</label>
            <div class="rec-row">
              <input
                id="recording-directory"
                class="rec-input"
                bind:value={directory}
                disabled={active || busy}
              />
              <button class="rec-quiet-btn" onclick={chooseFolder} disabled={active || busy}
                >Choose folder</button
              >
            </div>

            <div class="stream-head">
              <span class="rec-label" id="stream-list-label">
                {searching ? 'Looking for streams' : `Streams on this network (${streams.length})`}
              </span>
              <div class="stream-tools">
                {#if streams.length}
                  <button
                    class="rec-link"
                    onclick={() => (selected = streams.map(s => s.uid))}
                    disabled={active || busy}>Select all</button
                  >
                  <button
                    class="rec-link"
                    onclick={() => (selected = [])}
                    disabled={active || busy || !selected.length}>Clear</button
                  >
                {/if}
                <button
                  class="rec-quiet-btn"
                  onclick={findStreams}
                  disabled={active || busy || searching}>Find streams</button
                >
              </div>
            </div>
            <div class="stream-list" role="group" aria-labelledby="stream-list-label">
              {#each streams as stream (stream.uid)}
                <label class="stream-row">
                  <input
                    type="checkbox"
                    bind:group={selected}
                    value={stream.uid}
                    disabled={active || busy}
                  />
                  <span class="stream-name">{stream.name}</span>
                  <span class="stream-meta">{stream.channels} ch</span>
                  <span class="stream-meta"
                    >{stream.nominal_rate ? `${stream.nominal_rate} Hz` : 'irregular'}</span
                  >
                </label>
              {/each}
              {#if !streams.length && !searching}
                <p class="rec-empty">
                  No streams found. Connect a device on the main screen, then find streams again.
                  Include marker streams to keep event timing.
                </p>
              {/if}
            </div>

            <label class="rec-label" for="recording-label">Label (optional)</label>
            <input
              id="recording-label"
              class="rec-input"
              bind:value={label}
              disabled={active || busy}
              placeholder="Pilot session 3"
            />

            <div class="rec-row rec-start-row">
              <button class="rec-start" onclick={start} disabled={!canStart}>
                {selected.length
                  ? `Start recording ${plural(selected.length, 'stream')}`
                  : 'Start recording'}
              </button>
              <span class="rec-hint">
                {active && !manualActive
                  ? 'A study is recording. You can start your own recording when it ends.'
                  : 'Recording continues if you close this window.'}
              </span>
            </div>

            {#if manualFile}
              <div class="manual-result">
                <p>
                  <strong>Last recording:</strong>
                  {plural(status.committed_samples, 'sample')} saved
                </p>
                <p class="rec-path">{status.path}</p>
                {#if status.end_report}
                  <p class="rec-hint">
                    The file is complete. Acquisition coverage is unverified: Bridge cannot confirm
                    each device had sent its last samples when you stopped.
                  </p>
                  {#each status.end_report.streams as boundary (boundary.stream_uid)}
                    <p class="rec-path">{boundary.stream_uid}: {boundary.outcome}</p>
                  {/each}
                {/if}
                <button class="rec-quiet-btn" onclick={() => reveal(null)} disabled={busy}
                  >Show in Finder</button
                >
              </div>
            {/if}

            <div class="manual-recover">
              <button class="rec-link" onclick={recover} disabled={active || busy}
                >Repair an interrupted recording</button
              >
              {#if recoveredPath}<p class="rec-path">Repaired copy saved to {recoveredPath}</p>{/if}
            </div>
          </div>
        </details>
      </div>
    </div>
  </div>
{/if}

<style>
  .rec-overlay {
    position: fixed;
    inset: 0;
    z-index: 1000;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(0, 0, 0, 0.8);
    backdrop-filter: blur(4px);
  }
  .rec-dialog {
    display: flex;
    flex-direction: column;
    overflow: hidden;
    width: 90vw;
    max-width: 820px;
    max-height: 85vh;
    background: var(--color-surface);
    border: 1px solid var(--color-border);
    border-radius: 12px;
    font-variant-numeric: tabular-nums;
  }
  .rec-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 1.25rem 1.5rem;
    border-bottom: 1px solid var(--color-border);
  }
  .rec-header h2 {
    font-size: 1.25rem;
    font-weight: 600;
  }
  .rec-close {
    display: grid;
    place-items: center;
    width: 2rem;
    height: 2rem;
    border-radius: 6px;
    background: transparent;
    color: var(--color-text-secondary);
  }
  .rec-close:hover {
    color: var(--color-text-primary);
    background: var(--color-surface-elevated);
  }

  .rec-live {
    display: flex;
    align-items: center;
    gap: 0.875rem;
    padding: 0.875rem 1.5rem;
    background: rgba(76, 175, 80, 0.12);
    border-bottom: 1px solid rgba(76, 175, 80, 0.35);
  }
  .rec-live-dot {
    width: 0.625rem;
    height: 0.625rem;
    border-radius: 50%;
    flex-shrink: 0;
    background: var(--color-primary);
    animation: rec-pulse 1.6s ease-in-out infinite;
  }
  .rec-live-text {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
    font-size: 0.8125rem;
    color: var(--color-text-secondary);
  }
  .rec-live-text strong {
    font-size: 0.9375rem;
    color: var(--color-text-primary);
  }
  .rec-stop {
    padding: 0.5rem 1rem;
    border-radius: 6px;
    font-size: 0.875rem;
    background: var(--color-text-primary);
    color: #000;
  }
  @keyframes rec-pulse {
    50% {
      opacity: 0.35;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .rec-live-dot {
      animation: none;
    }
  }

  .rec-body {
    overflow-y: auto;
    padding: 1.5rem;
    display: flex;
    flex-direction: column;
    gap: 2rem;
  }
  .rec-body h3 {
    font-size: 0.9375rem;
    font-weight: 600;
  }
  .rec-lede,
  .rec-hint,
  .rec-empty {
    font-size: 0.8125rem;
    color: var(--color-text-secondary);
  }
  .rec-lede {
    margin-top: 0.25rem;
    max-width: 60ch;
  }
  .rec-empty {
    padding: 1rem 0;
  }
  .rec-hint {
    margin-top: 0.5rem;
  }
  .rec-alert {
    padding: 0.625rem 0.875rem;
    border-radius: 6px;
    font-size: 0.875rem;
    background: rgba(239, 68, 68, 0.12);
    border: 1px solid rgba(239, 68, 68, 0.4);
    color: #fca5a5;
  }

  .study-list {
    list-style: none;
    margin-top: 0.875rem;
    border-top: 1px solid var(--color-border);
  }
  .study-row {
    display: grid;
    grid-template-columns: 4.75rem minmax(0, 1fr) auto auto;
    gap: 1.25rem;
    align-items: start;
    padding: 0.875rem 0;
    border-bottom: 1px solid var(--color-border);
  }
  .study-when {
    display: flex;
    flex-direction: column;
  }
  .study-day {
    font-size: 0.9375rem;
    font-weight: 600;
  }
  .study-time {
    font-size: 0.8125rem;
    color: var(--color-text-secondary);
  }
  .study-name {
    font-size: 0.9375rem;
    overflow-wrap: anywhere;
  }
  .study-ids {
    display: flex;
    flex-wrap: wrap;
    gap: 0 1rem;
    font-size: 0.8125rem;
    color: var(--color-text-secondary);
  }
  .study-note {
    margin-top: 0.25rem;
    font-size: 0.8125rem;
    color: var(--color-warning);
  }
  .study-size {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    font-size: 0.8125rem;
    color: var(--color-text-secondary);
    white-space: nowrap;
  }
  .study-size span:first-child {
    color: var(--color-text-primary);
  }
  .study-actions {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 0.125rem;
  }
  .study-actions .rec-link {
    padding: 0.125rem 0;
  }
  .study-state {
    font-size: 0.8125rem;
    font-weight: 500;
    white-space: nowrap;
  }
  .study-state[data-tone='done'] {
    color: var(--color-success);
  }
  .study-state[data-tone='busy'] {
    color: var(--color-info);
  }
  .study-state[data-tone='wait'] {
    color: var(--color-warning);
  }
  .study-state[data-tone='fail'] {
    color: var(--color-error);
  }
  .study-state[data-tone='quiet'] {
    color: var(--color-text-secondary);
  }

  .manual {
    border: 1px solid var(--color-border);
    border-radius: 10px;
  }
  .manual summary {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    cursor: pointer;
    list-style: none;
    padding: 1rem 1.25rem 1rem 2.5rem;
    position: relative;
  }
  .manual summary::-webkit-details-marker {
    display: none;
  }
  .manual summary::before {
    content: '';
    position: absolute;
    left: 1.25rem;
    top: 1.45rem;
    width: 0.4rem;
    height: 0.4rem;
    border-right: 2px solid var(--color-text-secondary);
    border-bottom: 2px solid var(--color-text-secondary);
    transform: rotate(-45deg);
    transition: transform 0.15s ease;
  }
  .manual[open] summary::before {
    transform: rotate(45deg);
  }
  .manual summary:focus-visible {
    outline: 2px solid var(--color-primary);
    outline-offset: -2px;
    border-radius: 10px;
  }
  .manual-title {
    font-size: 0.9375rem;
    font-weight: 600;
  }
  .manual-sub {
    font-size: 0.8125rem;
    color: var(--color-text-secondary);
  }
  .manual-body {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 0 1.25rem 1.25rem;
  }

  .rec-label {
    font-size: 0.8125rem;
    font-weight: 500;
    color: var(--color-text-secondary);
    margin-top: 0.75rem;
  }
  .rec-row {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    flex-wrap: wrap;
  }
  .rec-input {
    flex: 1;
    min-width: 14rem;
    padding: 0.5rem 0.75rem;
    font: inherit;
    font-size: 0.875rem;
    color: inherit;
    background: var(--color-background);
    border: 1px solid var(--color-border);
    border-radius: 6px;
  }
  .rec-input:focus {
    outline: 2px solid var(--color-primary);
    outline-offset: -1px;
  }
  .rec-input::placeholder {
    color: var(--color-text-disabled);
  }
  .rec-quiet-btn {
    padding: 0.375rem 0.75rem;
    border-radius: 6px;
    font-size: 0.8125rem;
    white-space: nowrap;
    background: var(--color-surface-elevated);
    color: var(--color-text-primary);
    border: 1px solid var(--color-border);
  }
  .rec-quiet-btn:hover:not(:disabled) {
    border-color: var(--color-border-hover);
  }
  .rec-link {
    padding: 0.25rem 0.375rem;
    font-size: 0.8125rem;
    background: transparent;
    color: var(--color-text-secondary);
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .rec-link:hover:not(:disabled) {
    color: var(--color-text-primary);
  }

  .stream-head {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 0.75rem;
    flex-wrap: wrap;
  }
  .stream-tools {
    display: flex;
    align-items: center;
    gap: 0.25rem;
  }
  .stream-list {
    max-height: 16rem;
    overflow-y: auto;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    background: var(--color-background);
  }
  .stream-list .rec-empty {
    padding: 0.875rem;
  }
  .stream-row {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) 3.5rem 5.5rem;
    gap: 0.75rem;
    align-items: center;
    padding: 0.4rem 0.75rem;
    font-size: 0.875rem;
    cursor: pointer;
  }
  .stream-row + .stream-row {
    border-top: 1px solid var(--color-border);
  }
  .stream-row:hover {
    background: rgba(255, 255, 255, 0.03);
  }
  .stream-row input {
    accent-color: var(--color-primary);
  }
  .stream-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .stream-meta {
    font-size: 0.8125rem;
    color: var(--color-text-secondary);
    text-align: right;
  }

  .rec-start-row {
    margin-top: 1rem;
  }
  .rec-start {
    padding: 0.625rem 1.125rem;
    border-radius: 6px;
    font-size: 0.875rem;
    background: var(--color-primary);
    color: #fff;
  }
  .rec-start:hover:not(:disabled) {
    background: var(--color-primary-hover);
  }
  .rec-start-row .rec-hint {
    margin-top: 0;
  }

  .manual-result {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.375rem;
    margin-top: 1rem;
    padding: 0.875rem 1rem;
    border-radius: 6px;
    background: var(--color-surface-elevated);
    font-size: 0.875rem;
  }
  .rec-path {
    font-size: 0.8125rem;
    color: var(--color-text-secondary);
    overflow-wrap: anywhere;
  }
  .manual-recover {
    margin-top: 1rem;
  }

  @media (max-width: 640px) {
    .study-row {
      grid-template-columns: 4.25rem minmax(0, 1fr);
      row-gap: 0.375rem;
    }
    .study-size,
    .study-actions {
      grid-column: 2;
      flex-direction: row;
      align-items: baseline;
      gap: 1rem;
    }
  }
</style>
