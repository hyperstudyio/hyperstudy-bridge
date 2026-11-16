// Svelte 5 runes-based WebSocket store
// This file uses .svelte.js extension to enable runes

import { tauriService } from '../services/tauri.js';

// State using Svelte 5 runes
let status = $state('disconnected');
let devices = $state(new Map());
let lastError = $state(null);
let metrics = $state({});
let ws = $state(null);
let reconnectAttempts = $state(0);
let reconnectTimeout = null;
const maxReconnectAttempts = 5;
const reconnectDelay = 1000;

// Track initialization to prevent double-init
let initialized = false;

// Timeout constants (in milliseconds)
// Connect timeout is longer to accommodate mDNS (.local) hostname resolution,
// which can take 5+ seconds on macOS before the actual HTTP request begins.
// A pinned Neon connect can take ~3 s mDNS + up to three 5 s HTTP attempts;
// the UI must not give up (and let the user retry) before the backend does.
const CONNECT_TIMEOUT_MS = 30000;
const DISCONNECT_TIMEOUT_MS = 5000;
const COMMAND_TIMEOUT_MS = 5000;

// Message handlers and callbacks
const messageHandlers = new Map();
const requestCallbacks = new Map();

// Monotonic counter for truly unique IDs (avoids collisions from rapid calls)
let requestIdCounter = 0;

// WebSocket connection management
// Note: We use globalThis.WebSocket to ensure testability - this allows tests to mock WebSocket
function connect() {
  const WS = globalThis.WebSocket;
  if (ws?.readyState === WS.OPEN) {
    console.log('WebSocket already connected');
    return;
  }

  status = 'connecting';

  try {
    const socket = new WS('ws://localhost:9000');

    socket.onopen = () => {
      console.log('WebSocket connected to bridge server');
      ws = socket;
      status = 'ready';
      lastError = null;
      reconnectAttempts = 0;

      // Query initial state
      query('devices');
      query('status');
    };

    socket.onmessage = event => {
      try {
        const message = JSON.parse(event.data);
        handleMessage(message);
      } catch (error) {
        console.error('Failed to parse message:', error);
      }
    };

    socket.onerror = error => {
      console.error('WebSocket error:', error);
      lastError = 'Connection error occurred';
    };

    socket.onclose = () => {
      console.log('WebSocket disconnected');
      ws = null;
      status = 'disconnected';

      // Clear device states
      devices.forEach((device, _id) => {
        device.status = 'disconnected';
      });
      devices = new Map(devices);

      // Reject all pending callbacks to prevent memory leaks
      requestCallbacks.forEach((callback, _id) => {
        try {
          callback(false, 'Connection closed');
        } catch (e) {
          console.error('Error rejecting callback on close:', e);
        }
      });
      requestCallbacks.clear();

      // Clear any existing reconnection timeout
      if (reconnectTimeout) {
        clearTimeout(reconnectTimeout);
        reconnectTimeout = null;
      }

      // Attempt reconnection
      if (reconnectAttempts < maxReconnectAttempts) {
        reconnectAttempts++;
        const delay = reconnectDelay * Math.pow(2, reconnectAttempts - 1);
        console.log(`Reconnecting in ${delay}ms... (attempt ${reconnectAttempts})`);
        reconnectTimeout = setTimeout(() => {
          reconnectTimeout = null;
          connect();
        }, delay);
      }
    };
  } catch (error) {
    console.error('Failed to create WebSocket:', error);
    status = 'error';
    lastError = error.message;
  }
}

// Valid message types for validation
const VALID_MESSAGE_TYPES = [
  'status',
  'data',
  'error',
  'query_result',
  'event',
  'stream_list',
  'inlet_connected',
  'inlet_disconnected',
  'outlet_created',
  'outlet_removed',
  'sync_status',
];

// Validate message structure
function validateMessage(message) {
  if (!message || typeof message !== 'object') {
    console.warn('Invalid message: not an object', message);
    return false;
  }

  if (!message.type || typeof message.type !== 'string') {
    console.warn('Invalid message: missing or invalid type field', message);
    return false;
  }

  if (!VALID_MESSAGE_TYPES.includes(message.type)) {
    console.warn(`Unknown message type: "${message.type}"`, message);
    // Still allow processing for forward compatibility, but log the warning
  }

  return true;
}

