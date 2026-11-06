<script>
  import { onMount } from 'svelte';
  import { listTtlDevices } from '../services/tauri.js';

  // Use Svelte 5 $props() rune
  let { isOpen = false, onAdd = () => {}, onClose = () => {} } = $props();

  // Available device types - TTL port will be auto-detected
  let availableDevices = $state([
    {
      id: 'ttl',
      name: 'TTL Pulse Generator',
      type: 'Adafruit RP2040',
      connection: 'USB Serial',
      config: { port: '/dev/cu.usbmodem101' }, // Fallback default
    },
    {
      id: 'kernel',
      name: 'Kernel Flow2',
      type: 'fNIRS',
      connection: 'TCP Socket',
      config: { ip: '127.0.0.1', port: 6767 },
    },
    {
      id: 'pupil',
      name: 'Pupil Labs Neon',
      type: 'Eye Tracker',
      connection: 'REST API + LSL',
      config: { url: 'neon.local:8080' },
    },
    {
      id: 'eyelink',
      name: 'EyeLink 1000 Plus',
      type: 'Eye Tracker',
      connection: 'Ethernet (TCP)',
      config: { ip: '100.1.1.1', sample_rate: 1000 },
    },
    {
      id: 'frenz',
      name: 'Earable FRENZ Brainband',
      type: 'EEG/Biosignals',
      connection: 'LSL (via Python)',
      config: {
        streamEegRaw: true,
        streamHrRaw: true,
        streamSpo2Raw: true,
        streamFirmware: true,
        streamEegFiltered: true,
        streamEogFiltered: true,
        streamEmgFiltered: true,
        streamPpgRaw: true,
        streamImuRaw: true,
        streamFocus: true,
        streamSleepStage: true,
        streamPoas: true,
        streamPosture: true,
        streamSignalQuality: true,
        streamAlpha: true,
        streamBeta: true,
        streamTheta: true,
        streamGamma: true,
        streamDelta: true,
        enableMarkerOutlet: true,
      },
    },
  ]);

  let selectedDevices = $state(new Set());

  // Auto-detect TTL device on mount
  onMount(async () => {
    try {
      const result = await listTtlDevices();
      if (result.success && result.data) {
        const { autoSelected, devices } = result.data;

        // If we have an auto-selected device, update the TTL config
        if (autoSelected) {
          console.log('Auto-detected TTL device:', autoSelected);
          const ttlDevice = availableDevices.find(d => d.id === 'ttl');
          if (ttlDevice) {
            ttlDevice.config.port = autoSelected;
            // Trigger reactivity
            availableDevices = [...availableDevices];
          }
        } else if (devices && devices.length > 0) {
          // Multiple devices found - use the first one as default
          console.log('Multiple TTL devices found, using first:', devices[0].port);
          const ttlDevice = availableDevices.find(d => d.id === 'ttl');
          if (ttlDevice) {
            ttlDevice.config.port = devices[0].port;
            availableDevices = [...availableDevices];
          }
        }
      }
    } catch (error) {
      console.error('Failed to auto-detect TTL device:', error);
      // Keep using the fallback default port
    }
  });

  function toggleDevice(deviceId, event) {
    // Cmd+Click or Ctrl+Click for multi-select
    if (event.metaKey || event.ctrlKey) {
      if (selectedDevices.has(deviceId)) {
        selectedDevices.delete(deviceId);
      } else {
        selectedDevices.add(deviceId);
      }
    } else {
      // Single click - replace selection
      selectedDevices.clear();
      selectedDevices.add(deviceId);
    }
    selectedDevices = new Set(selectedDevices); // Trigger reactivity
  }

  function handleAdd() {
    const devicesToAdd = availableDevices.filter(d => selectedDevices.has(d.id));
    onAdd(devicesToAdd);
    selectedDevices.clear();
    isOpen = false;
  }

  function handleClose() {
    selectedDevices.clear();
    onClose();
  }

  function handleKeydown(e) {
    if (e.key === 'Escape') {
      handleClose();
    }
  }
</script>

