<script>
  import { onDestroy } from 'svelte';
  import * as logsStore from '../stores/logs.svelte.js';
  import { clearAllLogs } from '../services/tauri.js';
  import VirtualLogList from './VirtualLogList.svelte';

  // Props
  let { isOpen = $bindable(false) } = $props();

  // Reactive state from store - use $derived.by for computed values
  const logs = $derived.by(() => logsStore.getFilteredLogs());
  const deviceList = $derived.by(() => logsStore.getDeviceList());
  const isQuerying = $derived.by(() => logsStore.getIsQuerying());
  const dbTotalCount = $derived.by(() => logsStore.getDbTotalCount());
  const dbHasMore = $derived.by(() => logsStore.getDbHasMore());

  // Local state
  let isExporting = $state(false);
  let isClearing = $state(false);
  let searchQuery = $state(logsStore.getSearchQuery());
  let levelFilter = $state(logsStore.getLevelFilter());
  let deviceFilter = $state(logsStore.getDeviceFilter());

  // Effect to handle modal open/close
  $effect(() => {
    if (isOpen) {
      // Initialize log viewer when opening
      requestAnimationFrame(() => {
        logsStore.init().catch(err => {
          console.error('Failed to initialize log viewer:', err);
        });
      });
    } else {
      // Stop listening when closing to prevent memory leaks
      logsStore.stopListening();
    }
  });

  // Log level colors
  function getLevelColor(level) {
    switch (level) {
      case 'debug':
        return 'var(--color-text-secondary)';
      case 'info':
        return 'var(--color-info)';
      case 'warn':
        return 'var(--color-warning)';
      case 'error':
        return 'var(--color-error)';
      default:
        return 'var(--color-text-primary)';
    }
  }

  // Log level backgrounds
  function getLevelBackground(level) {
    switch (level) {
      case 'debug':
        return 'rgba(156, 163, 175, 0.1)';
      case 'info':
        return 'rgba(59, 130, 246, 0.1)';
      case 'warn':
        return 'rgba(245, 158, 11, 0.1)';
      case 'error':
        return 'rgba(239, 68, 68, 0.1)';
      default:
        return 'rgba(255, 255, 255, 0.05)';
    }
  }

  // Format timestamp with defensive checks
  function formatTimestamp(timestamp) {
    // Handle invalid or missing timestamps
    if (!timestamp || !(timestamp instanceof Date) || isNaN(timestamp.getTime())) {
      return '--:--:--.---';
    }
    return timestamp.toLocaleTimeString('en-US', {
      hour12: false,
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
      fractionalSecondDigits: 3,
    });
  }

  // Most important first; "All" shows every level. No Debug chip: Bridge saves its logs at info level and above.
  const LEVELS = [
    { value: 'all', label: 'All' },
    { value: 'error', label: 'Errors' },
    { value: 'warn', label: 'Warnings' },
    { value: 'info', label: 'Info' },
  ];
  const filtered = $derived(
    levelFilter !== 'all' || deviceFilter !== 'all' || searchQuery.trim() !== ''
  );

  function chooseLevel(level) {
    levelFilter = level;
    logsStore.setLevelFilter(level);
  }

  // Clear ALL logs from database
  async function handleClearDatabase() {
    const confirmed = confirm(
      'PERMANENTLY DELETE all logs from the database?\n\nThis cannot be undone.'
    );
    if (!confirmed) return;

    isClearing = true;
    try {
      const result = await clearAllLogs();
      if (result.success) {
        logsStore.clearLogs(); // Clear frontend buffer
        await logsStore.refreshLogsFromDatabase(); // Refresh from empty database
      } else {
        alert(`Failed to clear database: ${result.error}`);
      }
    } catch (error) {
      console.error('Error clearing database:', error);
      alert(`Error: ${error.message}`);
    } finally {
      isClearing = false;
    }
  }

  // Export logs
  async function exportLogs() {
    isExporting = true;
    try {
      await logsStore.exportLogs();
      // Success message is already logged by the store
    } catch (error) {
      console.error('Failed to export logs:', error);
      alert('Failed to export logs: ' + error.message);
    } finally {
      isExporting = false;
    }
  }

  // Handle scroll near bottom for lazy loading
  function handleScrollNearBottom() {
    if (dbHasMore && !isQuerying) {
      logsStore.loadMoreLogs();
    }
  }

  // Load more logs manually
  async function loadMore() {
    await logsStore.loadMoreLogs();
  }

  onDestroy(() => {
    // Always stop listening when component is destroyed
    logsStore.stopListening();
  });
