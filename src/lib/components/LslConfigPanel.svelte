<script>
  import { invoke } from '@tauri-apps/api/core';

  let { isOpen = $bindable(false) } = $props();

  // Bridge's own inlets/outlets are cheap to read; the network search takes about a second, so it runs less often.
  const BRIDGE_POLL_MS = 1000;
  const NETWORK_POLL_MS = 5000;
  const STALE_AFTER_S = 3;

  let receiving = $state([]);
  let publishing = $state([]);
  let network = $state([]);
  let searched = $state(false);
  let error = $state('');
  let filter = $state('');

  const receivedUids = $derived(new Set(receiving.map(s => s.uid)));
  const rateByUid = $derived(new Map(network.map(s => [s.uid, Number(s.nominal_rate) || 0])));
  const publishedNames = $derived(new Set(publishing.map(s => s.name)));
  const visibleNetwork = $derived(
    network.filter(s => !filter || s.name.toLowerCase().includes(filter.toLowerCase()))
  );

  $effect(() => {
    if (!isOpen) return;
    let closed = false;
    let bridgeBusy = false;
    let networkBusy = false;

    async function readBridge() {
      if (bridgeBusy) return;
      bridgeBusy = true;
      try {
        const streams = await invoke('lsl_bridge_streams');
        if (!closed) {
          receiving = streams.receiving ?? [];
          publishing = streams.publishing ?? [];
        }
      } catch (e) {
        if (!closed) error = String(e);
      } finally {
        bridgeBusy = false;
      }
    }

    async function searchNetwork() {
      if (networkBusy) return;
      networkBusy = true;
      try {
        const found = await invoke('discover_recording_streams');
        if (!closed) {
          network = [...found].sort((a, b) => a.name.localeCompare(b.name));
          searched = true;
        }
      } catch (e) {
        if (!closed) error = String(e);
      } finally {
        networkBusy = false;
      }
    }

    readBridge();
    searchNetwork();
    const bridgeTimer = setInterval(readBridge, BRIDGE_POLL_MS);
    const networkTimer = setInterval(searchNetwork, NETWORK_POLL_MS);
    return () => {
      closed = true;
      clearInterval(bridgeTimer);
      clearInterval(networkTimer);
    };
  });

  const count = n => Number(n || 0).toLocaleString();

  // A regular stream is late after a few of its own sample periods. An irregular one (scores, sleep stage) can be
  // quiet for long stretches by design, so its gap is reported without calling it a fault.
  function health(stream) {
    if (!stream.sample_count) return { text: 'Waiting for data', tone: 'wait' };
    const since = Number(stream.seconds_since_last_receive || 0);
    const rate = rateByUid.get(stream.uid) || 0;
    if (since <= Math.max(STALE_AFTER_S, rate ? 3 / rate : 0))
      return { text: 'Live', tone: 'live' };
    const gap = `${Math.round(since)} s`;
    return rate
      ? { text: `No data for ${gap}`, tone: 'fail' }
      : { text: `Last data ${gap} ago`, tone: 'quiet' };
  }

  function rate(stream) {
    return stream.nominal_rate ? `${stream.nominal_rate} Hz` : 'irregular';
  }

  function close() {
    isOpen = false;
  }
</script>