{#if isOpen}
  <div class="modal-overlay" role="presentation" onclick={handleClose} onkeydown={handleKeydown}>
    <div
      class="modal"
      role="dialog"
      aria-modal="true"
      tabindex="-1"
      onclick={e => e.stopPropagation()}
      onkeydown={e => e.key === 'Escape' && handleClose()}
    >
      <div class="modal-header">
        <h2>Add Devices</h2>
        <button class="close-btn" onclick={handleClose}>×</button>
      </div>

      <div class="modal-body">
        <p class="instructions">
          Click to select a device. Use <kbd>Cmd+Click</kbd> (Mac) or <kbd>Ctrl+Click</kbd> (Windows/Linux)
          to select multiple devices.
        </p>

        <div class="device-list">
          {#each availableDevices as device}
            <div
              class="device-item"
              class:selected={selectedDevices.has(device.id)}
              role="button"
              tabindex="0"
              aria-pressed={selectedDevices.has(device.id)}
              onclick={e => toggleDevice(device.id, e)}
              onkeydown={e => {
                if (e.key === 'Enter' || e.key === ' ') {
                  e.preventDefault();
                  toggleDevice(device.id, e);
                }
              }}
            >
              <div class="device-info">
                <h3>{device.name}</h3>
                <div class="device-meta">
                  <span class="device-type">{device.type}</span>
                  <span class="device-connection">{device.connection}</span>
                </div>
              </div>
              <div class="device-check">
                {#if selectedDevices.has(device.id)}
                  <svg width="20" height="20" viewBox="0 0 20 20" fill="none">
                    <path
                      d="M7 10L9 12L13 8"
                      stroke="currentColor"
                      stroke-width="2"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                    />
                  </svg>
                {/if}
              </div>
            </div>
          {/each}
        </div>
      </div>

      <div class="modal-footer">
        <button class="cancel-btn" onclick={handleClose}>Cancel</button>
        <button class="add-btn" onclick={handleAdd} disabled={selectedDevices.size === 0}>
          Add {selectedDevices.size > 0 ? `(${selectedDevices.size})` : ''}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .modal-overlay {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: rgba(0, 0, 0, 0.7);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
    animation: fadeIn 0.2s ease;
  }

  @keyframes fadeIn {
    from {
      opacity: 0;
    }
    to {
      opacity: 1;
    }
  }

  .modal {
    background: var(--color-surface);
    border-radius: 12px;
    width: 90%;
    max-width: 600px;
    max-height: 80vh;
    display: flex;
    flex-direction: column;
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.5);
    animation: slideUp 0.3s ease;
  }

  @keyframes slideUp {
    from {
      opacity: 0;
      transform: translateY(20px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }

  .modal-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 1.5rem;
    border-bottom: 1px solid var(--color-border);
  }

  .modal-header h2 {
    margin: 0;
    color: var(--color-primary);
    font-size: 1.5rem;
  }

  .close-btn {
    background: none;
    border: none;
    font-size: 2rem;
    color: var(--color-text-secondary);
    cursor: pointer;
    padding: 0;
    width: 32px;
    height: 32px;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: 4px;
    transition: all 0.2s;
  }

  .close-btn:hover {
    background: rgba(255, 255, 255, 0.1);
    color: var(--color-text-primary);
  }

  .modal-body {
    flex: 1;
    overflow-y: auto;
    padding: 1.5rem;
  }

  .instructions {
    color: var(--color-text-secondary);
    margin-bottom: 1.5rem;
    font-size: 0.9rem;
  }

  kbd {
    background: rgba(255, 255, 255, 0.1);
    padding: 2px 6px;
    border-radius: 4px;
    font-family: monospace;
    font-size: 0.85em;
  }

  .device-list {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .device-item {
    background: var(--color-background);
    border: 2px solid transparent;
    border-radius: 8px;
    padding: 1rem;
    cursor: pointer;
    transition: all 0.2s;
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  .device-item:hover {
    background: rgba(255, 255, 255, 0.02);
    border-color: rgba(76, 175, 80, 0.3);
  }

  .device-item.selected {
    background: rgba(76, 175, 80, 0.1);
    border-color: var(--color-primary);
  }

  .device-info h3 {
    margin: 0 0 0.5rem 0;
    color: var(--color-text-primary);
    font-size: 1.1rem;
  }

  .device-meta {
    display: flex;
    gap: 1rem;
    font-size: 0.9rem;
  }

  .device-type,
  .device-connection {
    color: var(--color-text-secondary);
  }

  .device-check {
    width: 24px;
    height: 24px;
    color: var(--color-primary);
  }

  .modal-footer {
    display: flex;
    justify-content: flex-end;
    gap: 1rem;
    padding: 1.5rem;
    border-top: 1px solid var(--color-border);
  }

  .cancel-btn,
  .add-btn {
    padding: 0.75rem 1.5rem;
    border: none;
    border-radius: 8px;
    font-size: 1rem;
    font-weight: 500;
    cursor: pointer;
    transition: all 0.2s;
  }

  .cancel-btn {
    background: transparent;
    color: var(--color-text-secondary);
    border: 1px solid var(--color-border);
  }

  .cancel-btn:hover {
    background: rgba(255, 255, 255, 0.05);
    color: var(--color-text-primary);
  }

  .add-btn {
    background: var(--color-primary);
    color: white;
  }

  .add-btn:hover:not(:disabled) {
    background: var(--color-primary-hover);
    transform: translateY(-1px);
    box-shadow: 0 4px 12px rgba(76, 175, 80, 0.3);
  }

  .add-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
</style>
