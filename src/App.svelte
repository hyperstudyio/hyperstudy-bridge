<script>
  import { onMount, onDestroy } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import { tauriService } from './lib/services/tauri.js';
  import { checkForUpdate, downloadAndInstallUpdate, relaunchApp } from './lib/services/updater.js';
  import RecordingPanel from './lib/components/RecordingPanel.svelte';
  import DeviceCard from './lib/components/DeviceCard.svelte';
  import StatusIndicator from './lib/components/StatusIndicator.svelte';
  import AddDeviceModal from './lib/components/AddDeviceModal.svelte';
  import LogViewer from './lib/components/LogViewer.svelte';
  import SettingsPanel from './lib/components/SettingsPanel.svelte';
  import LslConfigPanel from './lib/components/LslConfigPanel.svelte';
  import PerformancePanel from './lib/components/PerformancePanel.svelte';
  import Toast from './lib/components/Toast.svelte';
  import * as bridgeStore from './lib/stores/websocket.svelte.js';
  import * as toastStore from './lib/stores/toast.svelte.js';
  import { initialize } from './lib/stores/websocket.svelte.js';
  import logo from './assets/hyperstudy-logo.svg';

  // App version
  let appVersion = $state('...');

  // Modal state
  let showAddDeviceModal = $state(false);
  let showLogViewer = $state(false);
  let showSettingsPanel = $state(false);
  let showLslPanel = $state(false);
  let showRecordingPanel = $state(false);
  let showPerformancePanel = $state(false);

  // Selected devices - user has explicitly added these
  let selectedDevices = $state([]);

  // Import reactive state from store using getters
  const bridgeStatus = $derived(bridgeStore.getStatus());
  const wsDevices = $derived(bridgeStore.getDevices());

  // Derive device list with updated statuses from WebSocket - only show selected devices
  let devices = $derived(
    selectedDevices.map(device => {
      const wsDevice = wsDevices.get(device.id);
      if (wsDevice) {
        // `info` carries which hardware answered (Neon device_id/IP, recording state)
        return { ...device, status: wsDevice.status || 'disconnected', info: wsDevice.info ?? device.info };
      }
      return device;
    })
  );

  // Disconnect all selected devices
  async function disconnectAll() {
    console.log('Disconnect All button clicked');

    // Get current device statuses from the store
    const currentDevices = bridgeStore.getDevices();

    for (const device of selectedDevices) {
      const wsDevice = currentDevices.get(device.id);
      if (wsDevice && wsDevice.status === 'connected') {
        console.log(`Disconnecting device: ${device.id}`);
        try {
          await bridgeStore.disconnectDevice(device.id);
          console.log(`Successfully sent disconnect command for ${device.id}`);
        } catch (error) {
          console.error(`Failed to disconnect ${device.id}:`, error);
        }
      }
    }
  }

  // Handle adding devices from modal
  function handleAddDevices(devicesToAdd) {
    console.log('Adding devices:', devicesToAdd);
    selectedDevices = [
      ...selectedDevices,
      ...devicesToAdd.filter(d => !selectedDevices.some(existing => existing.id === d.id)),
    ];
    showAddDeviceModal = false;
  }

  // Update device configuration (called from DeviceCard)
  function updateDeviceConfig(deviceId, newConfig, newLslConfig = null) {
    selectedDevices = selectedDevices.map(device => {
      if (device.id === deviceId) {
        return {
          ...device,
          config: { ...device.config, ...newConfig },
          ...(newLslConfig && { lslConfig: { ...device.lslConfig, ...newLslConfig } }),
        };
      }
      return device;
    });
  }

  // Remove a device from the selected devices list
  function removeDevice(deviceId) {
    console.log('Removing device:', deviceId);
    selectedDevices = selectedDevices.filter(d => d.id !== deviceId);
  }

  // --- Update logic ---
  let menuUnlisten = null;
  let isCheckingForUpdate = false;
  let isUpdating = false;
  let updateAvailableToastId = null;

  async function performUpdateCheck(isManual) {
    if (isCheckingForUpdate) return;
    isCheckingForUpdate = true;

    try {
      const result = await checkForUpdate();

      if (result.available) {
        updateAvailableToastId = toastStore.addToast({
          type: 'update',
          title: `Update Available: v${result.version}`,
          message: 'A new version of HyperStudy Bridge is ready.',
          persistent: true,
          actions: [
            { label: 'Update Now', onClick: () => startUpdate() },
            { label: 'Later', onClick: toastId => toastStore.removeToast(toastId) },
          ],
        });
      } else if (result.error && isManual) {
        toastStore.addToast({
          type: 'error',
          title: 'Update check failed',
          message: result.error,
        });
      } else if (isManual) {
        toastStore.addToast({
          type: 'success',
          title: "You're up to date",
          message: `HyperStudy Bridge v${appVersion} is the latest version.`,
        });
      }
    } finally {
      isCheckingForUpdate = false;
    }
  }

  async function startUpdate() {
    if (isUpdating) return;
    isUpdating = true;

    // Dismiss the "Update Available" toast
    if (updateAvailableToastId != null) {
      toastStore.removeToast(updateAvailableToastId);
      updateAvailableToastId = null;
    }

    const downloadToastId = toastStore.addToast({
      type: 'info',
      title: 'Downloading Update...',
      message: '0%',
      persistent: true,
    });

    try {
      await downloadAndInstallUpdate(progress => {
        if (progress.phase === 'downloading') {
          toastStore.updateToast(downloadToastId, {
            message: `Downloading... ${progress.percent}%`,
          });
        } else if (progress.phase === 'finished') {
          toastStore.removeToast(downloadToastId);
        }
      });

      toastStore.addToast({
        type: 'success',
        title: 'Update Installed',
        message: 'Restarting HyperStudy Bridge...',
        persistent: true,
      });

      setTimeout(() => relaunchApp().catch(console.error), 2000);
    } catch (error) {
      toastStore.removeToast(downloadToastId);
      toastStore.addToast({
        type: 'error',
        title: 'Update Failed',
        message: error.message || 'An error occurred while updating.',
      });
      isUpdating = false;
    }
  }

  onMount(async () => {
    // Initialize the WebSocket connection
    initialize();
    console.log('HyperStudy Bridge initialized');

    // Fetch app version from Tauri (via get_app_info command for reliability)
    try {
      const info = await tauriService.getAppInfo();
      if (info && info.version) {
        appVersion = info.version;
      } else {
        appVersion = 'unknown';
      }
    } catch (error) {
      console.error('Failed to fetch app version:', error);
      appVersion = 'unknown';
    }

    // Listen for "Check for Updates" menu item
    menuUnlisten = await listen('menu-check-for-updates', () => {
      performUpdateCheck(true);
    });

    // Auto-check for updates on launch (after a brief delay)
    setTimeout(() => performUpdateCheck(false), 3000);
  });

  onDestroy(() => {
    bridgeStore.disconnect();
    if (menuUnlisten) menuUnlisten();
  });
