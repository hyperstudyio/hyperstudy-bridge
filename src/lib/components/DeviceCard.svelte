<script>
  import { onMount, onDestroy } from 'svelte';
  import * as bridgeStore from '../stores/websocket.svelte.js';
  import {
    sendTtlPulse,
    listTtlDevices,
    startFrenzBridge,
    getFrenzBridgeStatus,
    checkFrenzBridgeAvailable,
  } from '../services/tauri.js';
  import { getSecret } from '../services/stronghold.js';
  import { listen } from '@tauri-apps/api/event';
  import DeviceConfigModal from './DeviceConfigModal.svelte';

  let { device, onConfigUpdate = () => {}, onRemove = () => {} } = $props();

  // Modal state
  let showConfigModal = $state(false);

  // TTL device list for dropdown
  let detectedTtlDevices = $state([]);
  let isLoadingDevices = $state(false);

  // FRENZ credential status
  let frenzDeviceId = $state(null);
  let frenzKeyConfigured = $state(false);

  // FRENZ bridge process status
  let frenzBridgeAvailable = $state(false);
  let frenzBridgeStatus = $state({ state: 'stopped', streams: [], sample_count: 0 });
  let frenzTimingAnomaly = $derived(
    Object.entries(frenzBridgeStatus.timing || {}).some(([suffix, timing]) =>
      timing.backward_count > 0 || timing.nonfinite_count > 0 ||
      timing.sequence_discontinuity_count > 0 ||
      (suffix.endsWith('_raw') && timing.cadence_deviation_count > 0)
    )
  );
  let unlistenFrenzStatus = null;

  // FRENZ unified connect progress
  let frenzConnecting = $state(false);
  let frenzDisconnecting = $state(false);
  let frenzPhaseText = $state('');

  // General device connect progress
  let isConnecting = $state(false);
  let connectPhaseText = $state('');

  // Load TTL devices on mount if this is a TTL device
  // Load FRENZ credential status if this is a FRENZ device
  onMount(() => {
    if (device.id === 'ttl') {
      refreshTtlDevices();
    }
    if (device.id === 'frenz') {
      loadFrensCredentialStatus();
      loadFrenzBridgeInfo();
    }
  });

  onDestroy(() => {
    if (unlistenFrenzStatus) {
      unlistenFrenzStatus();
      unlistenFrenzStatus = null;
    }
  });

  async function loadFrenzBridgeInfo() {
    // Check if PyApp binary is available
    frenzBridgeAvailable = await checkFrenzBridgeAvailable();

    // Get current status
    frenzBridgeStatus = await getFrenzBridgeStatus();

    // Listen for real-time status updates
    unlistenFrenzStatus = await listen('frenz_bridge_status', event => {
      frenzBridgeStatus = event.payload;
    });
  }

  function getFrenzBridgeStateLabel(state) {
    switch (state) {
      case 'not_available':
        return 'Not Available';
      case 'stopped':
        return 'Stopped';
      case 'bootstrapping':
        return 'Starting...';
      case 'connecting':
        return 'Connecting...';
      case 'streaming':
        return 'Streaming';
      case 'error':
        return 'Error';
      default:
        return state;
    }
  }

  function getFrenzBridgeStateColor(state) {
    switch (state) {
      case 'streaming':
        return 'var(--color-success)';
      case 'bootstrapping':
      case 'connecting':
        return 'var(--color-warning)';
      case 'error':
        return 'var(--color-error)';
      default:
        return 'var(--color-text-disabled)';
    }
  }

  async function loadFrensCredentialStatus() {
    try {
      const deviceId = await getSecret('frenz_device_id');
      const productKey = await getSecret('frenz_product_key');
      frenzDeviceId = deviceId || null;
      frenzKeyConfigured = !!productKey;
    } catch {
      // Stronghold not initialized yet — that's fine
    }
  }

  async function refreshTtlDevices() {
    if (device.id !== 'ttl') return;

    isLoadingDevices = true;
    try {
      const result = await listTtlDevices();
      if (result.success && result.data) {
        detectedTtlDevices = result.data.devices || [];
        // Auto-select if only one device and no port configured
        if (result.data.autoSelected && !device.config.port) {
          device.config = { ...device.config, port: result.data.autoSelected };
        }
      }
    } catch (error) {
      console.error('Failed to refresh TTL devices:', error);
    } finally {
      isLoadingDevices = false;
    }
  }

  function updateTtlPort(port) {
    device.config = { ...device.config, port };
  }

  function getStatusColor(status) {
    switch (status) {
      case 'connected':
        return 'var(--color-success)';
      case 'connecting':
        return 'var(--color-warning)';
      case 'disconnected':
        return 'var(--color-text-disabled)';
      case 'error':
        return 'var(--color-error)';
      default:
        return 'var(--color-text-disabled)';
    }
  }

  function getStatusLabel(status) {
    switch (status) {
      case 'connected':
        return 'Connected';
      case 'connecting':
        return 'Connecting...';
      case 'disconnected':
        return 'Disconnected';
      case 'error':
        return 'Error';
      default:
        return 'Unknown';
    }
  }

  async function toggleConnection() {
    // Capture values at start to prevent stale closure issues
    const deviceId = device.id;
    const deviceName = device.name;
    const deviceConfig = { ...device.config };
    const currentStatus = device.status;

    console.log(`toggleConnection called! Status: ${currentStatus}`);

    // Route FRENZ to dedicated orchestration handlers
    if (deviceId === 'frenz') {
      if (currentStatus !== 'connected' && currentStatus !== 'connecting') {
        return handleFrenzConnect();
      } else if (currentStatus === 'connected') {
        return handleFrenzDisconnect();
      }
      return;
    }

    // Route EyeLink to dedicated handlers (uses custom WebSocket actions)
    if (deviceId === 'eyelink') {
      if (currentStatus !== 'connected' && currentStatus !== 'connecting') {
        return handleEyeLinkConnect();
      } else if (currentStatus === 'connected') {
        return handleEyeLinkDisconnect();
      }
      return;
    }

    // Allow connection from disconnected, error, or unknown status
    if (currentStatus !== 'connected' && currentStatus !== 'connecting') {
      console.log(`Connecting ${deviceName}...`);
      console.log(`Device config:`, deviceConfig);
      console.log(`Device status:`, currentStatus);

      isConnecting = true;
      let phaseTimeout = null;

      // Show contextual progress phases based on device type
      if (deviceId === 'pupil' && !deviceConfig.url && deviceConfig.device_id) {
        connectPhaseText = 'Finding pinned phone on the network...';
        phaseTimeout = setTimeout(() => {
          connectPhaseText = 'Connecting to Neon Companion...';
        }, 5000);
      } else if (deviceId === 'pupil' && deviceConfig.url?.includes('.local')) {
        connectPhaseText = 'Resolving hostname...';
        phaseTimeout = setTimeout(() => {
          connectPhaseText = 'Connecting to Neon Companion...';
        }, 4000);
      } else if (deviceId === 'pupil') {
        connectPhaseText = 'Connecting to Neon Companion...';
      } else if (deviceId === 'kernel') {
        connectPhaseText = 'Connecting to Kernel Flow2...';
      } else {
        connectPhaseText = `Connecting to ${deviceName}...`;
      }

      try {
        const result = await bridgeStore.connectDevice(deviceId, deviceConfig);
        console.log(`Successfully connected ${deviceName}`, result);
      } catch (error) {
        console.error(`Failed to connect ${deviceName}:`, error);
        alert(`Failed to connect: ${error.message || error}`);
      } finally {
        if (phaseTimeout) clearTimeout(phaseTimeout);
        isConnecting = false;
        connectPhaseText = '';
      }
    } else if (currentStatus === 'connected') {
      console.log(`Disconnecting ${deviceName}...`);
      try {
        await bridgeStore.disconnectDevice(deviceId);
        console.log(`Successfully disconnected ${deviceName}`);
      } catch (error) {
        console.error(`Failed to disconnect ${deviceName}:`, error);
        alert(`Failed to disconnect: ${error.message || error}`);
      }
    }
  }

  // EyeLink connect: uses custom WebSocket action
  async function handleEyeLinkConnect() {
    isConnecting = true;
    connectPhaseText = 'Connecting to EyeLink tracker...';

    try {
      const result = await bridgeStore.connectEyeLink(device.config);
      console.log('Successfully connected EyeLink', result);
    } catch (error) {
      console.error('Failed to connect EyeLink:', error);
      alert(`Failed to connect: ${error.message || error}`);
    } finally {
      isConnecting = false;
      connectPhaseText = '';
    }
  }

  // EyeLink disconnect: uses custom WebSocket action
  async function handleEyeLinkDisconnect() {
    try {
      await bridgeStore.disconnectEyeLink();
      console.log('Successfully disconnected EyeLink');
    } catch (error) {
      console.error('Failed to disconnect EyeLink:', error);
      alert(`Failed to disconnect: ${error.message || error}`);
    }
  }

  // FRENZ orchestrated connect: start bridge → wait for streaming → connect LSL
  async function handleFrenzConnect() {
    if (!frenzDeviceId || !frenzKeyConfigured) {
      alert('Please configure FRENZ credentials first (use Configure button)');
      return;
    }

    frenzConnecting = true;
    frenzPhaseText = 'Starting bridge...';

    try {
      // Start bridge if not already streaming
      const status = await getFrenzBridgeStatus();
      if (status.state !== 'streaming') {
        const productKey = await getSecret('frenz_product_key');
        if (!productKey) {
          throw new Error('Product key not found. Please reconfigure FRENZ credentials.');
        }

        const result = await startFrenzBridge(frenzDeviceId, productKey);
        if (!result.success) {
          throw new Error(result.error || 'Failed to start bridge');
        }

        // Wait for streaming state (timeout 3 min)
        await waitForFrenzStreaming(180000);
      }

      // Connect LSL streams via WebSocket
      frenzPhaseText = 'Connecting to data streams...';
      await bridgeStore.connectDevice('frenz', { ...device.config, device_name: frenzDeviceId });

    } catch (error) {
      alert(`Failed to connect: ${error.message || error}`);
    } finally {
      frenzConnecting = false;
      frenzPhaseText = '';
    }
  }

  // Wait for the FRENZ bridge to reach "streaming" state
  function waitForFrenzStreaming(timeoutMs = 180000) {
    return new Promise((resolve, reject) => {
      if (frenzBridgeStatus.state === 'streaming') {
        resolve();
        return;
      }

      const timeoutId = setTimeout(() => {
        clearInterval(interval);
        reject(new Error('FRENZ bridge startup timed out'));
      }, timeoutMs);

      const interval = setInterval(() => {
        // Update progress text from bridge status
        if (frenzBridgeStatus.state === 'bootstrapping') {
          frenzPhaseText = frenzBridgeStatus.message || 'Installing packages...';
        } else if (frenzBridgeStatus.state === 'connecting') {
          frenzPhaseText = 'Connecting to headband...';
        }

        if (frenzBridgeStatus.state === 'streaming') {
          clearTimeout(timeoutId);
          clearInterval(interval);
          resolve();
        } else if (frenzBridgeStatus.state === 'error') {
          clearTimeout(timeoutId);
          clearInterval(interval);
          reject(new Error(frenzBridgeStatus.message || 'Bridge error'));
        }
      }, 200);
    });
  }

  // FRENZ disconnect: LSL streams + bridge process
  async function handleFrenzDisconnect() {
    if (frenzDisconnecting) return;
    frenzDisconnecting = true;
    try {
      await bridgeStore.disconnectDevice('frenz');
      frenzBridgeStatus = await getFrenzBridgeStatus();
    } catch (error) {
      alert(`Failed to disconnect: ${error.message || error}`);
    } finally {
      frenzDisconnecting = false;
    }
  }

  async function configureDevice() {
    console.log(`Configuring ${device.name}...`);
    showConfigModal = true;
  }

  async function removeDevice() {
    // Disconnect first if connected
    if (device.status === 'connected') {
      try {
        await bridgeStore.disconnectDevice(device.id);
      } catch (error) {
        console.error(`Failed to disconnect before removal:`, error);
      }
    }
    onRemove(device.id);
  }

  async function sendTestPulse() {
    console.log('sendTestPulse clicked!');
    if (device.id !== 'ttl') {
      console.log('Not TTL device, returning');
      return;
    }

    try {
      console.log('Sending test pulse to:', device.config.port);
      const pulseResult = await sendTtlPulse(device.config.port, device.config.pulse_duration_ms);
      console.log('Pulse result:', pulseResult);
      const { result, latency } = pulseResult;
      console.log('Result:', result, 'Latency:', latency);
      if (result.success) {
        alert(`Test pulse sent successfully! (${latency.toFixed(1)}ms)`);
      } else {
        alert(`Failed to send pulse: ${result.error || 'Unknown error'}`);
      }
    } catch (error) {
      console.error('Error in sendTestPulse:', error);
      alert(`Error: ${error.message || String(error)}`);
    }
  }

  async function handleConfigSave(deviceId, newConfig, newLslConfig = null) {
    // Capture device name at start to prevent stale closure
    const deviceName = device.name;
    const wasConnected = device.status === 'connected';

    console.log(`Saving configuration for ${deviceId}:`, newConfig);
    if (newLslConfig) {
      console.log(`Saving LSL configuration:`, newLslConfig);
    }

    try {
      // Update the device config via parent callback (persists to selectedDevices)
      onConfigUpdate(deviceId, newConfig, newLslConfig);

      // If the device was connected, reconnect with new config
      // disconnectDevice properly awaits completion, so no arbitrary delay needed
      if (wasConnected && deviceId !== 'frenz') {
        if (deviceId === 'eyelink') {
          await bridgeStore.disconnectEyeLink();
          await bridgeStore.connectEyeLink(newConfig);
        } else {
          await bridgeStore.disconnectDevice(deviceId);
          await bridgeStore.connectDevice(deviceId, newConfig);
        }
      }

      console.log(`Configuration saved successfully for ${deviceName}`);

      // Refresh FRENZ credential status after save
      if (deviceId === 'frenz') {
        await loadFrensCredentialStatus();
      }
    } catch (error) {
      console.error(`Failed to save configuration for ${deviceName}:`, error);
      throw error; // Re-throw to let the modal handle the error display
    }
  }
