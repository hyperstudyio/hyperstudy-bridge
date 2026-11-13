<script>
  import { invoke } from '@tauri-apps/api/core';
  let { value = '', onSelect = () => {}, disabled = false, id = 'frenzDeviceId' } = $props();
  let bands = $state([]);
  let scanning = $state(false);
  let searched = $state(false);
  let error = $state('');
  async function scan() {
    scanning = true;
    error = '';
    bands = [];
    try {
      bands = await invoke('scan_frenz_devices');
    } catch (e) {
      error = String(e);
    } finally {
      scanning = false;
      searched = true;
    }
  }
</script>

<div class="frenz-picker">
  <p>{value ? `Selected band: ${value}` : 'No band selected'}</p>
  <button {id} type="button" onclick={scan} disabled={scanning || disabled}>
    {scanning ? 'Scanning Bluetooth…' : 'Scan for FRENZ bands'}
  </button>
  {#if error}<p role="alert">{error}</p>{/if}
  {#if searched && !scanning && !error && bands.length === 0}
    <p role="status">
      No FRENZ bands found. Turn on the band and disconnect it from the phone app or other
      acquisition software, then scan again.
    </p>
  {/if}
  {#each bands as band (band.address)}
    <button
      type="button"
      disabled={disabled || !band.selectable}
      aria-pressed={value === band.name}
      onclick={() => onSelect(band.name)}
    >
      {band.name} · {band.rssi} dBm
      {#if !band.selectable}
        — Duplicate name; turn off the other band and scan again{/if}
    </button>
  {/each}
  <p class="help">
    Scanning finds nearby bands without connecting. No device ID needs to be typed.
  </p>
</div>

<style>
  .frenz-picker {
    display: grid;
    gap: 0.6rem;
  }
  p {
    margin: 0;
  }
  button {
    text-align: left;
    padding: 0.65rem;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    background: var(--color-bg-secondary);
    color: var(--color-text-primary);
    cursor: pointer;
  }
  button:disabled {
    opacity: 0.6;
    cursor: default;
  }
  button[aria-pressed='true'] {
    border-color: var(--color-primary);
  }
  .help {
    color: var(--color-text-secondary);
    font-size: 0.85rem;
  }
</style>
