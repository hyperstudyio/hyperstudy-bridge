<script>
  import { listTtlDevices } from '../services/tauri.js';
  import { getSecret, setSecrets } from '../services/secrets.js';
  import * as bridgeStore from '../stores/websocket.svelte.js';

  import FrenzDevicePicker from './FrenzDevicePicker.svelte';
  import { FRENZ_OPENFRENZ } from '../services/tauri.js';
  import { untrack } from 'svelte';

  // Use Svelte 5 $props() rune
  let { isOpen = false, device = null, onSave = () => {}, onClose = () => {} } = $props();

  // Form state using $state rune
  let formData = $state({});
  let secureFields = $state({});
  let visibleSecrets = $state({});
  let errors = $state({});
  let isSubmitting = $state(false);
  let isLoading = $state(false);
  let saveError = $state('');
  let loadGeneration = 0;

  // TTL device detection state
  let detectedTtlDevices = $state([]);
  let isDetecting = $state(false);
  let showDeviceSelector = $state(false);

  // Neon phone discovery (mDNS) state
  let discoveredPhones = $state([]);
  let isDiscoveringPhones = $state(false);
  let phoneSearched = $state(false);
  let phoneDiscoveryError = $state('');

  // Device configuration templates with validation rules
  // frenztoolkit 0.3.2 sometimes repeats a one-second block in its raw data and skips the next.
  const SDK_RAW_WARNING =
    'comes from the FRENZ SDK, which can repeat one second of data and skip the next. Bridge records it unchanged.';

  const deviceConfigs = {
    ttl: {
      port: {
        label: 'Serial Port',
        type: 'text',
        placeholder: '/dev/cu.usbmodem or /dev/tty.usbmodem (macOS), /dev/ttyUSB0 (Linux)',
        required: true,
        pattern: '^(/dev/(tty\\.|cu\\.|ttyUSB)|COM\\d+)',
        errorMessage: 'Invalid port format. Expected /dev/cu.*, /dev/tty.*, /dev/ttyUSB*, or COM*',
      },
      pulse_duration_ms: {
        label: 'Pulse Duration (ms)',
        type: 'number',
        min: 1,
        max: 1000,
        default: 10,
        required: false,
        errorMessage: 'Pulse duration must be between 1-1000 ms',
      },
    },
    kernel: {
      ip: {
        label: 'IP Address',
        type: 'text',
        placeholder: '127.0.0.1 or 192.168.1.100',
        required: true,
        pattern: '^((25[0-5]|(2[0-4]|1\\d|[1-9]|)\\d)\\.?\\b){4}$',
        errorMessage: 'Invalid IP address format',
      },
      port: {
        label: 'Port',
        type: 'number',
        min: 1,
        max: 65535,
        default: 6767,
        required: true,
        errorMessage: 'Port must be between 1-65535',
      },
      samplingRate: {
        label: 'Sampling Rate (Hz)',
        type: 'select',
        options: [1, 5, 10, 25, 50, 100],
        default: 10,
        required: true,
      },
    },
    pupil: {
      url: {
        label: 'Neon Companion URL',
        type: 'text',
        placeholder: '192.168.1.101:8080 (leave empty to connect by pinned phone)',
        required: false,
        pattern: '^[\\w.-]+(:\\d+)?$',
        errorMessage: 'Invalid URL format. Expected hostname:port',
      },
      device_id: {
        label: 'Pinned phone (hardware ID)',
        type: 'text',
        placeholder: 'e.g. a41fe4fe2bccf6c3 — use "Find phones" below',
        required: false,
        pattern: '^[0-9a-fA-F]*$',
        errorMessage: 'Hardware ID is hexadecimal',
      },
    },
    frenz: {
      source: {
        label: 'Connection',
        type: 'select',
        options: ['SDK helper', FRENZ_OPENFRENZ],
        default: 'SDK helper',
        help: 'SDK helper: the Earable SDK, with its focus, sleep and posture streams (needs a product key). openfrenz: Bridge connects to the band itself and adds eye, brow, heart and head signals (experimental; no product key).',
      },
      // Credentials (kept in the system credential store, not in regular config)
      frenzDeviceId: {
        label: 'FRENZ band',
        type: 'text',
        placeholder: 'Bluetooth name, e.g. FRENZI85',
        secure: true,
        secretKey: 'frenz_device_id',
        readOnly: false,
      },
      frenzProductKey: {
        label: 'Product Key',
        type: 'password',
        placeholder: 'Enter FRENZ product key',
        secure: true,
        secretKey: 'frenz_product_key',
      },
      // Raw Signals
      streamEegRaw: {
        label: 'EEG: Bluetooth raw and SDK comparison (125Hz)',
        type: 'checkbox',
        default: true,
        warning: `The SDK comparison stream (EEG_raw) ${SDK_RAW_WARNING} The Bluetooth stream is not affected.`,
      },
      streamPpgRaw: {
        label: 'PPG Raw (4ch, 25Hz)',
        type: 'checkbox',
        default: true,
        warning: `PPG_raw ${SDK_RAW_WARNING} Keep "Original firmware streams" on for a second copy.`,
      },
      streamImuRaw: {
        label: 'IMU Raw (4ch, 50Hz)',
        type: 'checkbox',
        default: true,
        warning: `IMU_raw ${SDK_RAW_WARNING} Keep "Original firmware streams" on for a second copy.`,
      },
      streamHrRaw: { label: 'Heart rate', type: 'checkbox', default: true },
      streamSpo2Raw: { label: 'SpO2', type: 'checkbox', default: true },
      streamFirmware: { label: 'Original firmware streams', type: 'checkbox', default: true },
      // Filtered Signals
      streamEegFiltered: { label: 'EEG Filtered (4ch, 125Hz)', type: 'checkbox', default: true },
      streamEogFiltered: { label: 'EOG Filtered (4ch, 125Hz)', type: 'checkbox', default: true },
      streamEmgFiltered: { label: 'EMG Filtered (4ch, 125Hz)', type: 'checkbox', default: true },
      // Derived Metrics
      streamFocus: { label: 'Focus Score (0.5Hz)', type: 'checkbox', default: true },
      streamSleepStage: { label: 'Sleep Stage (0.2Hz)', type: 'checkbox', default: true },
      streamPoas: { label: 'PoAS (0.2Hz)', type: 'checkbox', default: true },
      streamPosture: { label: 'Posture (0.2Hz)', type: 'checkbox', default: true },
      streamSignalQuality: { label: 'Signal Quality (0.2Hz)', type: 'checkbox', default: true },
      // Power Bands
      streamAlpha: { label: 'Alpha Power (5ch, 0.5Hz)', type: 'checkbox', default: true },
      streamBeta: { label: 'Beta Power (5ch, 0.5Hz)', type: 'checkbox', default: true },
      streamTheta: { label: 'Theta Power (5ch, 0.5Hz)', type: 'checkbox', default: true },
      streamGamma: { label: 'Gamma Power (5ch, 0.5Hz)', type: 'checkbox', default: true },
      streamDelta: { label: 'Delta Power (5ch, 0.5Hz)', type: 'checkbox', default: true },
    },
  };

  // Through openfrenz there is no product key and no choice of SDK streams.
  let frenzOpenfrenz = $derived(device?.id === 'frenz' && formData.source === FRENZ_OPENFRENZ);
  function sdkOnly(fieldName) {
    return frenzOpenfrenz && fieldName !== 'source' && fieldName !== 'frenzDeviceId';
  }

  // Initialize form data when device changes
  $effect(() => {
    const id = device?.id;
    if (id && isOpen) {
      untrack(initializeForm);
    }
  });

  // Detect TTL devices
  async function detectTtlDevices() {
    isDetecting = true;
    detectedTtlDevices = [];
    showDeviceSelector = false;

    try {
      const result = await listTtlDevices();
      console.log('Detection result:', result);

      if (result.success && result.data) {
        const { devices, autoSelected, count } = result.data;
        detectedTtlDevices = devices || [];

        console.log('Detected TTL devices:', detectedTtlDevices);
        console.log('Auto-selected:', autoSelected);
        console.log('Count:', count);

        // Always show the selector if we have devices, so user can see what was found
        if (detectedTtlDevices.length > 0) {
          showDeviceSelector = true;

          // If only one device, also auto-fill it
          if (autoSelected) {
            formData.port = autoSelected;
          }
        } else {
          console.warn('No TTL devices found with VID: 0x239A, PID: 0x80F1');
          alert('No TTL devices found. Make sure your device is connected.');
        }
      } else {
        console.error('Detection failed:', result.error);
        alert(`Failed to detect devices: ${result.error || 'Unknown error'}`);
      }
    } catch (error) {
      console.error('Failed to detect TTL devices:', error);
      alert(`Error detecting devices: ${error.message}`);
    } finally {
      isDetecting = false;
    }
  }

  // Select a detected device
  function selectDetectedDevice(port) {
    formData.port = port;
    showDeviceSelector = false;
  }

  async function initializeForm() {
    visibleSecrets = {};
    const config = deviceConfigs[device.id];
    if (!config) return;

    const generation = ++loadGeneration;
    isLoading = device.id === 'frenz';
    saveError = '';
    const newFormData = {};
    const newSecureFields = {};
    const existingConfig = device.config || {};

    for (const [key, fieldConfig] of Object.entries(config)) {
      if (fieldConfig.secure) {
        // Load secure fields from the system credential store
        try {
          const value = await getSecret(fieldConfig.secretKey);
          newSecureFields[key] = value || '';
        } catch (e) {
          saveError = 'Could not load saved credentials. Close and reopen configuration.';
          newSecureFields[key] = '';
        }
      } else if (fieldConfig.type === 'checkbox') {
        newFormData[key] = existingConfig[key] ?? fieldConfig.default ?? false;
      } else {
        newFormData[key] = existingConfig[key] ?? fieldConfig.default ?? '';
      }
    }

    if (generation !== loadGeneration) return;
    formData = newFormData;
    secureFields = newSecureFields;

    // Auto-detect TTL devices on load
    if (device.id === 'ttl') {
      await detectTtlDevices();
    }

    errors = {};
    isLoading = false;
  }

  function validateField(fieldName, value) {
    const config = deviceConfigs[device?.id]?.[fieldName];
    if (!config) return null;

    // Required field validation
    if (config.required && (!value || value === '')) {
      return `${config.label} is required`;
    }

    // Pattern validation
    if (config.pattern && value && !new RegExp(config.pattern).test(value)) {
      return config.errorMessage || `Invalid ${config.label} format`;
    }

    // Number validation
    if (config.type === 'number' && value !== '') {
      const num = Number(value);
      if (isNaN(num)) {
        return `${config.label} must be a number`;
      }
      if (config.min !== undefined && num < config.min) {
        return `${config.label} must be at least ${config.min}`;
      }
      if (config.max !== undefined && num > config.max) {
        return `${config.label} must be at most ${config.max}`;
      }
    }

    return null;
  }

  function validateForm() {
    const config = deviceConfigs[device?.id];
    if (!config) return false;

    const newErrors = {};
    let hasErrors = false;

    Object.keys(config).forEach(fieldName => {
      const error = validateField(fieldName, formData[fieldName]);
      if (error) {
        newErrors[fieldName] = error;
        hasErrors = true;
      }
    });

    // Neon: either an address or a pinned phone is needed. With a pinned phone the
    // bridge resolves the address itself over mDNS, so URL may stay empty.
    if (device?.id === 'pupil' && !formData.url && !formData.device_id) {
      newErrors.url = 'Enter the Companion URL or pin a phone with "Find phones"';
      hasErrors = true;
    }

    if (device?.id === 'frenz') {
      if (!secureFields.frenzDeviceId) {
        newErrors.frenzDeviceId = 'Scan for FRENZ bands and select your band.';
        hasErrors = true;
      }
      if (!frenzOpenfrenz && !secureFields.frenzProductKey?.trim()) {
        newErrors.frenzProductKey = 'Enter your Earable product key.';
        hasErrors = true;
      }
    }
    errors = newErrors;
    return !hasErrors;
  }

  // Ask the bridge to browse mDNS for Neon Companion phones (name, hardware id, IP).
  async function findPhones() {
    isDiscoveringPhones = true;
    phoneDiscoveryError = '';
    try {
      const result = await bridgeStore.discoverNeonPhones(3000);
      discoveredPhones = Array.isArray(result?.phones) ? result.phones : [];
      phoneSearched = true;
    } catch (error) {
      phoneDiscoveryError = error.message || 'Phone discovery failed';
      discoveredPhones = [];
      phoneSearched = true;
    } finally {
      isDiscoveringPhones = false;
    }
  }

  // Pin this station to a discovered phone: hardware id + its current address.
  function usePhone(phone) {
    handleFieldChange('device_id', phone.device_id);
    handleFieldChange('url', `${phone.ip}:${phone.port}`);
  }

  function handleFieldChange(fieldName, value) {
    formData[fieldName] = value;

    // Clear error for this field when user starts typing
    if (errors[fieldName]) {
      const newErrors = { ...errors };
      delete newErrors[fieldName];
      errors = newErrors;
    }
  }

  function enableAllFrenzStreams() {
    for (const [name, config] of Object.entries(deviceConfigs.frenz)) {
      if (config.type === 'checkbox') formData[name] = true;
    }
  }

  async function handleSave() {
    if (isSubmitting || isLoading) return;
    if (!validateForm()) {
      return;
    }

    isSubmitting = true;
    saveError = '';
    try {
      // Persist secure fields to the system credential store (separate from regular config)
      const config = deviceConfigs[device.id];
      const secrets = {};
      for (const [key, fieldConfig] of Object.entries(config)) {
        // Pasted keys often carry a stray space or line break, which the vendor SDK rejects at connect time.
        // A setting hidden for the chosen connection is left as it was saved: choosing openfrenz must not
        // erase the product key.
        if (fieldConfig.secure && !sdkOnly(key))
          secrets[fieldConfig.secretKey] = secureFields[key]?.trim() || null;
      }
      if (Object.keys(secrets).length) await setSecrets(secrets);

      // Convert form data to appropriate types (excludes secure fields)
      const processedConfig = {};

      Object.entries(formData).forEach(([key, value]) => {
        const fieldConfig = config[key];
        if (fieldConfig.type === 'number') {
          processedConfig[key] = value === '' ? fieldConfig.default : Number(value);
        } else if (fieldConfig.type === 'checkbox') {
          processedConfig[key] = Boolean(value);
        } else if (fieldConfig.type === 'select' && typeof fieldConfig.options?.[0] === 'number') {
          // Select elements always return strings; convert back to number for numeric options
          processedConfig[key] = value === '' ? fieldConfig.default : Number(value);
        } else {
          processedConfig[key] = value;
        }
      });

      // The connection is saved only when it is openfrenz, so settings saved for the SDK helper stay as they
      // always were.
      if (device.id === 'frenz' && processedConfig.source !== FRENZ_OPENFRENZ) {
        // Saved settings are merged, so a band switched back from openfrenz has to say so.
        if (device.config?.source) processedConfig.source = undefined;
        else delete processedConfig.source;
      }

      await onSave(device.id, processedConfig);
      isSubmitting = false;
      handleClose();
    } catch (error) {
      saveError = 'Could not save configuration. Your settings have not been confirmed saved.';
    } finally {
      isSubmitting = false;
    }
  }

  function handleClose() {
    if (isSubmitting) return;
    loadGeneration++;
    isLoading = false;
    formData = {};
    secureFields = {};
    visibleSecrets = {};
    errors = {};
    isSubmitting = false;
    onClose();
  }

  function handleKeydown(e) {
    if (e.key === 'Escape') {
      handleClose();
    } else if (e.key === 'Enter' && e.ctrlKey) {
      handleSave();
    }
  }
