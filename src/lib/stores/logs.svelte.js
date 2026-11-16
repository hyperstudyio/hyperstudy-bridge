// Svelte 5 runes-based logs store - redesigned for performance
// Key improvements:
// 1. Batched event handling (log_batch instead of log_event)
// 2. Separated state concerns
// 3. Pre-computed filter index
// 4. Bounded stream buffer

import {
  tauriService,
  queryLogs as queryLogsFromDb,
  getLogStats as getLogStatsFromDb,
  getStorageStats as getStorageStatsFromDb,
} from '../services/tauri.js';
import { listen } from '@tauri-apps/api/event';
import { save } from '@tauri-apps/plugin-dialog';
import { downloadDir, join } from '@tauri-apps/api/path';

// Log levels
export const LOG_LEVELS = {
  DEBUG: 'debug',
  INFO: 'info',
  WARN: 'warn',
  ERROR: 'error',
};

// Log entry structure
export class LogEntry {
  constructor(level, message, timestamp = new Date(), device = null, source = 'bridge') {
    this.id = `${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;
    this.level = level;
    this.message = message;
    this.timestamp = timestamp;
    this.device = device;
    this.source = source;
  }
}

// ============================================================================
// STATE - Separated by concern for better performance
// ============================================================================

// Stream buffer - holds recent logs for real-time display
const streamBuffer = $state({
  logs: [],
  maxSize: 200, // Reduced from 1000 - only keep recent logs in memory
});

// Filter configuration
const filterConfig = $state({
  level: 'all',
  device: 'all',
  search: '',
});

// View state
const viewState = $state({
  isListening: false,
  isSettingUp: false,
  lastError: null,
});

// Database query state
const dbState = $state({
  isQuerying: false,
  totalCount: 0,
  hasMore: false,
  offset: 0,
  pageSize: 100,
});

// ============================================================================
// FILTER INDEX - Pre-computed for O(1) access
// ============================================================================

// The viewer shows at most a few thousand loaded entries, so filtering them on each read is cheap and never stale.
// Changing a filter also re-queries the database (see refreshForFilters), so matches beyond the loaded page appear.
function matchesFilters(log) {
  const { level, device, search } = filterConfig;
  if (level !== 'all' && log.level !== level) return false;
  if (device !== 'all' && log.device !== device) return false;
  const searchLower = search.toLowerCase().trim();
  if (!searchLower) return true;
  return (
    log.message.toLowerCase().includes(searchLower) ||
    (log.device && log.device.toLowerCase().includes(searchLower)) ||
    log.source.toLowerCase().includes(searchLower)
  );
}

function filtersActive() {
  const { level, device, search } = filterConfig;
  return level !== 'all' || device !== 'all' || search.trim() !== '';
}

// ============================================================================
// DEDUPLICATION CACHE
// ============================================================================

const recentLogHashes = new Set();
const MAX_HASH_CACHE = 500;

function getLogHash(timestamp, level, message) {
  return `${timestamp}-${level}-${message.substring(0, 50)}`;
}

function clearOldHashes() {
  if (recentLogHashes.size > MAX_HASH_CACHE) {
    const iterator = recentLogHashes.values();
    for (let i = 0; i < 200; i++) {
      recentLogHashes.delete(iterator.next().value);
    }
  }
}

// ============================================================================
// LOG HANDLING
// ============================================================================

// Add a single log entry
function addLog(level, message, device = null, source = 'frontend') {
  const entry = new LogEntry(level, message, new Date(), device, source);
  streamBuffer.logs.unshift(entry);

  if (streamBuffer.logs.length > streamBuffer.maxSize) {
    streamBuffer.logs.length = streamBuffer.maxSize;
  }
}

// Handle a single log from backend event (fallback path)
function addLogFromEvent(logData) {
  if (!logData || typeof logData !== 'object') {
    return;
  }

  const level = logData.level || 'info';
  const message = logData.message || '';
  const timestamp = logData.timestamp || new Date().toISOString();

  const hash = getLogHash(timestamp, level, message);
  if (recentLogHashes.has(hash)) {
    return;
  }

  const entry = new LogEntry(
    level,
    message,
    new Date(timestamp),
    logData.device,
    logData.source || 'backend'
  );

  recentLogHashes.add(hash);
  clearOldHashes();

  streamBuffer.logs.unshift(entry);

  if (streamBuffer.logs.length > streamBuffer.maxSize) {
    streamBuffer.logs.length = streamBuffer.maxSize;
  }
}

// Handle batched logs from backend (new optimized path)
function handleLogBatch(batch) {
  if (!Array.isArray(batch) || batch.length === 0) return;

  let addedCount = 0;
  for (const logData of batch) {
    if (!logData || typeof logData !== 'object') continue;

    const level = logData.level || 'info';
    const message = logData.message || '';
    const timestamp = logData.timestamp || new Date().toISOString();

    const hash = getLogHash(timestamp, level, message);
    if (recentLogHashes.has(hash)) continue;

    const entry = new LogEntry(
      level,
      message,
      new Date(timestamp),
      logData.device,
      logData.source || 'backend'
    );

    recentLogHashes.add(hash);
    streamBuffer.logs.unshift(entry);
    addedCount++;
  }

  // Trim buffer after batch processing
  if (streamBuffer.logs.length > streamBuffer.maxSize) {
    streamBuffer.logs.length = streamBuffer.maxSize;
  }

  clearOldHashes();
}

// ============================================================================
// GETTERS - Access to filtered/derived data
// ============================================================================

// Loaded entries that match the current filters (new live entries included)
function getFilteredLogs() {
  return filtersActive() ? streamBuffer.logs.filter(matchesFilters) : streamBuffer.logs;
}

// Number of loaded entries that match the current filters
function getTotalCount() {
  return getFilteredLogs().length;
}

// Get unique device list from logs
function getDeviceList() {
  const devices = new Set();
  streamBuffer.logs.forEach(log => {
    if (log.device) {
      devices.add(log.device);
    }
  });
  return Array.from(devices).sort();
}

// Get log count by level
function getLogCounts() {
  const counts = {
    total: streamBuffer.logs.length,
    debug: 0,
    info: 0,
    warn: 0,
    error: 0,
  };

  streamBuffer.logs.forEach(log => {
    if (Object.prototype.hasOwnProperty.call(counts, log.level)) {
      counts[log.level]++;
    }
  });

  return counts;
}

// ============================================================================
// EVENT LISTENERS
// ============================================================================

let unlistenLogBatch = null;
let unlistenLogEvent = null;

// Start listening for log events
async function startListening() {
  if (viewState.isListening || viewState.isSettingUp) return;

  viewState.isSettingUp = true;

  try {
    // Listen for batched logs (new optimized path)
    unlistenLogBatch = await listen('log_batch', event => {
      handleLogBatch(event.payload);
    });

    // Also listen for individual log events (fallback/compatibility)
    unlistenLogEvent = await listen('log_event', event => {
      addLogFromEvent(event.payload);
    });

    viewState.isListening = true;
    viewState.lastError = null;
  } catch (error) {
    console.error('Failed to set up log event listener:', error);
    viewState.lastError = error.message;
  } finally {
    viewState.isSettingUp = false;
  }
}

// Stop listening for log events
function stopListening() {
  if (unlistenLogBatch) {
    unlistenLogBatch();
    unlistenLogBatch = null;
  }
  if (unlistenLogEvent) {
    unlistenLogEvent();
    unlistenLogEvent = null;
  }
  viewState.isListening = false;
}

// ============================================================================
// DATABASE QUERIES
// ============================================================================

// Fetch historical logs from backend (for late-joining clients)
async function fetchHistoricalLogs() {
  try {
    const result = await tauriService.getLogs();
    if (result.success && result.data && Array.isArray(result.data)) {
      const newLogs = [];

      result.data.forEach(logData => {
        if (!logData || typeof logData !== 'object') return;

        const level = logData.level || 'info';
        const message = logData.message || '';
        const timestamp = logData.timestamp || new Date().toISOString();

        const hash = getLogHash(timestamp, level, message);
        if (!recentLogHashes.has(hash)) {
          const entry = new LogEntry(
            level,
            message,
            new Date(timestamp),
            logData.device,
            logData.source || 'backend'
          );
          newLogs.push(entry);
          recentLogHashes.add(hash);
        }
      });

      if (newLogs.length > 0) {
        // Backend returns logs already sorted by timestamp (newest first)
        // Simply prepend new logs and truncate to maxSize - no sorting needed
        // This avoids an expensive O(n log n) sort on the main thread
        streamBuffer.logs.length = 0;
        streamBuffer.logs.push(...newLogs.slice(0, streamBuffer.maxSize));
      }

      viewState.lastError = null;
    }
  } catch (error) {
    console.error('Failed to fetch historical logs:', error);
    viewState.lastError = error?.message || 'Unknown error';
  }
}

// Each query gets a generation; results from a query that a newer one (or a clear) superseded are dropped, so a slow
// query for an old filter can never overwrite the current view.
let queryGeneration = 0;

// Query logs from database with filtering and pagination
async function queryFromDatabase(options = {}) {
  // Paging only continues the current query; a filter change always starts a new one.
  if (options.append && dbState.isQuerying) return;

  const generation = ++queryGeneration;
  dbState.isQuerying = true;
  try {
    const queryOptions = {
      limit: options.limit || dbState.pageSize,
      offset: options.offset ?? dbState.offset,
      level: filterConfig.level !== 'all' ? filterConfig.level : undefined,
      device: filterConfig.device !== 'all' ? filterConfig.device : undefined,
      search: filterConfig.search.trim() || undefined,
    };

    const result = await queryLogsFromDb(queryOptions);
    if (generation !== queryGeneration) return;

    if (result?.success && result.data) {
      const dbLogs = result.data.logs.map(
        log => new LogEntry(log.level, log.message, new Date(log.timestamp), log.device, log.source)
      );

      if (options.append) {
        streamBuffer.logs.push(...dbLogs);
      } else {
        // The log database is written in batches, so the newest live entries may not be in it yet. Keep loaded
        // entries newer than its newest row; the rest of the view comes from the database.
        const newestSaved = dbLogs.length ? dbLogs[0].timestamp.getTime() : -Infinity;
        const unsaved = streamBuffer.logs.filter(entry => entry.timestamp.getTime() > newestSaved);
        streamBuffer.logs.length = 0;
        streamBuffer.logs.push(...unsaved, ...dbLogs);
      }

      dbState.totalCount = result.data.total_count;
      dbState.hasMore = result.data.has_more;
      dbState.offset = queryOptions.offset + dbLogs.length;
      viewState.lastError = null;
    } else {
      throw new Error(result?.error || 'Failed to query logs');
    }
  } catch (error) {
    if (generation !== queryGeneration) return;
    console.error('Failed to query logs from database:', error);
    viewState.lastError = error?.message || 'Query failed';
  } finally {
    if (generation === queryGeneration) dbState.isQuerying = false;
  }
}

// Re-run the database query for the current filters. Typing in the search box waits for a pause.
let searchRefreshTimeout = null;
function refreshForFilters({ debounce = false } = {}) {
  if (searchRefreshTimeout) clearTimeout(searchRefreshTimeout);
  searchRefreshTimeout = null;
  if (!debounce) {
    refreshLogsFromDatabase();
    return;
  }
  searchRefreshTimeout = setTimeout(() => {
    searchRefreshTimeout = null;
    refreshLogsFromDatabase();
  }, 250);
}

// Load more logs from database (pagination)
async function loadMoreLogs() {
  if (!dbState.hasMore || dbState.isQuerying) return;
  await queryFromDatabase({ append: true });
}

// Refresh logs from database (reset pagination)
async function refreshLogsFromDatabase() {
  dbState.offset = 0;
  await queryFromDatabase({ offset: 0 });
}

// Get database statistics
async function getDatabaseStats() {
  try {
    const result = await getStorageStatsFromDb();
    if (result.success) {
      return result.data;
    }
    return null;
  } catch (error) {
    console.error('Failed to get database stats:', error);
    return null;
  }
}

// Get log statistics from database
async function getDbLogStats() {
  try {
    const result = await getLogStatsFromDb();
    if (result.success) {
      return result.data;
    }
    return null;
  } catch (error) {
    console.error('Failed to get log stats:', error);
    return null;
  }
}

// ============================================================================
// ACTIONS
// ============================================================================

// Clear all logs
function clearLogs() {
  streamBuffer.logs.length = 0;
  recentLogHashes.clear();
  queryGeneration++; // results of any query still in flight belong to the cleared view
  dbState.isQuerying = false;
}

// Export logs to file with save dialog
async function exportLogs() {
  try {
    // Generate default filename with timestamp
    const now = new Date();
    const timestamp = now.toISOString().replace(/[-:]/g, '').replace('T', '_').slice(0, 15);
    const defaultFilename = `hyperstudy_bridge_logs_${timestamp}.json`;

    // Get downloads directory as default location
    let defaultPath;
    try {
      const downloads = await downloadDir();
      defaultPath = await join(downloads, defaultFilename);
    } catch {
      defaultPath = defaultFilename;
    }

    // Show native save dialog
    const filePath = await save({
      defaultPath,
      filters: [
        {
          name: 'JSON Files',
          extensions: ['json'],
        },
      ],
      title: 'Export Logs',
    });

    // User cancelled the dialog
    if (!filePath) {
      return null;
    }

    const logsToExport = getFilteredLogs().map(log => ({
      timestamp: log.timestamp.toISOString(),
      level: log.level,
      device: log.device,
      source: log.source,
      message: log.message,
    }));

    const result = await tauriService.exportLogs(logsToExport, filePath);
    if (result.success) {
      addLog('info', `Logs exported successfully to ${result.data.path}`, null, 'frontend');
      return result.data;
    } else {
      throw new Error(result.error || 'Failed to export logs');
    }
  } catch (error) {
    console.error('Failed to export logs:', error);
    addLog('error', `Failed to export logs: ${error.message}`, null, 'frontend');
    throw error;
  }
}

// Add frontend log (for local logging)
function log(level, message, device = null) {
  if (streamBuffer.logs.length === 0 && !viewState.isListening) {
    init();
  }
  addLog(level, message, device, 'frontend');
}

// Initialize logging
async function init() {
  if (viewState.isSettingUp) return;
  viewState.isSettingUp = true;

  try {
    // Load logs from database
    await refreshLogsFromDatabase();
    // Start listening for new logs
    await startListening();
  } catch (error) {
    console.error('Failed to initialize log viewer:', error);
    viewState.lastError = error?.message || 'Failed to initialize';
  } finally {
    viewState.isSettingUp = false;
  }
}

// Cleanup
function cleanup() {
  stopListening();
  if (searchRefreshTimeout) clearTimeout(searchRefreshTimeout);
  searchRefreshTimeout = null;
  queryGeneration++;
  dbState.isQuerying = false;
}

// ============================================================================
// EXPORTS
// ============================================================================

// Getters that access reactive state
export const getLogs = () => streamBuffer.logs;
export const getMaxLogs = () => streamBuffer.maxSize;
export const getIsListening = () => viewState.isListening;
export const getLastError = () => viewState.lastError;
export const getLevelFilter = () => filterConfig.level;
export const getDeviceFilter = () => filterConfig.device;
export const getSearchQuery = () => filterConfig.search;
export const getIsQuerying = () => dbState.isQuerying;
export const getDbTotalCount = () => dbState.totalCount;
export const getDbHasMore = () => dbState.hasMore;

// Derived state exports
export { getFilteredLogs, getTotalCount, getDeviceList, getLogCounts };

// Action exports
export {
  log,
  clearLogs,
  exportLogs,
  startListening,
  stopListening,
  fetchHistoricalLogs,
  cleanup,
  init,
  queryFromDatabase,
  loadMoreLogs,
  refreshLogsFromDatabase,
  getDatabaseStats,
  getDbLogStats,
};

// Filter setter exports - these trigger filter index rebuild
export const setLevelFilter = level => {
  if (filterConfig.level === level) return;
  filterConfig.level = level;
  refreshForFilters();
};

export const setDeviceFilter = device => {
  if (filterConfig.device === device) return;
  filterConfig.device = device;
  refreshForFilters();
};

export const setSearchQuery = query => {
  if (filterConfig.search === query) return;
  filterConfig.search = query;
  refreshForFilters({ debounce: true });
};

export const setMaxLogs = max => {
  streamBuffer.maxSize = max;
};
export const setDbPageSize = size => {
  dbState.pageSize = size;
};