</script>

<!-- Log Viewer Modal -->
{#if isOpen}
  <div
    class="log-modal-overlay"
    role="presentation"
    onclick={e => {
      if (e.target === e.currentTarget) isOpen = false;
    }}
    onkeydown={e => {
      if (e.key === 'Escape') isOpen = false;
    }}
  >
    <div
      class="log-modal"
      role="dialog"
      aria-modal="true"
      tabindex="-1"
      onkeydown={e => e.key === 'Escape' && (isOpen = false)}
    >
      <div class="log-header">
        <div class="log-title">
          <h2>Logs</h2>
          <p class="log-stats">
            {dbTotalCount.toLocaleString()}
            {filtered ? 'matching entries' : 'entries'}
          </p>
        </div>

        <div class="log-controls">
          <button
            class="control-btn danger-btn"
            onclick={handleClearDatabase}
            disabled={isClearing}
            aria-label="Clear database"
            title="Delete ALL logs from database"
          >
            {#if isClearing}
              <svg
                width="16"
                height="16"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                class="spin"
              >
                <circle cx="12" cy="12" r="10"></circle>
              </svg>
            {:else}
              <svg
                width="16"
                height="16"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
              >
                <polyline points="3,6 5,6 21,6"></polyline>
                <path
                  d="m19,6v14a2,2 0 0,1 -2,2H7a2,2 0 0,1 -2,-2V6m3,0V4a2,2 0 0,1 2,-2h4a2,2 0 0,1 2,2v2"
                ></path>
                <line x1="10" y1="11" x2="10" y2="17"></line>
                <line x1="14" y1="11" x2="14" y2="17"></line>
              </svg>
            {/if}
          </button>

          <button
            class="control-btn export-btn"
            onclick={exportLogs}
            disabled={isExporting}
            aria-label="Export logs"
            title="Export logs"
          >
            {#if isExporting}
              <svg
                width="16"
                height="16"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                class="spin"
              >
                <circle cx="12" cy="12" r="10"></circle>
              </svg>
            {:else}
              <svg
                width="16"
                height="16"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
              >
                <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"></path>
                <polyline points="7,10 12,15 17,10"></polyline>
                <line x1="12" y1="15" x2="12" y2="3"></line>
              </svg>
            {/if}
          </button>

          <button class="close-btn" aria-label="Close log viewer" onclick={() => (isOpen = false)}>
            <svg
              width="20"
              height="20"
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

      <div class="log-toolbar">
        <input
          type="search"
          bind:value={searchQuery}
          oninput={e => logsStore.setSearchQuery(e.target.value)}
          placeholder="Search messages"
          aria-label="Search logs"
          class="search-input"
        />
        <div class="level-chips" role="group" aria-label="Level">
          {#each LEVELS as level (level.value)}
            <button
              type="button"
              class="level-chip"
              aria-pressed={levelFilter === level.value}
              onclick={() => chooseLevel(level.value)}
            >
              {level.label}
            </button>
          {/each}
        </div>
        {#if deviceList.length > 0}
          <select
            bind:value={deviceFilter}
            onchange={e => logsStore.setDeviceFilter(e.target.value)}
            aria-label="Device"
            class="filter-select"
          >
            <option value="all">All devices</option>
            {#each deviceList as device (device)}
              <option value={device}>{device}</option>
            {/each}
          </select>
        {/if}
      </div>

      <!-- Log Content - Now using VirtualLogList for performance -->
      <div class="log-content">
        {#if logs.length === 0}
          <div class="empty-logs">
            {#if filtered}
              <p>No entries match these filters</p>
            {:else}
              <p>No logs yet</p>
              <p class="hint">Entries appear here as Bridge runs</p>
            {/if}
          </div>
        {:else}
          <VirtualLogList
            {logs}
            onScrollNearBottom={handleScrollNearBottom}
            {formatTimestamp}
            {getLevelColor}
            {getLevelBackground}
          />

          <!-- Load more indicator -->
          {#if isQuerying || dbHasMore}
            <div class="load-more-container">
              {#if isQuerying}
                <div class="loading-indicator">
                  <svg
                    width="20"
                    height="20"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2"
                    class="spin"
                  >
                    <circle cx="12" cy="12" r="10"></circle>
                  </svg>
                  <span>Loading...</span>
                </div>
              {:else if dbHasMore}
                <button class="load-more-btn" onclick={loadMore}>
                  Load more ({(dbTotalCount - logs.length).toLocaleString()} more)
                </button>
              {/if}
            </div>
          {/if}
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .log-modal-overlay {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: rgba(0, 0, 0, 0.8);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
    backdrop-filter: blur(4px);
  }

  .log-modal {
    background: var(--color-surface);
    border-radius: 12px;
    border: 1px solid var(--color-border);
    width: 90vw;
    max-width: 1200px;
    height: 80vh;
    max-height: 800px;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    box-shadow:
      0 20px 25px -5px rgba(0, 0, 0, 0.1),
      0 10px 10px -5px rgba(0, 0, 0, 0.04);
  }

  .log-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 1.5rem 2rem;
    border-bottom: 1px solid var(--color-border);
    background: var(--color-surface-elevated);
  }

  .log-title h2 {
    margin: 0;
    color: var(--color-text-primary);
    font-size: 1.5rem;
    font-weight: 600;
  }

  .log-stats {
    margin: 0.25rem 0 0;
    font-size: 0.875rem;
    color: var(--color-text-secondary);
  }

  .log-controls {
    display: flex;
    gap: 0.75rem;
    align-items: center;
  }

  .control-btn {
    padding: 0.5rem;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    background: var(--color-surface);
    color: var(--color-text-secondary);
    cursor: pointer;
    transition: all 0.2s;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .control-btn:hover {
    background: var(--color-surface-elevated);
    border-color: var(--color-border-hover);
    color: var(--color-text-primary);
  }

  .control-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .control-btn.danger-btn:hover {
    background: var(--color-error);
    border-color: var(--color-error);
    color: white;
  }

  .export-btn:hover:not(:disabled) {
    background: var(--color-primary);
    border-color: var(--color-primary);
    color: white;
  }

  .close-btn {
    padding: 0.5rem;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--color-text-secondary);
    cursor: pointer;
    transition: all 0.2s;
    display: flex;
    align-items: center;
    justify-content: center;
    margin-left: 0.5rem;
  }

  .close-btn:hover {
    background: var(--color-error);
    color: white;
  }

  .log-toolbar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.75rem;
    padding: 0.75rem 2rem;
    background: var(--color-background);
    border-bottom: 1px solid var(--color-border);
  }

  .log-toolbar .search-input {
    flex: 1 1 14rem;
  }

  .level-chips {
    display: flex;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    overflow: hidden;
  }

  .level-chip {
    padding: 0.45rem 0.8rem;
    border: none;
    border-left: 1px solid var(--color-border);
    background: var(--color-surface);
    color: var(--color-text-secondary);
    font-size: 0.8125rem;
    cursor: pointer;
  }

  .level-chip:first-child {
    border-left: none;
  }

  .level-chip:hover {
    color: var(--color-text-primary);
  }

  .level-chip[aria-pressed='true'] {
    background: var(--color-primary);
    color: white;
  }

  .level-chip:focus-visible {
    outline: 2px solid var(--color-primary);
    outline-offset: -2px;
  }

  .search-input,
  .filter-select {
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    background: var(--color-surface);
    color: var(--color-text-primary);
    font-size: 0.875rem;
    min-width: 150px;
  }

  .search-input:focus,
  .filter-select:focus {
    outline: none;
    border-color: var(--color-primary);
    box-shadow: 0 0 0 2px rgba(76, 175, 80, 0.1);
  }

  .log-content {
    flex: 1;
    overflow: hidden;
    background: var(--color-background);
    display: flex;
    flex-direction: column;
  }

  .empty-logs {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    color: var(--color-text-secondary);
    text-align: center;
  }

  .empty-logs p {
    margin: 0.5rem 0;
  }

  .empty-logs .hint {
    font-size: 0.813rem;
    opacity: 0.8;
  }

  .spin {
    animation: spin 1s linear infinite;
  }

  @keyframes spin {
    from {
      transform: rotate(0deg);
    }
    to {
      transform: rotate(360deg);
    }
  }

  /* Load more container */
  .load-more-container {
    display: flex;
    justify-content: center;
    align-items: center;
    padding: 1rem;
    flex-shrink: 0;
    border-top: 1px solid var(--color-border);
    background: var(--color-surface);
  }

  .loading-indicator {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    color: var(--color-text-secondary);
    font-size: 0.875rem;
  }

  .load-more-btn {
    padding: 0.5rem 1rem;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    background: var(--color-surface);
    color: var(--color-text-primary);
    font-size: 0.875rem;
    cursor: pointer;
    transition: all 0.2s;
  }

  .load-more-btn:hover {
    background: var(--color-surface-elevated);
    border-color: var(--color-primary);
  }
</style>