</script>

<div class="device-card">
  <div class="device-header">
    <h3>{device.name}</h3>
    <div class="header-actions">
      <div
        class="status-dot"
        style="background-color: {getStatusColor(device.status)}"
        title={getStatusLabel(device.status)}
      ></div>
      <button
        class="remove-btn"
        onclick={removeDevice}
        title="Remove device"
        aria-label="Remove device"
      >
        <svg
          width="14"
          height="14"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
        >
          <line x1="18" y1="6" x2="6" y2="18"></line>
          <line x1="6" y1="6" x2="18" y2="18"></line>
        </svg>
      </button>
    </div>
  </div>

  <div class="device-info">
    <div class="info-row">
      <span class="label">Type:</span>
      <span class="value">{device.type}</span>
    </div>
    <div class="info-row">
      <span class="label">Connection:</span>
      <span class="value">{device.connection}</span>
    </div>
    <div class="info-row">
      <span class="label">Status:</span>
      <span class="value status-text">{getStatusLabel(device.status)}</span>
    </div>
  </div>

  <div class="device-config">
    {#if device.id === 'ttl'}
      <div class="config-row port-selector">
        <span class="label">Port:</span>
        <div class="port-controls">
          <select
            class="port-select"
            value={device.config.port || ''}
            onchange={e => updateTtlPort(e.target.value)}
            disabled={device.status === 'connected' || device.status === 'connecting'}
          >
            <option value="">Select device...</option>
            {#each detectedTtlDevices as ttlDevice}
              <option value={ttlDevice.port}>
                {ttlDevice.port}
                {ttlDevice.serial_number !== 'Unknown' ? `(S/N: ${ttlDevice.serial_number})` : ''}
              </option>
            {/each}
          </select>
          <button
            class="refresh-btn"
            onclick={refreshTtlDevices}
            title="Refresh devices"
            disabled={isLoadingDevices || device.status === 'connected'}
          >
            {isLoadingDevices ? '...' : '↻'}
          </button>
        </div>
      </div>
    {:else if device.id === 'kernel'}
      <div class="config-row">
        <span class="label">IP:</span>
        <span class="value">{device.config.ip || 'Not configured'}</span>
      </div>
      <div class="config-row">
        <span class="label">Port:</span>
        <span class="value">{device.config.port}</span>
      </div>
    {:else if device.id === 'pupil'}
      <div class="config-row">
        <span class="label">URL:</span>
        <span class="value">
          {device.config.url || (device.config.device_id ? 'Resolved from pinned phone' : 'Not configured')}
        </span>
      </div>
      <div class="config-row">
        <span class="label">Pinned phone:</span>
        <span class="value">{device.config.device_id || 'Any (not pinned)'}</span>
      </div>
      {#if device.status === 'connected' && device.info?.device_id}
        <div class="config-row">
          <span class="label">Connected:</span>
          <span class="value">
            {device.info.device_name} · {device.info.device_id} · {device.info.device_ip}{device.info
              .battery_level != null
              ? ` · ${Math.round(device.info.battery_level)}%`
              : ''}
          </span>
        </div>
        {#if device.info.recording_id}
          <div class="config-row">
            <span class="label">Recording:</span>
            <span class="value">
              {device.info.recording_owned ? 'started by this station' : 'started elsewhere'}
              ({String(device.info.recording_id).slice(0, 8)})
            </span>
          </div>
        {/if}
      {/if}
    {:else if device.id === 'eyelink'}
      <div class="config-row">
        <span class="label">IP:</span>
        <span class="value">{device.config.ip || 'Not configured'}</span>
      </div>
      <div class="config-row">
        <span class="label">Sample Rate:</span>
        <span class="value">{device.config.sample_rate || 1000} Hz</span>
      </div>
    {:else if device.id === 'frenz'}
      <div class="config-row">
        <span class="label">Device ID:</span>
        <span class="value">{frenzDeviceId || 'Not configured'}</span>
      </div>
      <div class="config-row">
        <span class="label">Product Key:</span>
        <span class="value credential-status" class:configured={frenzKeyConfigured}>
          {frenzKeyConfigured ? 'Configured' : 'Not configured'}
        </span>
      </div>
      {#if frenzBridgeAvailable}
        <div class="config-row">
          <span class="label">Bridge:</span>
          <span
            class="value bridge-status"
            style="color: {getFrenzBridgeStateColor(frenzBridgeStatus.state)}"
          >
            {getFrenzBridgeStateLabel(frenzBridgeStatus.state)}
          </span>
        </div>
        {#if frenzBridgeStatus.message}
          <div class="config-row">
            <span class="label"></span>
            <span class="value bridge-message">{frenzBridgeStatus.message}</span>
          </div>
        {/if}
        {#if frenzBridgeStatus.state === 'streaming'}
          <div class="config-row">
            <span class="label">Streams:</span>
            <span class="value">{frenzBridgeStatus.streams.length} active</span>
          </div>
          <div class="config-row">
            <span class="label">Samples:</span>
            <span class="value">{frenzBridgeStatus.sample_count.toLocaleString()}</span>
          </div>
          <div class="config-row">
            <span class="label">Timestamp quality:</span>
            <span class="value" style:color={frenzTimingAnomaly ? 'var(--color-warning)' : undefined}>
              {frenzTimingAnomaly ? 'Source timestamp discontinuities detected' : 'Acquisition alignment unverified'}
            </span>
          </div>
        {/if}
      {:else if !frenzBridgeAvailable && frenzBridgeStatus.state !== 'stopped'}
        <div class="config-row">
          <span class="label">Bridge:</span>
          <span class="value" style="color: var(--color-text-disabled)"
            >Not available on this platform</span
          >
        </div>
      {/if}
      <div class="config-row">
        <span class="label">LSL Streams:</span>
        <span class="value">
          {Object.entries(device.config || {}).filter(([k, v]) => k.startsWith('stream') && v)
            .length} selected
        </span>
      </div>
    {/if}
  </div>

  {#if isConnecting && device.id !== 'frenz'}
    <div class="connect-progress">
      <div class="progress-bar">
        <div class="progress-fill indeterminate"></div>
      </div>
      <span class="progress-text">{connectPhaseText}</span>
    </div>
  {/if}

  {#if device.id === 'frenz' && frenzConnecting}
    <div class="frenz-progress">
      <div class="progress-bar">
        <div class="progress-fill indeterminate"></div>
      </div>
      <span class="progress-text">{frenzPhaseText}</span>
    </div>
  {/if}

  <div class="device-actions">
    <button
      class="action-btn connect-btn"
      onclick={toggleConnection}
      disabled={device.status === 'connecting' ||
        isConnecting ||
        frenzConnecting ||
        frenzDisconnecting ||
        (device.id === 'ttl' && !device.config.port)}
    >
      {frenzDisconnecting ? 'Disconnecting...' : device.status === 'connected'
        ? 'Disconnect'
        : isConnecting || frenzConnecting
          ? 'Connecting...'
          : 'Connect'}
    </button>
    {#if device.id === 'ttl' && device.status === 'connected'}
      <button class="action-btn pulse-btn" onclick={sendTestPulse}> Send Pulse </button>
    {/if}
    <button class="action-btn config-btn" onclick={configureDevice}> Configure </button>
  </div>
</div>

<DeviceConfigModal
  bind:isOpen={showConfigModal}
  {device}
  onSave={handleConfigSave}
  onClose={() => (showConfigModal = false)}
/>

<style>
  .device-card {
    background: var(--color-surface);
    border-radius: 12px;
    padding: 1.5rem;
    border: 1px solid var(--color-border);
    transition: all 0.2s;
  }

  .device-card:hover {
    background: var(--color-surface-elevated);
    border-color: var(--color-border-hover);
    transform: translateY(-2px);
  }

  .device-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 1rem;
    padding-bottom: 0.75rem;
    border-bottom: 1px solid var(--color-border);
  }

  h3 {
    margin: 0;
    font-size: 1.125rem;
    font-weight: 600;
    color: var(--color-text-primary);
  }

  .header-actions {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }

  .status-dot {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    animation: pulse 2s infinite;
  }

  .remove-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    padding: 0;
    border: none;
    border-radius: 4px;
    background: transparent;
    color: var(--color-text-secondary);
    cursor: pointer;
    opacity: 0;
    transition: all 0.2s;
  }

  .device-card:hover .remove-btn {
    opacity: 1;
  }

  .remove-btn:hover {
    background: var(--color-error);
    color: white;
  }

  @keyframes pulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.5;
    }
  }

  .device-info,
  .device-config {
    margin-bottom: 1rem;
  }

  .info-row,
  .config-row {
    display: flex;
    justify-content: space-between;
    padding: 0.25rem 0;
    font-size: 0.875rem;
  }

  .label {
    color: var(--color-text-secondary);
    font-weight: 500;
  }

  .value {
    color: var(--color-text-primary);
    font-family: 'SF Mono', Monaco, monospace;
    font-size: 0.813rem;
  }

  .status-text {
    font-weight: 500;
  }

  .device-config {
    padding: 0.75rem;
    background: var(--color-background);
    border-radius: 6px;
    margin: 1rem 0;
    border: 1px solid var(--color-border);
    overflow: hidden;
  }

  .device-actions {
    display: flex;
    gap: 0.75rem;
  }

  .action-btn {
    flex: 1;
    padding: 0.5rem;
    border: none;
    border-radius: 6px;
    font-size: 0.875rem;
    font-weight: 500;
    cursor: pointer;
    transition: all 0.2s;
  }

  .action-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .connect-btn {
    background: var(--color-primary);
    color: white;
  }

  .connect-btn:hover:not(:disabled) {
    background: var(--color-primary-hover);
  }

  .config-btn {
    background: var(--color-surface-elevated);
    color: var(--color-text-secondary);
    border: 1px solid var(--color-border);
  }

  .config-btn:hover {
    background: rgba(255, 255, 255, 0.1);
    color: var(--color-text-primary);
    border-color: var(--color-border-hover);
  }

  .pulse-btn {
    background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
    color: white;
    font-weight: 600;
  }

  .pulse-btn:hover {
    background: linear-gradient(135deg, #764ba2 0%, #667eea 100%);
    transform: translateY(-1px);
    box-shadow: 0 4px 12px rgba(102, 126, 234, 0.4);
  }

  .port-selector {
    flex-direction: column;
    align-items: flex-start;
    gap: 0.5rem;
    width: 100%;
    box-sizing: border-box;
  }

  .port-controls {
    display: flex;
    gap: 0.5rem;
    width: 100%;
    min-width: 0;
    overflow: hidden;
  }

  .port-select {
    flex: 1 1 0;
    min-width: 0;
    width: 0;
    padding: 0.375rem 0.5rem;
    border: 1px solid var(--color-border);
    border-radius: 4px;
    background: var(--color-background);
    color: var(--color-text-primary);
    font-family: 'SF Mono', Monaco, monospace;
    font-size: 0.75rem;
    cursor: pointer;
    box-sizing: border-box;
  }

  .port-select:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .port-select:focus {
    outline: none;
    border-color: var(--color-primary);
  }

  .refresh-btn {
    padding: 0.375rem 0.5rem;
    border: 1px solid var(--color-border);
    border-radius: 4px;
    background: var(--color-surface-elevated);
    color: var(--color-text-secondary);
    font-size: 0.875rem;
    cursor: pointer;
    transition: all 0.2s;
  }

  .refresh-btn:hover:not(:disabled) {
    background: var(--color-primary);
    color: white;
    border-color: var(--color-primary);
  }

  .refresh-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .credential-status {
    font-family: inherit;
    font-style: italic;
    color: var(--color-text-disabled);
  }

  .credential-status.configured {
    color: var(--color-success);
    font-style: normal;
  }

  .bridge-status {
    font-weight: 500;
  }

  .bridge-message {
    font-size: 0.75rem;
    color: var(--color-text-secondary);
    font-style: italic;
  }

  .connect-progress,
  .frenz-progress {
    margin-bottom: 1rem;
    padding: 0.5rem 0;
  }

  .progress-bar {
    height: 4px;
    background: var(--color-border);
    border-radius: 2px;
    overflow: hidden;
    margin-bottom: 0.5rem;
  }

  .progress-fill {
    height: 100%;
    background: var(--color-primary);
    border-radius: 2px;
    transition: width 0.3s ease;
  }

  .progress-fill.indeterminate {
    width: 30%;
    animation: indeterminate 1.5s infinite ease-in-out;
  }

  @keyframes indeterminate {
    0% {
      transform: translateX(-100%);
    }
    100% {
      transform: translateX(400%);
    }
  }

  .progress-text {
    font-size: 0.75rem;
    color: var(--color-text-secondary);
  }
</style>