</script>

<div class="app">
  <header>
    <div class="logo-container">
      <img src={logo} alt="HyperStudy" class="logo" />
      <h1>HyperStudy Device Bridge</h1>
    </div>

    <div class="header-actions">
      <button
        class="header-btn"
        onclick={e => {
          e.preventDefault();
          e.stopPropagation();
          showPerformancePanel = true;
        }}
        title="Performance Monitor"
        type="button"
      >
        <svg
          width="16"
          height="16"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
        >
          <path d="M22 12h-4l-3 9L9 3l-3 9H2"></path>
        </svg>
        Perf
      </button>

      <button
        class="header-btn"
        onclick={e => {
          e.preventDefault();
          e.stopPropagation();
          showLslPanel = true;
        }}
        title="LSL Streams"
        type="button"
      >
        <svg
          width="16"
          height="16"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
        >
          <path d="M3 12l2-2 4 4 8-8 2 2"></path>
          <circle cx="12" cy="12" r="10"></circle>
        </svg>
        LSL
      </button>

      <button class="header-btn" onclick={() => { showRecordingPanel = true; }} type="button">Recordings</button>

      <button
        class="header-btn"
        onclick={e => {
          e.preventDefault();
          e.stopPropagation();
          showLogViewer = true;
        }}
        title="View Logs"
        type="button"
      >
        <svg
          width="16"
          height="16"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
        >
          <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"></path>
          <polyline points="14,2 14,8 20,8"></polyline>
          <line x1="16" y1="13" x2="8" y2="13"></line>
          <line x1="16" y1="17" x2="8" y2="17"></line>
          <polyline points="10,9 9,9 8,9"></polyline>
        </svg>
        Logs
      </button>

      <button
        class="header-btn"
        onclick={e => {
          e.preventDefault();
          e.stopPropagation();
          showSettingsPanel = true;
        }}
        title="Settings"
        type="button"
      >
        <svg
          width="16"
          height="16"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
        >
          <circle cx="12" cy="12" r="3"></circle>
          <path
            d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z"
          ></path>
        </svg>
        Settings
      </button>

      <StatusIndicator status={bridgeStatus} />
    </div>
  </header>

  <main>
    <div class="controls">
      <button class="add-device-btn" onclick={() => (showAddDeviceModal = true)}>
        Add Device
      </button>
      <button
        class="disconnect-btn"
        onclick={disconnectAll}
        disabled={bridgeStatus !== 'ready' || selectedDevices.length === 0}
      >
        Disconnect All
      </button>
    </div>

    <div class="devices">
      {#if selectedDevices.length === 0}
        <div class="empty-state">
          <p>No devices added yet</p>
          <p class="hint">Click "Add Device" to get started</p>
        </div>
      {:else}
        {#each devices as device}
          <DeviceCard {device} onConfigUpdate={updateDeviceConfig} onRemove={removeDevice} />
        {/each}
      {/if}
    </div>
  </main>

  <AddDeviceModal
    bind:isOpen={showAddDeviceModal}
    onAdd={handleAddDevices}
    onClose={() => (showAddDeviceModal = false)}
  />

  <RecordingPanel bind:isOpen={showRecordingPanel} />
  <LslConfigPanel bind:isOpen={showLslPanel} />

  <LogViewer bind:isOpen={showLogViewer} />

  <SettingsPanel bind:isOpen={showSettingsPanel} />

  <PerformancePanel bind:isOpen={showPerformancePanel} />

  <Toast />

  <footer>
    <p>WebSocket: ws://localhost:9000</p>
    <p>Version: {appVersion}</p>
  </footer>
</div>

<style>
  .app {
    display: flex;
    flex-direction: column;
    height: 100vh;
    background: var(--color-background);
  }

  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 1rem 2rem;
    background: var(--color-surface);
    border-bottom: 1px solid var(--color-border);
    color: var(--color-text-primary);
  }

  .header-actions {
    display: flex;
    align-items: center;
    gap: 1rem;
  }

  .logo-container {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }

  .logo {
    height: 2rem;
    width: auto;
    object-fit: contain;
  }

  h1 {
    margin: 0;
    font-size: 1.5rem;
    font-weight: 600;
    color: var(--color-primary);
  }

  main {
    flex: 1;
    padding: 2rem;
    overflow-y: auto;
    background: var(--color-background);
  }

  .controls {
    display: flex;
    gap: 1rem;
    margin-bottom: 2rem;
  }

  button {
    padding: 0.75rem 1.5rem;
    border: none;
    border-radius: 8px;
    font-size: 1rem;
    font-weight: 500;
    cursor: pointer;
    transition: all 0.2s;
  }

  button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .add-device-btn {
    background: var(--color-surface-elevated);
    color: var(--color-text-primary);
    border: 1px solid var(--color-border);
  }

  .add-device-btn:hover:not(:disabled) {
    background: rgba(255, 255, 255, 0.1);
    transform: translateY(-1px);
    box-shadow: 0 4px 12px rgba(255, 255, 255, 0.1);
    border-color: var(--color-border-hover);
  }

  .disconnect-btn {
    background: var(--color-error);
    color: white;
  }

  .disconnect-btn:hover:not(:disabled) {
    background: #dc2626;
    transform: translateY(-1px);
    box-shadow: 0 4px 12px rgba(239, 68, 68, 0.3);
  }

  .header-btn {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    background: var(--color-surface-elevated);
    color: var(--color-text-secondary);
    font-size: 0.875rem;
    font-weight: 500;
    cursor: pointer;
    transition: all 0.2s;
  }

  .header-btn:hover {
    background: var(--color-primary);
    border-color: var(--color-primary);
    color: white;
    transform: translateY(-1px);
    box-shadow: 0 4px 12px rgba(76, 175, 80, 0.2);
  }

  .header-btn svg {
    flex-shrink: 0;
  }

  .devices {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
    gap: 1.5rem;
  }

  .empty-state {
    grid-column: 1 / -1;
    text-align: center;
    padding: 3rem;
    color: var(--color-text-secondary);
  }

  .empty-state p {
    margin: 0.5rem 0;
    font-size: 1.1rem;
  }

  .empty-state .hint {
    font-size: 0.9rem;
    opacity: 0.8;
  }

  footer {
    display: flex;
    justify-content: space-between;
    padding: 1rem 2rem;
    background: var(--color-surface);
    border-top: 1px solid var(--color-border);
    color: var(--color-text-secondary);
    font-size: 0.875rem;
  }
</style>