function handleMessage(message) {
  console.log('Received message:', message);

  // Validate message structure
  if (!validateMessage(message)) {
    return;
  }

  // Resolve callbacks for any message with an id (single-message protocol)
  // The backend sends status/data/error messages with the request ID embedded,
  // rather than separate ack messages
  if (message.id) {
    const callback = requestCallbacks.get(message.id);
    if (callback) {
      const isSuccess = message.type !== 'error';
      const resultMessage =
        message.type === 'error'
          ? message.message || message.payload || 'Unknown error'
          : message.status || message.payload || 'OK';
      callback(isSuccess, resultMessage);
      requestCallbacks.delete(message.id);
    }
  }

  // Call all registered message handlers
  messageHandlers.forEach(handler => {
    try {
      handler(message);
    } catch (error) {
      console.error('Message handler error:', error);
    }
  });

  switch (message.type) {
    case 'status':
      // Handle both message.status and message.payload for backwards compatibility
      updateDeviceStatus(message.device, message.status || message.payload, message.info);
      break;
    case 'data':
      handleDeviceData(message.device, message.payload);
      break;
    case 'error':
      handleError(message);
      break;
    case 'query_result':
      handleQueryResult(message);
      break;
    case 'event':
      handleEvent(message);
      break;
    // LSL-specific message types are handled by component message handlers
    case 'stream_list':
    case 'inlet_connected':
    case 'inlet_disconnected':
    case 'outlet_created':
    case 'outlet_removed':
    case 'sync_status':
      // These are handled by component-specific handlers registered via subscribe()
      break;
    default:
      // Unknown types are logged in validateMessage but still passed to handlers
      break;
  }
}

function updateDeviceStatus(deviceId, deviceStatus, info = undefined) {
  const device = devices.get(deviceId) || {
    id: deviceId,
    name: getDeviceName(deviceId),
    type: deviceId,
  };
  // Normalize status to lowercase for consistent UI display
  device.status = typeof deviceStatus === 'string' ? deviceStatus.toLowerCase() : 'disconnected';
  // Device metadata from the backend (Neon device_id/name/IP, recording state…)
  // so the card can show *which* hardware is connected, not just that one is.
  if (info !== undefined) {
    device.info = info;
  }
  device.lastUpdate = Date.now();
  devices.set(deviceId, device);
  devices = new Map(devices); // Trigger reactivity
}

function handleDeviceData(deviceId, data) {
  const handlers = messageHandlers.get(deviceId);
  if (handlers) {
    handlers.forEach(handler => handler(data));
  }
}

function handleError(message) {
  console.error('Bridge error:', message);
  lastError = message.payload || 'Unknown error';
  if (message.device) {
    updateDeviceStatus(message.device, 'Error');
  }
}

function handleQueryResult(message) {
  if (message.payload) {
    if (Array.isArray(message.payload)) {
      // Device list
      devices.clear();
      message.payload.forEach(device => {
        devices.set(device.id, device);
      });
      devices = new Map(devices); // Trigger reactivity
    } else if (message.payload.server === 'running') {
      // Status update
      metrics = message.payload;
    }
  }
}

function handleEvent(message) {
  console.log('Event received:', message);
}

function getDeviceName(deviceId) {
  const names = {
    ttl: 'TTL Pulse Generator',
    kernel: 'Kernel Flow2',
    pupil: 'Pupil Labs Neon',
    lsl: 'Lab Streaming Layer',
    mock: 'Mock Device',
  };
  return names[deviceId] || deviceId;
}

function generateId() {
  // Use monotonic counter + timestamp to guarantee uniqueness even with rapid calls
  requestIdCounter++;
  return `${Date.now()}-${requestIdCounter}-${Math.random().toString(36).substr(2, 5)}`;
}

function send(message) {
  const WS = globalThis.WebSocket;
  console.log('Attempting to send message:', message);
  console.log('WebSocket state:', ws?.readyState, 'OPEN=', WS.OPEN);

  if (ws?.readyState === WS.OPEN) {
    const messageStr = JSON.stringify(message);
    console.log('Sending to WebSocket:', messageStr);
    ws.send(messageStr);
    return true;
  } else {
    console.error('WebSocket not connected. Current state:', ws?.readyState);
    lastError = 'Not connected to bridge server';
    return false;
  }
}

function query(target) {
  const message = {
    type: 'query',
    target,
  };

  if (target.startsWith('device:')) {
    message.target = {
      device: target.split(':')[1],
    };
  }

  send(message);
}

// Public API functions
export async function connectDevice(deviceId, config = {}) {
  return new Promise((resolve, reject) => {
    const id = generateId();
    let timeoutId = null;
    console.log(`connectDevice called for ${deviceId} with id ${id}`);

    requestCallbacks.set(id, (success, message) => {
      // Clear timeout since we got a response
      if (timeoutId) clearTimeout(timeoutId);
      console.log(`Callback for ${deviceId}: success=${success}, message=${message}`);
      if (success) {
        resolve(message);
      } else {
        reject(new Error(message || 'Failed to connect device'));
      }
    });

    const sent = send({
      type: 'command',
      device: deviceId,
      action: 'connect',
      payload: config,
      id,
    });

    if (!sent) {
      requestCallbacks.delete(id);
      reject(new Error('Failed to send command'));
      return;
    }

    // Timeout to prevent callback accumulation
    timeoutId = setTimeout(() => {
      if (requestCallbacks.has(id)) {
        console.log(`Timeout for ${deviceId} - no response received`);
        requestCallbacks.delete(id);
        reject(new Error(`Request timeout for ${deviceId}`));
      }
    }, CONNECT_TIMEOUT_MS);
  });
}

/**
 * Discover Neon Companion phones on the local network via mDNS.
 * Resolves to `{ phones: [{ device_name, device_id, ip, port, reachable,
 * battery_level?, recording_id?, glasses_connected? }], count }`.
 */