{#if isOpen}
  <div
    class="lsl-overlay"
    role="presentation"
    onclick={close}
    onkeydown={e => e.key === 'Escape' && close()}
  >
    <div
      class="lsl-dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby="lsl-title"
      tabindex="-1"
      onclick={e => e.stopPropagation()}
      onkeydown={e => e.key === 'Escape' && close()}
    >
      <header class="lsl-header">
        <div>
          <h2 id="lsl-title">LSL streams</h2>
          <p class="lsl-sub">Updates on its own while this window is open.</p>
        </div>
        <button class="lsl-close" aria-label="Close LSL streams" onclick={close}>
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

      <div class="lsl-body">
        {#if error}<p class="lsl-alert" role="alert">{error}</p>{/if}

        <section aria-labelledby="lsl-receiving">
          <h3 id="lsl-receiving">
            Bridge is receiving <span class="lsl-count">{receiving.length}</span>
          </h3>
          {#if receiving.length === 0}
            <p class="lsl-empty">
              Nothing yet. Connect a FRENZ band or Neon on the main screen and its streams appear
              here.
            </p>
          {:else}
            <ul class="lsl-list">
              {#each receiving as stream (stream.uid)}
                {@const state = health(stream)}
                <li class="lsl-row">
                  <span class="lsl-dot" data-tone={state.tone} aria-hidden="true"></span>
                  <span class="lsl-name">{stream.name}</span>
                  <span class="lsl-num">{count(stream.sample_count)} samples</span>
                  <span class="lsl-state" data-tone={state.tone}>{state.text}</span>
                </li>
              {/each}
            </ul>
          {/if}
        </section>

        <section aria-labelledby="lsl-publishing">
          <h3 id="lsl-publishing">
            Bridge is publishing <span class="lsl-count">{publishing.length}</span>
          </h3>
          {#if publishing.length === 0}
            <p class="lsl-empty">
              Nothing yet. Bridge publishes a marker stream for study events once a device connects.
            </p>
          {:else}
            <ul class="lsl-list">
              {#each publishing as stream (stream.name)}
                <li class="lsl-row">
                  <span class="lsl-dot" data-tone="live" aria-hidden="true"></span>
                  <span class="lsl-name">{stream.name}</span>
                  <span class="lsl-num">{count(stream.sample_count)} sent</span>
                  <span class="lsl-state" data-tone="quiet">{stream.type}</span>
                </li>
              {/each}
            </ul>
          {/if}
        </section>

        <section aria-labelledby="lsl-network">
          <div class="lsl-network-head">
            <h3 id="lsl-network">
              On this network <span class="lsl-count">{network.length}</span>
            </h3>
            {#if network.length > 8}
              <input
                class="lsl-filter"
                type="search"
                bind:value={filter}
                placeholder="Filter by name"
                aria-label="Filter streams by name"
              />
            {/if}
          </div>
          <p class="lsl-lede">
            Every LSL stream any app on this network is sending, including ones Bridge isn't using.
          </p>
          {#if !searched}
            <p class="lsl-empty">Looking for streams</p>
          {:else if network.length === 0}
            <p class="lsl-empty">
              No streams found. If a device should be streaming, check that it is on and on the same
              network.
            </p>
          {:else}
            <ul class="lsl-list">
              {#each visibleNetwork as stream (stream.uid)}
                <li class="lsl-row lsl-row-network">
                  <span class="lsl-name">{stream.name}</span>
                  <span class="lsl-num">{stream.channels} ch</span>
                  <span class="lsl-num">{rate(stream)}</span>
                  <span class="lsl-state" data-tone="quiet">
                    {receivedUids.has(stream.uid)
                      ? 'Bridge receives'
                      : publishedNames.has(stream.name)
                        ? 'From Bridge'
                        : ''}
                  </span>
                </li>
              {/each}
            </ul>
          {/if}
        </section>
      </div>
    </div>
  </div>
{/if}

<style>
  .lsl-overlay {
    position: fixed;
    inset: 0;
    z-index: 1000;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(0, 0, 0, 0.8);
    backdrop-filter: blur(4px);
  }
  .lsl-dialog {
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
  .lsl-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 1.25rem 1.5rem;
    border-bottom: 1px solid var(--color-border);
  }
  .lsl-header h2 {
    font-size: 1.25rem;
    font-weight: 600;
  }
  .lsl-sub {
    font-size: 0.8125rem;
    color: var(--color-text-secondary);
  }
  .lsl-close {
    display: grid;
    place-items: center;
    width: 2rem;
    height: 2rem;
    border-radius: 6px;
    background: transparent;
    color: var(--color-text-secondary);
  }
  .lsl-close:hover {
    color: var(--color-text-primary);
    background: var(--color-surface-elevated);
  }

  .lsl-body {
    overflow-y: auto;
    padding: 1.5rem;
    display: flex;
    flex-direction: column;
    gap: 2rem;
  }
  .lsl-body h3 {
    font-size: 0.9375rem;
    font-weight: 600;
  }
  .lsl-count {
    margin-left: 0.375rem;
    font-weight: 400;
    color: var(--color-text-secondary);
  }
  .lsl-lede,
  .lsl-empty {
    font-size: 0.8125rem;
    color: var(--color-text-secondary);
  }
  .lsl-lede {
    margin-top: 0.25rem;
  }
  .lsl-empty {
    padding: 0.75rem 0;
  }
  .lsl-alert {
    padding: 0.625rem 0.875rem;
    border-radius: 6px;
    font-size: 0.875rem;
    background: rgba(239, 68, 68, 0.12);
    border: 1px solid rgba(239, 68, 68, 0.4);
    color: #fca5a5;
  }

  .lsl-list {
    list-style: none;
    margin-top: 0.625rem;
    border-top: 1px solid var(--color-border);
  }
  .lsl-row {
    display: grid;
    grid-template-columns: 0.5rem minmax(0, 1fr) 8.5rem 8rem;
    gap: 0.875rem;
    align-items: center;
    padding: 0.5rem 0;
    border-bottom: 1px solid var(--color-border);
    font-size: 0.875rem;
  }
  .lsl-row-network {
    grid-template-columns: minmax(0, 1fr) 3.5rem 5.5rem 8rem;
  }
  .lsl-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .lsl-num {
    text-align: right;
    font-size: 0.8125rem;
    color: var(--color-text-secondary);
    white-space: nowrap;
  }
  .lsl-state {
    text-align: right;
    font-size: 0.8125rem;
    white-space: nowrap;
  }
  .lsl-state[data-tone='live'] {
    color: var(--color-success);
  }
  .lsl-state[data-tone='wait'] {
    color: var(--color-warning);
  }
  .lsl-state[data-tone='fail'] {
    color: var(--color-error);
  }
  .lsl-state[data-tone='quiet'] {
    color: var(--color-text-secondary);
  }
  .lsl-dot {
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
    background: var(--color-text-disabled);
  }
  .lsl-dot[data-tone='live'] {
    background: var(--color-success);
  }
  .lsl-dot[data-tone='wait'] {
    background: var(--color-warning);
  }
  .lsl-dot[data-tone='fail'] {
    background: var(--color-error);
  }

  .lsl-network-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
  }
  .lsl-filter {
    width: 14rem;
    padding: 0.375rem 0.625rem;
    font: inherit;
    font-size: 0.8125rem;
    color: inherit;
    background: var(--color-background);
    border: 1px solid var(--color-border);
    border-radius: 6px;
  }
  .lsl-filter:focus {
    outline: 2px solid var(--color-primary);
    outline-offset: -1px;
  }

  @media (max-width: 640px) {
    .lsl-row {
      grid-template-columns: 0.5rem minmax(0, 1fr) auto;
    }
    .lsl-row .lsl-num {
      display: none;
    }
    .lsl-row-network {
      grid-template-columns: minmax(0, 1fr) auto;
    }
    .lsl-row-network .lsl-num {
      display: none;
    }
  }
</style>
