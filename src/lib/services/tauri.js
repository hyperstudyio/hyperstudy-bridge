import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { writable } from 'svelte/store';

// Store for backend events with circular buffer
const MAX_BACKEND_EVENTS = 100;
export const backendEvents = writable([]);

// Tauri command wrappers
export async function startBridgeServer() {
  try {
    const result = await invoke('start_bridge_server');
    console.log('Bridge server started:', result);
    return result;
  } catch (error) {
    console.error('Failed to start bridge server:', error);
    throw error;
  }
}

export async function stopBridgeServer() {
  try {
    const result = await invoke('stop_bridge_server');
    console.log('Bridge server stopped:', result);
    return result;
  } catch (error) {
    console.error('Failed to stop bridge server:', error);
    throw error;
  }
}

export async function getBridgeStatus() {
  try {
    return await invoke('get_bridge_status');
  } catch (error) {
    console.error('Failed to get bridge status:', error);
    throw error;
  }
}

// Direct device commands via Tauri (bypassing WebSocket)
export async function connectDeviceDirect(deviceType, config = {}) {
  try {
    return await invoke('connect_device', {
      deviceType,
      config,
    });
  } catch (error) {
    console.error(`Failed to connect ${deviceType}:`, error);
    throw error;
  }
}

export async function disconnectDeviceDirect(deviceId) {
  try {
    return await invoke('disconnect_device', { deviceId });
  } catch (error) {
    console.error(`Failed to disconnect ${deviceId}:`, error);
    throw error;
  }
}

export async function sendDeviceCommand(deviceId, command) {
  try {
    return await invoke('send_device_command', {
      deviceId,
      command,
    });
  } catch (error) {
    console.error(`Failed to send command to ${deviceId}:`, error);
    throw error;
  }
}

// TTL-specific commands for low-latency operations
export async function sendTtlPulse(port, pulseDurationMs) {
  try {
    const startTime = performance.now();
    const result = await invoke('send_ttl_pulse', { port, pulseDurationMs });
    const latency = performance.now() - startTime;

    console.log(`TTL pulse sent in ${latency.toFixed(2)}ms`);
    return { result, latency };
  } catch (error) {
    console.error('Failed to send TTL pulse:', error);
    throw error;
  }
}

export async function listSerialPorts() {
  try {
    return await invoke('list_serial_ports');
  } catch (error) {
    console.error('Failed to list serial ports:', error);
    return [];
  }
}

// TTL device discovery with VID/PID filtering
export async function listTtlDevices() {
  try {
    return await invoke('list_ttl_devices');
  } catch (error) {
    console.error('Failed to list TTL devices:', error);
    return { success: false, error: error.message };
  }
}

export async function findTtlPortBySerial(serialNumber) {
  try {
    return await invoke('find_ttl_port_by_serial', { serialNumber });
  } catch (error) {
    console.error('Failed to find TTL device by serial:', error);
    return { success: false, error: error.message };
  }
}

// Test TTL device connection without keeping it open
// Returns device response (e.g., firmware version) or error
export async function testTtlDevice(port) {
  try {
    return await invoke('test_ttl_device', { port });
  } catch (error) {
    console.error('Failed to test TTL device:', error);
    return { success: false, error: error.message };
  }
}

// Reset a device - clears error state and allows fresh connection
export async function resetDevice(deviceId) {
  try {
    return await invoke('reset_device', { deviceId });
  } catch (error) {
    console.error('Failed to reset device:', error);
    return { success: false, error: error.message };
  }
}

// Device discovery
export async function discoverDevices() {
  try {
    return await invoke('discover_devices');
  } catch (error) {
    console.error('Failed to discover devices:', error);
    return [];
  }
}

// Metrics and diagnostics
export async function getSystemDiagnostics() {
  try {
    return await invoke('get_system_diagnostics');
  } catch (error) {
    console.error('Failed to get system diagnostics:', error);
    return null;
  }
}