</script>

{#if isOpen && device}
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
        <h2>Configure {device.name}</h2>
        <button class="close-btn" onclick={handleClose}>×</button>
      </div>

      <div class="modal-body">
        <p class="instructions">
          Configure settings for your {device.name}. Use <kbd>Ctrl+Enter</kbd> to save quickly.
        </p>

        {#if isLoading}<p role="status">Loading saved settings…</p>{/if}
        {#if saveError}<p role="alert">{saveError}</p>{/if}
        <form
          class="config-form"
          onsubmit={e => {
            e.preventDefault();
            handleSave();
          }}
        >
          <fieldset disabled={isSubmitting || isLoading}>
            {#if deviceConfigs[device.id]}
              {#each Object.entries(deviceConfigs[device.id]) as [fieldName, fieldConfig]}
                {#if device.id === 'frenz' && fieldName === 'streamEegRaw'}
                  <div class="frenz-stream-actions" class:not-for-source={frenzOpenfrenz}>
                    <span>Sensor and model streams</span>
                    <button type="button" class="reveal-btn" onclick={enableAllFrenzStreams}
                      >Enable all</button
                    >
                  </div>
                {/if}
                <div class="form-group" class:not-for-source={sdkOnly(fieldName)}>
                  <label for={fieldName} class="form-label">
                    {fieldConfig.label}
                    {#if fieldConfig.required}
                      <span class="required">*</span>
                    {/if}
                    {#if fieldConfig.secure && fieldName !== 'frenzDeviceId'}
                      <span class="secure-badge">Encrypted</span>
                    {/if}
                  </label>

                  {#if device.id === 'frenz' && fieldName === 'frenzDeviceId'}
                    <FrenzDevicePicker
                      id={fieldName}
                      openfrenz={frenzOpenfrenz}
                      value={secureFields.frenzDeviceId || ''}
                      disabled={isLoading || isSubmitting}
                      onSelect={name => {
                        secureFields.frenzDeviceId = name;
                        delete errors.frenzDeviceId;
                      }}
                    />
                    {#if errors.frenzDeviceId}<p role="alert">{errors.frenzDeviceId}</p>{/if}
                  {:else if fieldConfig.secure}
                    <div class="input-with-button">
                      <input
                        id={fieldName}
                        type={fieldConfig.type === 'password' && visibleSecrets[fieldName]
                          ? 'text'
                          : fieldConfig.type}
                        class="form-input"
                        class:readonly={fieldConfig.readOnly}
                        placeholder={fieldConfig.placeholder || ''}
                        value={secureFields[fieldName] || ''}
                        readonly={fieldConfig.readOnly}
                        spellcheck={false}
                        autocapitalize="none"
                        oninput={e => (secureFields[fieldName] = e.target.value)}
                      />
                      {#if fieldConfig.type === 'password'}
                        <button
                          type="button"
                          class="reveal-btn"
                          aria-label={`${visibleSecrets[fieldName] ? 'Hide' : 'Show'} ${fieldConfig.label}`}
                          aria-controls={fieldName}
                          aria-pressed={!!visibleSecrets[fieldName]}
                          onclick={() => (visibleSecrets[fieldName] = !visibleSecrets[fieldName])}
                        >
                          {visibleSecrets[fieldName] ? 'Hide' : 'Show'}
                        </button>
                      {/if}
                      {#if !fieldConfig.readOnly && secureFields[fieldName]}
                        <button
                          type="button"
                          class="clear-btn"
                          title="Clear stored key"
                          onclick={() => (secureFields[fieldName] = '')}
                        >
                          Clear
                        </button>
                      {/if}
                    </div>
                  {:else if fieldConfig.type === 'select'}
                    <select
                      id={fieldName}
                      class="form-input"
                      class:error={errors[fieldName]}
                      value={formData[fieldName]}
                      onchange={e => handleFieldChange(fieldName, e.target.value)}
                    >
                      <option value="">Select {fieldConfig.label}</option>
                      {#each fieldConfig.options as option}
                        <option value={option}>{option}</option>
                      {/each}
                    </select>
                    {#if fieldConfig.help}<p class="field-help">{fieldConfig.help}</p>{/if}
                  {:else if fieldConfig.type === 'checkbox'}
                    <label class="checkbox-wrapper">
                      <input
                        id={fieldName}
                        type="checkbox"
                        class="form-checkbox"
                        checked={formData[fieldName]}
                        onchange={e => handleFieldChange(fieldName, e.target.checked)}
                      />
                      <span class="checkbox-label">Enable {fieldConfig.label}</span>
                    </label>
                    {#if fieldConfig.warning}
                      <p class="field-warning">{fieldConfig.warning}</p>
                    {/if}
                  {:else}
                    <div class="input-with-button">
                      <input
                        id={fieldName}
                        type={fieldConfig.type}
                        class="form-input"
                        class:error={errors[fieldName]}
                        placeholder={fieldConfig.placeholder || ''}
                        min={fieldConfig.min}
                        max={fieldConfig.max}
                        value={formData[fieldName]}
                        oninput={e => handleFieldChange(fieldName, e.target.value)}
                      />
                      {#if device.id === 'ttl' && fieldName === 'port'}
                        <button
                          type="button"
                          class="detect-btn"
                          onclick={detectTtlDevices}
                          disabled={isDetecting}
                        >
                          {isDetecting ? 'Detecting...' : 'Detect'}
                        </button>
                      {/if}
                    </div>

                    <!-- Show detected devices dropdown for TTL port -->
                    {#if device.id === 'ttl' && fieldName === 'port' && showDeviceSelector && detectedTtlDevices.length > 0}
                      <div class="device-selector">
                        <div class="selector-header">
                          Found {detectedTtlDevices.length} device(s):
                        </div>
                        {#each detectedTtlDevices as ttlDevice}
                          <button
                            type="button"
                            class="device-option"
                            onclick={() => selectDetectedDevice(ttlDevice.port)}
                          >
                            <div class="device-option-main">
                              <strong>{ttlDevice.port}</strong>
                              <span class="device-serial">S/N: {ttlDevice.serial_number}</span>
                            </div>
                            <div class="device-option-details">
                              {ttlDevice.manufacturer} - {ttlDevice.product}
                            </div>
                          </button>
                        {/each}
                      </div>
                    {/if}
                  {/if}

                  {#if errors[fieldName]}
                    <div class="form-error">{errors[fieldName]}</div>
                  {/if}
                </div>
              {/each}

              {#if device.id === 'pupil'}
                <div class="phone-finder">
                  <div class="phone-finder-header">
                    <span class="form-label">Phones on this network</span>
                    <button
                      type="button"
                      class="phone-btn"
                      onclick={findPhones}
                      disabled={isDiscoveringPhones}
                    >
                      {isDiscoveringPhones ? 'Searching…' : 'Find phones'}
                    </button>
                  </div>
                  <p class="phone-hint">
                    Pick the phone this station should always use. The bridge then connects by
                    hardware ID and refuses any other phone that answers to the same name.
                  </p>
                  {#if phoneDiscoveryError}
                    <p class="phone-error">{phoneDiscoveryError}</p>
                  {/if}
                  {#if discoveredPhones.length > 0}
                    <ul class="phone-list">
                      {#each discoveredPhones as phone (phone.device_id)}
                        <li
                          class="phone-row"
                          class:selected={formData.device_id === phone.device_id}
                        >
                          <div class="phone-main">
                            <span class="phone-name">{phone.device_name}</span>
                            <span class="phone-id">{phone.device_id}</span>
                          </div>
                          <div class="phone-meta">
                            <span>{phone.ip}:{phone.port}</span>
                            {#if phone.reachable}
                              {#if phone.battery_level != null}
                                <span>{Math.round(phone.battery_level)}% battery</span>
                              {/if}
                              {#if phone.recording_id}
                                <span class="phone-flag busy">recording</span>
                              {/if}
                              {#if phone.glasses_connected === false}
                                <span class="phone-flag warn">no glasses</span>
                              {/if}
                            {:else}
                              <span class="phone-flag warn">unreachable</span>
                            {/if}
                          </div>
                          <button
                            type="button"
                            class="phone-btn small"
                            onclick={() => usePhone(phone)}
                          >
                            {formData.device_id === phone.device_id ? 'Pinned' : 'Use this phone'}
                          </button>
                        </li>
                      {/each}
                    </ul>
                  {:else if phoneSearched && !isDiscoveringPhones && !phoneDiscoveryError}
                    <div class="phone-empty">
                      <p class="phone-hint">
                        <strong>No Neon Companion phones found.</strong> Check that the Companion app
                        is open and the phone is on the same Wi-Fi as this computer.
                      </p>
                      <p class="phone-hint">
                        Some networks (campus and guest Wi-Fi especially) block the broadcast this
                        search relies on. You can still connect: find the phone's IP address in the
                        Companion app (it shows the address it streams on), then type it into
                        <em>Neon Companion URL</em> above, e.g. <code>192.168.1.101:8080</code>. If
                        that also fails, the network is keeping devices apart; a dedicated router or
                        a phone hotspot will work.
                      </p>
                    </div>
                  {/if}
                </div>
              {/if}
            {:else}
              <div class="no-config">
                <p>No configuration options available for this device type.</p>
              </div>
            {/if}
          </fieldset>
        </form>
      </div>

      <div class="modal-footer">
        <button class="cancel-btn" onclick={handleClose} disabled={isSubmitting}> Cancel </button>
        <button class="save-btn" onclick={handleSave} disabled={isSubmitting || isLoading}>
          {isSubmitting ? 'Saving...' : 'Save Configuration'}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  fieldset {
    border: 0;
    padding: 0;
    margin: 0;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1.25rem;
  }
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

  .config-form {
    display: flex;
    flex-direction: column;
    gap: 1.25rem;
  }

  .form-group {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .form-label {
    font-weight: 500;
    color: var(--color-text-primary);
    font-size: 0.9rem;
  }

  .required {
    color: var(--color-error);
    margin-left: 0.25rem;
  }

  .form-input,
  select.form-input {
    padding: 0.75rem;
    background: var(--color-background);
    border: 1px solid var(--color-border);
    border-radius: 8px;
    color: var(--color-text-primary);
    font-size: 1rem;
    transition: all 0.2s;
  }

  .form-input:focus {
    outline: none;
    border-color: var(--color-primary);
    box-shadow: 0 0 0 3px rgba(76, 175, 80, 0.1);
  }

  .form-input.error {
    border-color: var(--color-error);
    box-shadow: 0 0 0 3px rgba(239, 68, 68, 0.1);
  }

  .form-input::placeholder {
    color: var(--color-text-secondary);
  }

  .checkbox-wrapper {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    cursor: pointer;
    padding: 0.5rem 0;
  }

  .form-checkbox {
    width: 18px;
    height: 18px;
    accent-color: var(--color-primary);
  }

  .checkbox-label {
    color: var(--color-text-primary);
    font-size: 0.95rem;
  }

  .field-warning {
    color: var(--color-text-secondary);
    font-size: 0.8rem;
    margin: 0 0 0.25rem 1.75rem;
  }

  .form-error {
    color: var(--color-error);
    font-size: 0.85rem;
    margin-top: 0.25rem;
  }

  .no-config {
    text-align: center;
    padding: 2rem;
    color: var(--color-text-secondary);
    background: var(--color-background);
    border-radius: 8px;
    border: 1px solid var(--color-border);
  }

  .modal-footer {
    display: flex;
    justify-content: flex-end;
    gap: 1rem;
    padding: 1.5rem;
    border-top: 1px solid var(--color-border);
  }

  .cancel-btn,
  .save-btn {
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

  .cancel-btn:hover:not(:disabled) {
    background: rgba(255, 255, 255, 0.05);
    color: var(--color-text-primary);
  }

  .save-btn {
    background: var(--color-primary);
    color: white;
  }

  .save-btn:hover:not(:disabled) {
    background: var(--color-primary-hover);
    transform: translateY(-1px);
    box-shadow: 0 4px 12px rgba(76, 175, 80, 0.3);
  }

  .save-btn:disabled,
  .cancel-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  /* Custom select styling */
  select.form-input {
    background-image: url("data:image/svg+xml;charset=US-ASCII,%3csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 4 5'%3e%3cpath fill='%23ffffff80' d='m2 0-2 2h4zm0 5 2-2h-4z'/%3e%3c/svg%3e");
    background-repeat: no-repeat;
    background-position: right 0.75rem center;
    background-size: 16px;
    padding-right: 2.5rem;
    appearance: none;
    cursor: pointer;
  }
  .input-with-button {
    display: flex;
    gap: 0.5rem;
  }

  .input-with-button .form-input {
    flex: 1;
    min-width: 0;
  }

  .secure-badge {
    font-size: 0.7rem;
    font-weight: 600;
    color: var(--color-primary);
    background: rgba(76, 175, 80, 0.1);
    padding: 1px 6px;
    border-radius: 4px;
    margin-left: 0.5rem;
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }

  .form-input.readonly {
    opacity: 0.7;
    cursor: default;
    background: var(--color-surface-elevated);
  }

  .clear-btn {
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--color-error);
    border-radius: 6px;
    background: transparent;
    color: var(--color-error);
    cursor: pointer;
    transition: all 0.2s;
    font-size: 0.813rem;
    font-weight: 500;
    white-space: nowrap;
  }

  .clear-btn:hover {
    background: var(--color-error);
    color: white;
  }

  .field-help {
    margin: 0.4rem 0 0;
    color: var(--color-text-secondary);
    font-size: 0.85rem;
  }

  /* Settings that belong to the other FRENZ connection. */
  .not-for-source {
    display: none;
  }

  .frenz-stream-actions {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 1rem;
    margin: 1rem 0;
    font-weight: 600;
  }

  .reveal-btn {
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--color-primary);
    border-radius: 6px;
    background: transparent;
    color: var(--color-primary);
    cursor: pointer;
    font-size: 0.813rem;
    font-weight: 500;
  }

  .reveal-btn:hover:not(:disabled) {
    background: var(--color-surface-elevated);
  }

  .detect-btn {
    padding: 0.5rem 1rem;
    border: 1px solid var(--color-primary);
    border-radius: 6px;
    background: var(--color-primary);
    color: white;
    cursor: pointer;
    transition: all 0.2s;
    font-size: 0.875rem;
    font-weight: 500;
    white-space: nowrap;
  }

  .detect-btn:hover:not(:disabled) {
    background: var(--color-primary-hover);
  }

  .detect-btn:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .device-selector {
    margin-top: 0.75rem;
    padding: 0.75rem;
    background: var(--color-surface-elevated);
    border: 1px solid var(--color-border);
    border-radius: 6px;
  }

  .selector-header {
    font-size: 0.875rem;
    color: var(--color-text-secondary);
    margin-bottom: 0.5rem;
    font-weight: 500;
  }

  .device-option {
    width: 100%;
    padding: 0.75rem;
    margin-bottom: 0.5rem;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    background: var(--color-surface);
    color: var(--color-text-primary);
    text-align: left;
    cursor: pointer;
    transition: all 0.2s;
  }

  .device-option:last-child {
    margin-bottom: 0;
  }

  .device-option:hover {
    background: var(--color-primary);
    border-color: var(--color-primary);
    color: white;
  }

  .device-option-main {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 0.25rem;
  }

  .device-serial {
    font-size: 0.75rem;
    font-family: 'SF Mono', Monaco, monospace;
    opacity: 0.8;
  }

  .device-option-details {
    font-size: 0.75rem;
    opacity: 0.7;
  }

  /* Neon phone finder */
  .phone-finder {
    padding-top: 1.25rem;
    border-top: 1px solid var(--color-border);
  }
  .phone-finder-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
  }
  .phone-hint {
    margin: 0.35rem 0 0;
    font-size: 0.8rem;
    color: var(--color-text-secondary);
  }
  .phone-hint code {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.78rem;
    padding: 0 0.3rem;
    border-radius: 4px;
    background: var(--color-surface-elevated);
  }
  .phone-hint strong,
  .phone-hint em {
    color: var(--color-text-primary);
    font-style: normal;
  }
  .phone-error {
    margin: 0.5rem 0 0;
    font-size: 0.8rem;
    color: var(--color-error);
  }
  .phone-btn {
    padding: 0.4rem 0.8rem;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    background: var(--color-surface-elevated);
    color: var(--color-text-primary);
    font-size: 0.85rem;
    cursor: pointer;
    white-space: nowrap;
  }
  .phone-btn:hover:not(:disabled) {
    border-color: var(--color-border-hover);
    background: var(--color-surface);
  }
  .phone-btn:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .phone-btn.small {
    padding: 0.3rem 0.6rem;
    font-size: 0.8rem;
  }
  .phone-list {
    list-style: none;
    margin: 0.75rem 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .phone-row {
    display: grid;
    grid-template-columns: 1fr auto;
    grid-template-areas:
      'main action'
      'meta action';
    gap: 0.15rem 0.75rem;
    align-items: center;
    padding: 0.6rem 0.75rem;
    border: 1px solid var(--color-border);
    border-radius: 6px;
  }
  .phone-row.selected {
    border-color: var(--color-primary);
    background: rgba(76, 175, 80, 0.08);
  }
  .phone-main {
    grid-area: main;
    display: flex;
    gap: 0.6rem;
    align-items: baseline;
    min-width: 0;
  }
  .phone-name {
    font-weight: 600;
  }
  .phone-id {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.8rem;
    color: var(--color-text-secondary);
  }
  .phone-meta {
    grid-area: meta;
    display: flex;
    flex-wrap: wrap;
    gap: 0.6rem;
    font-size: 0.78rem;
    color: var(--color-text-secondary);
  }
  .phone-row .phone-btn {
    grid-area: action;
  }
  .phone-flag {
    padding: 0 0.4rem;
    border-radius: 999px;
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .phone-flag.busy {
    background: rgba(220, 38, 38, 0.12);
    color: var(--color-error);
  }
  .phone-flag.warn {
    background: rgba(217, 119, 6, 0.14);
    color: var(--color-warning);
  }
</style>