export async function discoverNeonPhones(timeoutMs = 3000) {
  return new Promise((resolve, reject) => {
    const id = generateId();
    let timeoutId = null;

    requestCallbacks.set(id, (success, message) => {
      if (timeoutId) clearTimeout(timeoutId);
      if (success) {
        resolve(message);
      } else {
        reject(new Error(message || 'Failed to discover Neon phones'));
      }
    });

    const sent = send({
      type: 'command',
      device: 'pupil',
      action: 'discover_neon_phones',
      payload: { timeout_ms: timeoutMs },
      id,
    });

    if (!sent) {
      requestCallbacks.delete(id);
      reject(new Error('Failed to send command'));
      return;
    }

    timeoutId = setTimeout(() => {
      if (requestCallbacks.has(id)) {
        requestCallbacks.delete(id);
        reject(new Error('Timed out waiting for Neon phone discovery'));
      }
    }, timeoutMs + 10000);
  });
}

export async function disconnectDevice(deviceId) {
  return new Promise((resolve, reject) => {
    const id = generateId();
    let timeoutId = null;

    requestCallbacks.set(id, (success, message) => {
      // Clear timeout since we got a response
      if (timeoutId) clearTimeout(timeoutId);
      if (success) {
        resolve(message);
      } else {
        reject(new Error(message || 'Failed to disconnect device'));
      }
    });

    const sent = send({
      type: 'command',
      device: deviceId,
      action: 'disconnect',
      id,
    });

    if (!sent) {
      requestCallbacks.delete(id);
      reject(new Error('Failed to send command'));
      return;
    }

    // Timeout to prevent callback accumulation
    timeoutId = setTimeout(() => {
      if (requestCallbacks.has(id)) {
        console.log(`Timeout for disconnect ${deviceId} - no response received`);
        requestCallbacks.delete(id);
        reject(new Error(`Disconnect timeout for ${deviceId}`));
      }
    }, DISCONNECT_TIMEOUT_MS);
  });
}

export async function sendCommand(deviceId, command) {
  // For TTL, use direct Tauri command for lowest latency
  if (deviceId === 'ttl' && command === 'PULSE') {
    const result = await tauriService.sendTtlPulse();
    console.log('TTL pulse latency:', result.latency);
    return result;
  }

  return new Promise((resolve, reject) => {
    const id = generateId();
    let timeoutId = null;

    requestCallbacks.set(id, (success, message) => {
      // Clear timeout since we got a response
      if (timeoutId) clearTimeout(timeoutId);
      if (success) {
        resolve(message);
      } else {
        reject(new Error(message || 'Failed to send command'));
      }
    });

    const sent = send({
      type: 'command',
      device: deviceId,
      action: 'send',
      payload: { command },
      id,
    });

    if (!sent) {
      requestCallbacks.delete(id);
      reject(new Error('Failed to send command'));
      return;
    }

    // Timeout to prevent callback accumulation
    timeoutId = setTimeout(() => {
      if (requestCallbacks.has(id)) {
        console.log(`Timeout for sendCommand ${deviceId} - no response received`);
        requestCallbacks.delete(id);
        reject(new Error(`Command timeout for ${deviceId}`));
      }
    }, COMMAND_TIMEOUT_MS);
  });
}

export function disconnect() {
  // Clear reconnection timeout if pending
  if (reconnectTimeout) {
    clearTimeout(reconnectTimeout);
    reconnectTimeout = null;
  }

  if (ws) {
    reconnectAttempts = maxReconnectAttempts; // Prevent auto-reconnect
    ws.close();
    ws = null;
  }

  // Clear all callbacks and handlers
  requestCallbacks.clear();
  messageHandlers.clear();
}

// Initialize the WebSocket connection
// Call this once from App.svelte on mount
export function initialize() {
  if (initialized) {
    console.log('WebSocket store already initialized');
    return;
  }
  initialized = true;
  connect();
}

// Reset all state for testing - allows proper test isolation
export function _resetForTesting() {
  // Disconnect and clean up
  if (reconnectTimeout) {
    clearTimeout(reconnectTimeout);
    reconnectTimeout = null;
  }
  if (ws) {
    ws.close();
    ws = null;
  }

  // Reset all state
  status = 'disconnected';
  devices = new Map();
  lastError = null;
  metrics = {};
  reconnectAttempts = 0;
  requestIdCounter = 0;
  initialized = false;

  // Clear callbacks and handlers
  requestCallbacks.clear();
  messageHandlers.clear();
}

// Generic message sending function
export function sendMessage(message) {
  return send(message);
}

// Subscribe to message updates (for compatibility)
export function subscribe(callback) {
  // Create a unique handler ID
  const handlerId = generateId();

  // Add the callback to message handlers
  messageHandlers.set(handlerId, callback);

  // Return unsubscribe function
  return () => {
    messageHandlers.delete(handlerId);
  };
}

// Export getters for reactive state (Svelte 5 pattern for reassigned state)
export function getStatus() {
  return status;
}
export function getDevices() {
  return devices;
}
export function getLastError() {
  return lastError;
}
export function getMetrics() {
  return metrics;
}