// Event listeners for backend updates
let eventUnlisteners = [];

export async function setupEventListeners() {
  // Clean up existing listeners
  cleanupEventListeners();

  // Device status updates
  const unlistenStatus = await listen('device_status_changed', event => {
    console.log('Device status changed:', event.payload);
    backendEvents.update(events => {
      const newEvents = [
        ...events,
        {
          type: 'status',
          ...event.payload,
          timestamp: Date.now(),
        },
      ];
      // Maintain circular buffer to prevent memory leak
      return newEvents.slice(-MAX_BACKEND_EVENTS);
    });
  });

  // Device data events
  const unlistenData = await listen('device_data', event => {
    console.log('Device data received:', event.payload);
    backendEvents.update(events => {
      const newEvents = [
        ...events,
        {
          type: 'data',
          ...event.payload,
          timestamp: Date.now(),
        },
      ];
      return newEvents.slice(-MAX_BACKEND_EVENTS);
    });
  });

  // Error events
  const unlistenError = await listen('device_error', event => {
    console.error('Device error:', event.payload);
    backendEvents.update(events => {
      const newEvents = [
        ...events,
        {
          type: 'error',
          ...event.payload,
          timestamp: Date.now(),
        },
      ];
      return newEvents.slice(-MAX_BACKEND_EVENTS);
    });
  });

  // Connection events
  const unlistenConnection = await listen('bridge_connection', event => {
    console.log('Bridge connection event:', event.payload);
    backendEvents.update(events => {
      const newEvents = [
        ...events,
        {
          type: 'connection',
          ...event.payload,
          timestamp: Date.now(),
        },
      ];
      return newEvents.slice(-MAX_BACKEND_EVENTS);
    });
  });

  eventUnlisteners = [unlistenStatus, unlistenData, unlistenError, unlistenConnection];

  console.log('Backend event listeners setup complete');
}

export function cleanupEventListeners() {
  eventUnlisteners.forEach(unlisten => {
    if (typeof unlisten === 'function') {
      unlisten();
    }
  });
  eventUnlisteners = [];
  console.log('Event listeners cleaned up');
}

// Logging commands
export async function getLogs() {
  try {
    return await invoke('get_logs');
  } catch (error) {
    console.error('Failed to get logs:', error);
    throw error;
  }
}

/**
 * Query logs from the database with filtering and pagination.
 * @param {Object} options - Query options
 * @param {number} [options.limit=100] - Maximum number of logs to return
 * @param {number} [options.offset=0] - Number of logs to skip (for pagination)
 * @param {string} [options.level] - Filter by log level (debug, info, warn, error)
 * @param {string} [options.device] - Filter by device name
 * @param {string} [options.search] - Search term to match against message
 * @param {string} [options.fromTimestamp] - Filter logs after this ISO 8601 timestamp
 * @param {string} [options.toTimestamp] - Filter logs before this ISO 8601 timestamp
 * @param {string} [options.sessionId] - Filter by session ID
 * @returns {Promise<{logs: Array, totalCount: number, hasMore: boolean}>}
 */
export async function queryLogs(options = {}) {
  try {
    return await invoke('query_logs', {
      limit: options.limit,
      offset: options.offset,
      level: options.level,
      device: options.device,
      search: options.search,
      fromTimestamp: options.fromTimestamp,
      toTimestamp: options.toTimestamp,
      sessionId: options.sessionId,
    });
  } catch (error) {
    console.error('Failed to query logs:', error);
    throw error;
  }
}

/**
 * Get log statistics (counts by level).
 * @returns {Promise<{debug: number, info: number, warn: number, error: number}>}
 */
export async function getLogStats() {
  try {
    return await invoke('get_log_stats');
  } catch (error) {
    console.error('Failed to get log stats:', error);
    throw error;
  }
}

