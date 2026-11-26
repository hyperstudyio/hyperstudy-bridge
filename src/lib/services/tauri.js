import { invoke } from '@tauri-apps/api/core';

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

// TTL device discovery with VID/PID filtering
export async function listTtlDevices() {
  try {
    return await invoke('list_ttl_devices');
  } catch (error) {
    console.error('Failed to list TTL devices:', error);
    return { success: false, error: error.message };
  }
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
 * @returns {Promise<{success: boolean, data?: {log_count: number, session_count: number, database_size_bytes: number}, error?: string}>}
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

// FRENZ through openfrenz: Bridge connects to the band itself, without the SDK helper.

/** The value of the FRENZ `source` setting that selects openfrenz. Anything else means the SDK helper. */
export const FRENZ_OPENFRENZ = 'openfrenz';

async function frenzNative(command, args, fallback) {
  try {
    return await invoke(command, args);
  } catch (error) {
    console.error(`${command} failed:`, error);
    return fallback(error);
  }
}

/** @returns {Promise<{success: boolean, data?: {name: string, rssi: number|null, selectable: boolean}[], error?: string}>} */
export function scanFrenzNative() {
  return frenzNative('scan_frenz_native', {}, e => ({ success: false, error: String(e) }));
}

/**
 * Connect through openfrenz. `models` are extra Hugging Face models (owner/name) to run; Bridge reads the token for
 * private ones from the system credential store itself.
 * @returns {Promise<{success: boolean, data?: string, error?: string}>}
 */
// `replacements`: edited copies kept in the configuration, each in place of a model ({ 'brow-emg':
// 'saved:brow-emg-edited' }).
export function startFrenzNative(deviceId, models = [], replacements = {}) {
  return frenzNative('start_frenz_native', { deviceId, models, replacements }, e => ({
    success: false,
    error: String(e),
  }));
}

/** @returns {Promise<{state: string, message?: string, device?: string, streams: string[], samples: Record<string, number>}>} */
export function getFrenzNativeStatus() {
  return frenzNative('get_frenz_native_status', {}, () => ({
    state: 'stopped',
    streams: [],
    samples: {},
  }));
}

/**
 * The last `seconds` of every stream and model output.
 * @returns {Promise<{name: string, channels: string[], t: number[], values: number[][]}[]>}
 */
export function getFrenzNativeRecent(seconds) {
  return frenzNative('get_frenz_native_recent', { seconds }, () => []);
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
  sendTtlPulse,
  listTtlDevices,
  // Logging and storage
  getLogs,
  queryLogs,
  getLogStats,
  getStorageStats,
  clearAllLogs,
  exportLogs,
  // FRENZ bridge
  startFrenzBridge,
  getFrenzBridgeStatus,
  checkFrenzBridgeAvailable,
  // App info
  getAppInfo,
};

// Pipeline editor

// `origin` names the model a pipeline was opened from (`saved:<name>` or `model:<name>`), whose stored numbers
// (trained weights, a calibration) it may use; null for a pipeline made in the editor.

/** Whether openfrenz accepts a pipeline config: { ok, outputs, inputs, error, node }. */
export function checkPipeline(config, origin = null) {
  return frenzNative('check_pipeline', { config, origin }, e => ({
    ok: false,
    outputs: [],
    inputs: [],
    error: String(e),
  }));
}

/** Runs a pipeline on the band connected through openfrenz; drawn as the trace `preview`, never published. */
export function previewPipeline(config, origin = null) {
  return frenzNative('preview_pipeline', { config, origin }, e => ({
    success: false,
    error: String(e),
  }));
}

/** A model running with the band, to open in the editor: { name, config, origin, stored, saved }. */
export function openFrenzModel(name) {
  return invoke('open_frenz_model', { name });
}

/**
 * Runs a saved pipeline with the band now, replacing a running copy, or in place of the model `insteadOf`:
 * { success, data: name, error }.
 */
export function runPipeline(name, insteadOf = null) {
  return frenzNative('run_pipeline', { name, insteadOf }, e => ({
    success: false,
    error: String(e),
  }));
}

/** Puts back the model an edited copy runs in place of, on the same stream. */
export function revertFrenzModel(name) {
  return frenzNative('revert_frenz_model', { name }, e => ({ success: false, error: String(e) }));
}

export function stopPipelinePreview() {
  return frenzNative('stop_pipeline_preview', {}, () => null);
}

/** Saved pipelines, newest first: [{ name, version, outputs, saved_at, config }]. */
export function listPipelines() {
  return frenzNative('list_pipelines', {}, () => []);
}

/** Saves a pipeline under `name` (openfrenz checks it first). Throws with openfrenz's message when refused. */
export function savePipeline(name, config, origin = null) {
  return invoke('save_pipeline', { name, config, origin });
}

export function deletePipeline(name) {
  return invoke('delete_pipeline', { name });
}

/** Copies a saved pipeline, with a model card, into `destination/<name>/`; returns that folder. */
export function exportPipeline(name, destination) {
  return invoke('export_pipeline', { name, destination });
}
