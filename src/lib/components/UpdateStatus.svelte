<script>
  // Bridge checks for an update on every launch (in Rust, see src-tauri/src/updates.rs). This shows the result in
  // the footer for as long as Bridge runs, offers each new version once in a toast, and installs on request.
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import * as toastStore from '../stores/toast.svelte.js';

  let { appVersion = '' } = $props();

  let status = $state({ state: 'not_checked' });
  let percent = $state(null);
  let installing = $state(false);

  let heardEvent = false;
  let manualCheck = false;
  let offeredVersion = null;
  let offerToastId = null;

  function apply(next) {
    status = next;
    if (next.state === 'available' && offeredVersion !== next.version) {
      offeredVersion = next.version;
      offerToastId = toastStore.addToast({
        type: 'update',
        title: `Update available: v${next.version}`,
        message: `You have v${next.current_version}. Installing restarts Bridge.`,
        persistent: true,
        actions: [
          { label: 'Install now', onClick: () => install() },
          { label: 'Later', onClick: id => toastStore.removeToast(id) },
        ],
      });
    }
    if (manualCheck && next.state !== 'checking') {
      manualCheck = false;
      if (next.state === 'up_to_date') {
        toastStore.addToast({
          type: 'success',
          title: "You're up to date",
          message: `v${next.version} is the latest version.`,
        });
      } else if (next.state === 'failed') {
        toastStore.addToast({
          type: 'error',
          title: "Couldn't check for updates",
          message: next.error,
        });
      }
    }
  }

  onMount(() => {
    const unlisteners = [
      listen('update-status', event => {
        heardEvent = true;
        apply(event.payload);
      }),
      listen('update-progress', event => {
        const { downloaded, total } = event.payload;
        percent = total ? Math.round((downloaded / total) * 100) : null;
      }),
      listen('menu-check-for-updates', () => {
        manualCheck = true;
      }),
    ];
    // Subscribed first, so a check that finishes now is never missed; an event that already arrived is newer
    // than this snapshot, so the snapshot never overwrites it.
    invoke('update_status').then(current => {
      if (!heardEvent) apply(current);
    });
    return () => unlisteners.forEach(p => p.then(unlisten => unlisten()));
  });

  async function checkNow() {
    manualCheck = true;
    apply(await invoke('check_for_updates'));
  }

  async function install() {
    if (installing) return;
    installing = true;
    if (offerToastId != null) toastStore.removeToast(offerToastId);
    try {
      await invoke('install_update'); // Bridge restarts when it succeeds.
    } catch (error) {
      toastStore.addToast({
        type: 'error',
        title: "Couldn't install the update",
        message: String(error),
      });
      installing = false;
    }
  }
</script>

<p class="update-status">
  <span>Version {appVersion}</span>
  {#if status.state === 'checking'}
    <span class="update-note">Checking for updates</span>
  {:else if status.state === 'up_to_date'}
    <span class="update-note">Up to date</span>
  {:else if status.state === 'available'}
    <span class="update-note update-available">v{status.version} available</span>
    <button class="update-action" onclick={install} disabled={installing}
      >Install and restart</button
    >
  {:else if status.state === 'installing'}
    <span class="update-note">Installing update{percent != null ? ` ${percent}%` : ''}</span>
  {:else if status.state === 'failed'}
    <span class="update-note update-failed" title={status.error}>Couldn't check for updates</span>
    <button class="update-action" onclick={checkNow}>Try again</button>
  {/if}
</p>

<style>
  .update-status {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }
  .update-note {
    color: var(--color-text-secondary);
  }
  .update-available {
    color: var(--color-primary);
  }
  .update-failed {
    color: var(--color-warning);
  }
  .update-action {
    padding: 0.2rem 0.6rem;
    border-radius: 4px;
    font-size: 0.8125rem;
    background: var(--color-surface-elevated);
    color: var(--color-text-primary);
    border: 1px solid var(--color-border);
  }
  .update-action:hover:not(:disabled) {
    border-color: var(--color-border-hover);
  }
</style>