/**
 * Get storage statistics (database size, record counts).
 * @returns {Promise<{logCount: number, sampleCount: number, streamCount: number, sessionCount: number, databaseSizeBytes: number}>}
 */
export async function getStorageStats() {
  try {
    return await invoke('get_storage_stats');
  } catch (error) {
    console.error('Failed to get storage stats:', error);
    throw error;
  }
}

/**
 * Start a new recording session.
 * @param {Object} [metadata] - Optional session metadata
 * @returns {Promise<string>} Session ID
 */
export async function startSession(metadata = null) {
  try {
    return await invoke('start_session', { metadata });
  } catch (error) {
    console.error('Failed to start session:', error);
    throw error;
  }
}

/**
 * End the current recording session.
 */
export async function endSession() {
  try {
    return await invoke('end_session');
  } catch (error) {
    console.error('Failed to end session:', error);
    throw error;
  }
}

/**
 * List all recording sessions.
 * @param {number} [limit=100] - Maximum number of sessions to return
 * @returns {Promise<Array<{id: string, startedAt: string, endedAt: string|null, metadata: string|null}>>}
 */
export async function listSessions(limit = 100) {
  try {
    return await invoke('list_sessions', { limit });
  } catch (error) {
    console.error('Failed to list sessions:', error);
    throw error;
  }
}

/**
 * Clean up old logs from the database.
 * @param {number} olderThanDays - Delete logs older than this many days
 * @returns {Promise<number>} Number of deleted log entries
 */
export async function cleanupOldLogs(olderThanDays) {
  try {
    return await invoke('cleanup_old_logs', { olderThanDays });
  } catch (error) {
    console.error('Failed to cleanup old logs:', error);
    throw error;
  }
}

/**
 * Clear ALL logs from the database.
 * @returns {Promise<{success: boolean, data?: number, error?: string}>} Number of deleted log entries
 */
export async function clearAllLogs() {
  try {
    return await invoke('clear_all_logs');
  } catch (error) {
    console.error('Failed to clear all logs:', error);
    throw error;
  }
}

export async function exportLogs(logsData, filePath = null) {
  try {
    return await invoke('export_logs', { logsData, filePath });
  } catch (error) {
    console.error('Failed to export logs:', error);
    throw error;
  }
}

// FRENZ bridge commands

/**
 * Start the FRENZ Python bridge process.
 * @param {string} deviceId - FRENZ device ID
 * @param {string} productKey - FRENZ product key
 * @returns {Promise<{success: boolean, data?: string, error?: string}>}
 */
export async function startFrenzBridge(deviceId, productKey) {
  try {
    return await invoke('start_frenz_bridge', { deviceId, productKey });
  } catch (error) {
    console.error('Failed to start FRENZ bridge:', error);
    return { success: false, error: error.message || String(error) };
  }
}

/**
 * Stop the FRENZ Python bridge process.
 * @returns {Promise<{success: boolean, data?: string, error?: string}>}
 */
export async function stopFrenzBridge() {
  try {
    return await invoke('stop_frenz_bridge');
  } catch (error) {
    console.error('Failed to stop FRENZ bridge:', error);
    return { success: false, error: error.message || String(error) };
  }
}

/**
 * Get the current status of the FRENZ bridge process.
 * @returns {Promise<{state: string, message?: string, phase?: string, streams: string[], sample_count: number}>}
 */
export async function getFrenzBridgeStatus() {
  try {
    return await invoke('get_frenz_bridge_status');
  } catch (error) {
    console.error('Failed to get FRENZ bridge status:', error);
    return { state: 'stopped', streams: [], sample_count: 0 };
  }
}

/**
 * Check if the FRENZ bridge binary is available on this platform.
 * @returns {Promise<boolean>}
 */
export async function checkFrenzBridgeAvailable() {
  try {
    return await invoke('check_frenz_bridge_available');
  } catch (error) {
    console.error('Failed to check FRENZ bridge availability:', error);
    return false;
  }
}

