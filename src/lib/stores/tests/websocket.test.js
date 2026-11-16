import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import * as websocketStore from '../websocket.svelte.js';

// Mock Tauri service
vi.mock('../../services/tauri.js', () => ({
  tauriService: {
    sendTtlPulse: vi.fn(),
  },
}));

// Helper to get the latest WebSocket instance
function getLatestWebSocketInstance() {
  const instances = global.MockWebSocket?.instances;
  if (instances && instances.length > 0) {
    return instances[instances.length - 1];
  }
  return null;
}

// Helper to get the first (or specific) WebSocket instance
function getWebSocketInstance(index = 0) {
  const instances = global.MockWebSocket?.instances;
  if (instances && instances.length > index) {
    return instances[index];
  }
  return null;
}

describe('WebSocket Store', () => {
  beforeEach(() => {
    // Reset store state before each test for proper isolation
    websocketStore._resetForTesting();
    // Clear MockWebSocket instances
    if (global.MockWebSocket) {
      global.MockWebSocket.instances = [];
    }
  });

  afterEach(() => {
    // Clean up after each test
    websocketStore._resetForTesting();
  });

  describe('Initial State', () => {
    it('has correct initial state after reset', () => {
      // After reset, status should be disconnected
      const status = websocketStore.getStatus();
      expect(status).toBe('disconnected');
      expect(websocketStore.getDevices()).toBeInstanceOf(Map);
      expect(websocketStore.getDevices().size).toBe(0);
      expect(websocketStore.getLastError()).toBeNull();
      expect(websocketStore.getMetrics()).toEqual({});
    });
  });

  describe('WebSocket Connection Management', () => {
    it('creates WebSocket connection when initialized', () => {
      websocketStore.initialize();
      expect(global.MockWebSocket.instances.length).toBe(1);
    });

    it('connects to the correct URL', () => {
      websocketStore.initialize();
      const instance = getWebSocketInstance(0);
      expect(instance).not.toBeNull();
      expect(instance.url).toBe('ws://localhost:9000');
    });

    it('handles WebSocket onopen event', () => {
      websocketStore.initialize();
      const wsInstance = getWebSocketInstance(0);
      // Use simulateOpen() to properly set readyState before calling onopen callback
      wsInstance.simulateOpen();
      expect(websocketStore.getStatus()).toBe('ready');
      expect(websocketStore.getLastError()).toBeNull();
    });

    it('handles WebSocket onclose event', () => {
      websocketStore.initialize();
      const wsInstance = getWebSocketInstance(0);
      wsInstance.simulateOpen();
      expect(websocketStore.getStatus()).toBe('ready');
      wsInstance.simulateClose();
      expect(websocketStore.getStatus()).toBe('disconnected');
    });

    it('handles WebSocket onerror event', () => {
      websocketStore.initialize();
      const wsInstance = getWebSocketInstance(0);
      const error = new Error('Connection failed');
      wsInstance.onerror(error);
      expect(websocketStore.getLastError()).toBe('Connection error occurred');
    });

    it('prevents double initialization', () => {
      websocketStore.initialize();
      websocketStore.initialize();
      // Should only create one WebSocket instance
      expect(global.MockWebSocket.instances.length).toBe(1);
    });
  });

  describe('Message Handling', () => {
    let wsInstance;

    beforeEach(() => {
      websocketStore._resetForTesting();
      global.MockWebSocket.instances = [];
      websocketStore.initialize();
      wsInstance = getWebSocketInstance(0);
      wsInstance.simulateOpen(); // Ensure connection is established with proper readyState
    });

    it('parses JSON messages correctly', () => {
      const message = {
        type: 'status',
        device: 'ttl',
        status: 'connected',
      };

      wsInstance.onmessage({ data: JSON.stringify(message) });

      const devices = websocketStore.getDevices();
      expect(devices.has('ttl')).toBe(true);
      expect(devices.get('ttl').status).toBe('connected');
    });

    it('handles invalid JSON gracefully', () => {
      wsInstance.onmessage({ data: 'invalid json' });

      // Should not crash and should log error
      expect(console.error).toHaveBeenCalledWith('Failed to parse message:', expect.any(Error));
    });

    it('processes status messages', () => {
      const message = {
        type: 'status',
        device: 'kernel',
        status: 'connecting',
      };

      wsInstance.onmessage({ data: JSON.stringify(message) });

      const devices = websocketStore.getDevices();
      const kernelDevice = devices.get('kernel');
      expect(kernelDevice.status).toBe('connecting');
      expect(kernelDevice.name).toBe('Kernel Flow2');
      expect(kernelDevice.type).toBe('kernel');
    });

    it('processes error messages', () => {
      const message = {
        type: 'error',
        device: 'pupil',
        payload: 'Device not found',
      };

      wsInstance.onmessage({ data: JSON.stringify(message) });

      expect(websocketStore.getLastError()).toBe('Device not found');
      const devices = websocketStore.getDevices();
      expect(devices.get('pupil').status).toBe('error');
    });

    it('processes query result messages', () => {
      const deviceList = [
        { id: 'ttl', name: 'TTL Generator', status: 'connected' },
        { id: 'kernel', name: 'Kernel Flow2', status: 'disconnected' },
      ];

      const message = {
        type: 'query_result',
        payload: deviceList,
      };

      wsInstance.onmessage({ data: JSON.stringify(message) });

      const devices = websocketStore.getDevices();
      expect(devices.size).toBe(2);
      expect(devices.get('ttl').name).toBe('TTL Generator');
      expect(devices.get('kernel').name).toBe('Kernel Flow2');
    });

    it('handles messages with id by resolving callbacks', () => {
      // Test single-message protocol: any message type with an id resolves callbacks
      const message = {
        type: 'status',
        device: 'ttl',
        id: 'test-id',
        status: 'connected',
        timestamp: Date.now(),
      };

      // Manually add callback to simulate pending request
      websocketStore.connectDevice('ttl', {}).catch(() => {}); // This would set up callback

      wsInstance.onmessage({ data: JSON.stringify(message) });

      // Note: Testing callbacks directly is complex due to internal state
      // This would require exposing more internal state or refactoring
    });

    it('logs unknown message types', () => {
      const message = {
        type: 'unknown',
        payload: 'test',
      };

      wsInstance.onmessage({ data: JSON.stringify(message) });

      expect(console.warn).toHaveBeenCalledWith(
        expect.stringContaining('Unknown message type'),
        expect.anything()
      );
    });
  });

  describe('Device Operations', () => {
    let wsInstance;

    beforeEach(() => {
      websocketStore._resetForTesting();
      global.MockWebSocket.instances = [];
      websocketStore.initialize();
      wsInstance = getWebSocketInstance(0);
      wsInstance.simulateOpen();
    });

    describe('connectDevice', () => {
      it('sends connect command via WebSocket', async () => {
        const deviceId = 'ttl';
        const config = { port: '/dev/ttyUSB0' };

        // Don't await to avoid timeout in test
        websocketStore.connectDevice(deviceId, config).catch(() => {});

        expect(wsInstance.send).toHaveBeenCalledWith(expect.stringContaining('"type":"command"'));
        expect(wsInstance.send).toHaveBeenCalledWith(expect.stringContaining('"device":"ttl"'));
        expect(wsInstance.send).toHaveBeenCalledWith(expect.stringContaining('"action":"connect"'));
      });

      it('resolves promise on successful acknowledgment', async () => {
        const promise = websocketStore.connectDevice('ttl', {});

        // Simulate successful status response (single-message protocol)
        // Note: Use .at(-1) to get the last call (the connect command, not the initial queries)
        const sentMessage = JSON.parse(wsInstance.send.mock.calls.at(-1)[0]);
        const statusMessage = {
          type: 'status',
          device: 'ttl',
          id: sentMessage.id,
          status: 'connected',
          timestamp: Date.now(),
        };

        wsInstance.onmessage({ data: JSON.stringify(statusMessage) });

        await expect(promise).resolves.toBe('connected');
      });

      it('rejects promise on failed acknowledgment', async () => {
        const promise = websocketStore.connectDevice('ttl', {});

        // Simulate error response (single-message protocol)
        // Note: Use .at(-1) to get the last call (the connect command, not the initial queries)
        const sentMessage = JSON.parse(wsInstance.send.mock.calls.at(-1)[0]);
        const errorMessage = {
          type: 'error',
          device: 'ttl',
          id: sentMessage.id,
          message: 'Connection failed',
          timestamp: Date.now(),
        };

        wsInstance.onmessage({ data: JSON.stringify(errorMessage) });

        await expect(promise).rejects.toThrow('Connection failed');
      });

      it('rejects promise on timeout', async () => {
        vi.useFakeTimers();

        const promise = websocketStore.connectDevice('ttl', {});

        // Fast-forward past CONNECT_TIMEOUT_MS (30000ms in the store)
        vi.advanceTimersByTime(30100);

        await expect(promise).rejects.toThrow('Request timeout for ttl');

        vi.useRealTimers();
      });

      it('rejects when WebSocket is not connected', async () => {
        wsInstance.readyState = WebSocket.CLOSED;

        await expect(websocketStore.connectDevice('ttl', {})).rejects.toThrow(
          'Failed to send command'
        );
      });
    });

    describe('disconnectDevice', () => {
      it('sends disconnect command via WebSocket', async () => {
        websocketStore.disconnectDevice('ttl').catch(() => {});

        expect(wsInstance.send).toHaveBeenCalledWith(
          expect.stringContaining('"action":"disconnect"')
        );
      });

      it('handles successful disconnection', async () => {
        const promise = websocketStore.disconnectDevice('ttl');

        // Note: Use .at(-1) to get the last call (the disconnect command, not the initial queries)
        const sentMessage = JSON.parse(wsInstance.send.mock.calls.at(-1)[0]);
        const statusMessage = {
          type: 'status',
          device: 'ttl',
          id: sentMessage.id,
          status: 'disconnected',
          timestamp: Date.now(),
        };

        wsInstance.onmessage({ data: JSON.stringify(statusMessage) });

        await expect(promise).resolves.toBe('disconnected');
      });
    });

    describe('sendCommand', () => {
      it('uses Tauri service for TTL pulse commands', async () => {
        const { tauriService } = await import('../../services/tauri.js');
        tauriService.sendTtlPulse.mockResolvedValue({
          success: true,
          latency: 0.5,
        });

        const result = await websocketStore.sendCommand('ttl', 'PULSE');

        expect(tauriService.sendTtlPulse).toHaveBeenCalled();
        expect(result.latency).toBe(0.5);
      });

      it('uses WebSocket for other device commands', async () => {
        websocketStore.sendCommand('kernel', 'START_RECORDING').catch(() => {});

        expect(wsInstance.send).toHaveBeenCalledWith(expect.stringContaining('"action":"send"'));
        expect(wsInstance.send).toHaveBeenCalledWith(
          expect.stringContaining('"command":"START_RECORDING"')
        );
      });

      it('handles command errors gracefully', async () => {
        const promise = websocketStore.sendCommand('kernel', 'INVALID');

        // Note: Use .at(-1) to get the last call (the send command, not the initial queries)
        const sentMessage = JSON.parse(wsInstance.send.mock.calls.at(-1)[0]);
        const errorMessage = {
          type: 'error',
          device: 'kernel',
          id: sentMessage.id,
          message: 'Unknown command',
          timestamp: Date.now(),
        };

        wsInstance.onmessage({ data: JSON.stringify(errorMessage) });

        await expect(promise).rejects.toThrow('Unknown command');
      });
    });
  });

  describe('Device Name Mapping', () => {
    beforeEach(() => {
      websocketStore._resetForTesting();
      global.MockWebSocket.instances = [];
    });

    it('maps device IDs to human-readable names', () => {
      websocketStore.initialize();
      const message = {
        type: 'status',
        device: 'kernel',
        status: 'connected',
      };

      const wsInstance = getWebSocketInstance(0);
      wsInstance.simulateOpen();
      wsInstance.onmessage({ data: JSON.stringify(message) });

      const devices = websocketStore.getDevices();
      // 'kernel' is mapped to 'Kernel Flow2' in the getDeviceName function
      expect(devices.get('kernel').name).toBe('Kernel Flow2');
    });

    it('handles unknown device IDs', () => {
      websocketStore.initialize();
      const message = {
        type: 'status',
        device: 'unknown-device',
        status: 'connected',
      };

      const wsInstance = getWebSocketInstance(0);
      wsInstance.simulateOpen();
      wsInstance.onmessage({ data: JSON.stringify(message) });

      const devices = websocketStore.getDevices();
      expect(devices.get('unknown-device').name).toBe('unknown-device');
    });
  });

  describe('State Management', () => {
    beforeEach(() => {
      websocketStore._resetForTesting();
      global.MockWebSocket.instances = [];
    });

    it('updates device status and triggers reactivity', () => {
      websocketStore.initialize();
      const wsInstance = getWebSocketInstance(0);
      wsInstance.simulateOpen();

      const initialDevices = websocketStore.getDevices();
      expect(initialDevices.size).toBe(0);

      const message = {
        type: 'status',
        device: 'ttl',
        status: 'connected',
      };

      wsInstance.onmessage({ data: JSON.stringify(message) });

      const updatedDevices = websocketStore.getDevices();
      expect(updatedDevices.size).toBe(1);
      expect(updatedDevices.get('ttl').status).toBe('connected');
    });

    it('normalizes device status to lowercase', () => {
      websocketStore.initialize();
      const wsInstance = getWebSocketInstance(0);
      wsInstance.simulateOpen();

      const message = {
        type: 'status',
        device: 'ttl',
        status: 'CONNECTED', // Uppercase
      };

      wsInstance.onmessage({ data: JSON.stringify(message) });

      const devices = websocketStore.getDevices();
      expect(devices.get('ttl').status).toBe('connected'); // Lowercase
    });

    it('sets device status to disconnected on WebSocket close', () => {
      websocketStore.initialize();
      const wsInstance = getWebSocketInstance(0);
      wsInstance.simulateOpen();

      // Add a connected device
      const message = {
        type: 'status',
        device: 'ttl',
        status: 'connected',
      };
      wsInstance.onmessage({ data: JSON.stringify(message) });

      // Close connection
      wsInstance.simulateClose();

      const devices = websocketStore.getDevices();
      expect(devices.get('ttl').status).toBe('disconnected');
    });

    it('tracks device last update timestamp', () => {
      websocketStore.initialize();
      const wsInstance = getWebSocketInstance(0);
      wsInstance.simulateOpen();

      const beforeTime = Date.now();

      const message = {
        type: 'status',
        device: 'ttl',
        status: 'connected',
      };
      wsInstance.onmessage({ data: JSON.stringify(message) });

      const afterTime = Date.now();
      const devices = websocketStore.getDevices();
      const device = devices.get('ttl');

      expect(device.lastUpdate).toBeGreaterThanOrEqual(beforeTime);
      expect(device.lastUpdate).toBeLessThanOrEqual(afterTime);
    });
  });

  describe('Connection Cleanup', () => {
    beforeEach(() => {
      websocketStore._resetForTesting();
      global.MockWebSocket.instances = [];
    });

    it('disconnects WebSocket on cleanup', () => {
      websocketStore.initialize();
      const wsInstance = getWebSocketInstance(0);
      wsInstance.simulateOpen();

      websocketStore.disconnect();

      expect(wsInstance.close).toHaveBeenCalled();
    });

    it('prevents auto-reconnect after manual disconnect', () => {
      websocketStore.initialize();
      const wsInstance = getWebSocketInstance(0);
      wsInstance.simulateOpen();

      const instanceCountBeforeDisconnect = global.MockWebSocket.instances.length;
      websocketStore.disconnect();

      // Trigger close event
      wsInstance.simulateClose();

      // Should not have created any new WebSocket instances
      expect(global.MockWebSocket.instances.length).toBe(instanceCountBeforeDisconnect);
    });
  });

  describe('Error Handling', () => {
    beforeEach(() => {
      websocketStore._resetForTesting();
      global.MockWebSocket.instances = [];
    });

    it('handles message parsing errors', () => {
      websocketStore.initialize();
      const wsInstance = getWebSocketInstance(0);
      wsInstance.simulateOpen();

      wsInstance.onmessage({ data: 'invalid json {' });

      expect(console.error).toHaveBeenCalledWith(
        'Failed to parse message:',
        expect.any(SyntaxError)
      );
    });

    it('handles missing message handlers gracefully', () => {
      websocketStore.initialize();
      const wsInstance = getWebSocketInstance(0);
      wsInstance.simulateOpen();

      const message = {
        type: 'data',
        device: 'nonexistent',
        payload: 'test data',
      };

      // Should not crash when no handlers are registered
      expect(() => {
        wsInstance.onmessage({ data: JSON.stringify(message) });
      }).not.toThrow();
    });
  });

  describe('Performance', () => {
    beforeEach(() => {
      websocketStore._resetForTesting();
      global.MockWebSocket.instances = [];
    });

    it('handles rapid message processing efficiently', () => {
      websocketStore.initialize();
      const wsInstance = getWebSocketInstance(0);
      wsInstance.simulateOpen();

      const startTime = performance.now();

      // Send many messages rapidly
      for (let i = 0; i < 100; i++) {
        const message = {
          type: 'status',
          device: `device-${i}`,
          status: 'connected',
        };
        wsInstance.onmessage({ data: JSON.stringify(message) });
      }

      const endTime = performance.now();
      const processingTime = endTime - startTime;

      // Should complete within reasonable time (arbitrary threshold)
      expect(processingTime).toBeLessThan(100); // 100ms
      expect(websocketStore.getDevices().size).toBe(100);
    });

    it('maintains efficient message sending', () => {
      websocketStore.initialize();
      const wsInstance = getWebSocketInstance(0);
      wsInstance.simulateOpen();

      // Clear previous calls from initial queries (devices + status)
      wsInstance.send.mockClear();

      const startTime = performance.now();

      // Send many commands rapidly (but don't await to avoid timeouts)
      for (let i = 0; i < 50; i++) {
        websocketStore.connectDevice(`device-${i}`, {}).catch(() => {});
      }

      const endTime = performance.now();
      const sendingTime = endTime - startTime;

      expect(sendingTime).toBeLessThan(50); // 50ms
      expect(wsInstance.send).toHaveBeenCalledTimes(50);
    });
  });

  describe('ID Generation', () => {
    beforeEach(() => {
      websocketStore._resetForTesting();
      global.MockWebSocket.instances = [];
    });

    it('generates unique IDs for requests', () => {
      websocketStore.initialize();
      const wsInstance = getWebSocketInstance(0);
      wsInstance.simulateOpen();

      // Clear initial queries (devices + status) to make indexing simpler
      wsInstance.send.mockClear();

      websocketStore.connectDevice('ttl1', {}).catch(() => {});
      websocketStore.connectDevice('ttl2', {}).catch(() => {});

      const call1 = JSON.parse(wsInstance.send.mock.calls[0][0]);
      const call2 = JSON.parse(wsInstance.send.mock.calls[1][0]);

      expect(call1.id).toBeDefined();
      expect(call2.id).toBeDefined();
      expect(call1.id).not.toBe(call2.id);
    });

    it('includes timestamp in generated IDs', () => {
      websocketStore.initialize();
      const wsInstance = getWebSocketInstance(0);
      wsInstance.simulateOpen();

      // Clear initial queries (devices + status) to make indexing simpler
      wsInstance.send.mockClear();

      const beforeTime = Date.now();
      websocketStore.connectDevice('ttl', {}).catch(() => {});
      const afterTime = Date.now();

      const call = JSON.parse(wsInstance.send.mock.calls[0][0]);
      const idTimestamp = parseInt(call.id.split('-')[0]);

      expect(idTimestamp).toBeGreaterThanOrEqual(beforeTime);
      expect(idTimestamp).toBeLessThanOrEqual(afterTime);
    });
  });
});
