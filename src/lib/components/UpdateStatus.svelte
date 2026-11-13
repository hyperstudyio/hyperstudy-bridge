<script>
  // Bridge checks for an update on every launch (in Rust, see src-tauri/src/updates.rs, which also logs every
  // outcome). The page shows the version and announces updates in toasts: each new version is offered once,
  // install progress is shown in the toast, and a check asked for from the Help menu always reports its result.
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import * as toastStore from '../stores/toast.svelte.js';

  let { appVersion = '' } = $props();

  let heardEvent = false;
  let manualCheck = false;
  let installing = false;
  let offeredVersion = null;
  let offerToastId = null;
  let progressToastId = null;

  function apply(next) {
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
        if (progressToastId != null && total) {
          toastStore.updateToast(progressToastId, {
            message: `${Math.round((downloaded / total) * 100)}%`,
          });
        }
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

  async function install() {
    if (installing) return;
    installing = true;
    if (offerToastId != null) toastStore.removeToast(offerToastId);
    progressToastId = toastStore.addToast({
      type: 'update',
      title: 'Installing update',
      message: 'Bridge restarts when it finishes.',
      persistent: true,
    });
    try {
      await invoke('install_update'); // Bridge restarts when it succeeds.
    } catch (error) {
      toastStore.removeToast(progressToastId);
      progressToastId = null;
      installing = false;
      // Offer it again: a refusal (a recording is running) is usually temporary.
      offeredVersion = null;
      toastStore.addToast({
        type: 'error',
        title: "Couldn't install the update",
        message: String(error),
      });
      apply(await invoke('update_status'));
    }
  }
</script>

<p>Version: {appVersion}</p>