// LSL-specific commands
export async function discoverLslStreams() {
  try {
    return await invoke('discover_lsl_streams');
  } catch (error) {
    console.error('Failed to discover LSL streams:', error);
    return [];
  }
}

export async function connectLslInlet(streamInfo) {
  try {
    return await invoke('connect_lsl_inlet', { streamInfo });
  } catch (error) {
    console.error('Failed to connect LSL inlet:', error);
    throw error;
  }
}

export async function disconnectLslInlet(inletId) {
  try {
    return await invoke('disconnect_lsl_inlet', { inletId });
  } catch (error) {
    console.error('Failed to disconnect LSL inlet:', error);
    throw error;
  }
}

export async function createLslOutlet(deviceType, outletConfig) {
  try {
    return await invoke('create_lsl_outlet', {
      deviceType,
      config: outletConfig,
    });
  } catch (error) {
    console.error('Failed to create LSL outlet:', error);
    throw error;
  }
}

export async function removeLslOutlet(outletId) {
  try {
    return await invoke('remove_lsl_outlet', { outletId });
  } catch (error) {
    console.error('Failed to remove LSL outlet:', error);
    throw error;
  }
}

export async function getLslSyncStatus() {
  try {
    return await invoke('get_lsl_sync_status');
  } catch (error) {
    console.error('Failed to get LSL sync status:', error);
    return { quality: 0, offset: 0, jitter: 0 };
  }
}

export async function configureLslOutlet(deviceId, config) {
  try {
    return await invoke('configure_lsl_outlet', {
      deviceId,
      config,
    });
  } catch (error) {
    console.error('Failed to configure LSL outlet:', error);
    throw error;
  }
}

export async function getLslStreamInfo(streamUid) {
  try {
    return await invoke('get_lsl_stream_info', { streamUid });
  } catch (error) {
    console.error('Failed to get LSL stream info:', error);
    return null;
  }
}

export async function setLslBufferSize(inletId, bufferSize) {
  try {
    return await invoke('set_lsl_buffer_size', { inletId, bufferSize });
  } catch (error) {
    console.error('Failed to set LSL buffer size:', error);
    throw error;
  }
}

export async function getLslMetrics() {
  try {
    return await invoke('get_lsl_metrics');
  } catch (error) {
    console.error('Failed to get LSL metrics:', error);
    return null;
  }
}

/**
 * Get application info from Cargo.toml metadata.
 * @returns {Promise<{name: string, version: string, description: string, authors: string[], license: string, repository: string, homepage: string}>}
 */
export async function getAppInfo() {
  try {
    return await invoke('get_app_info');
  } catch (error) {
    console.error('Failed to get app info:', error);
    return null;
  }
}

// Export all functions as a service object for convenience
export const tauriService = {
  startBridgeServer,
  stopBridgeServer,
  getBridgeStatus,
  connectDeviceDirect,
  disconnectDeviceDirect,
  sendDeviceCommand,
  sendTtlPulse,
  listSerialPorts,
  listTtlDevices,
  findTtlPortBySerial,
  testTtlDevice,
  resetDevice,
  discoverDevices,
  getSystemDiagnostics,
  setupEventListeners,
  cleanupEventListeners,
  // Logging and storage
  getLogs,
  queryLogs,
  getLogStats,
  getStorageStats,
  startSession,
  endSession,
  listSessions,
  cleanupOldLogs,
  clearAllLogs,
  exportLogs,
  // FRENZ bridge
  startFrenzBridge,
  stopFrenzBridge,
  getFrenzBridgeStatus,
  checkFrenzBridgeAvailable,
  // LSL functions
  discoverLslStreams,
  connectLslInlet,
  disconnectLslInlet,
  createLslOutlet,
  removeLslOutlet,
  getLslSyncStatus,
  configureLslOutlet,
  getLslStreamInfo,
  setLslBufferSize,
  getLslMetrics,
  // App info
  getAppInfo,
};
