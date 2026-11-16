import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import {
  LOG_LEVELS,
  LogEntry,
  getLogs,
  getMaxLogs,
  getIsListening,
  getLastError,
  getLevelFilter,
  getDeviceFilter,
  getSearchQuery,
  getFilteredLogs,
  getDeviceList,
  getLogCounts,
  log,
  clearLogs,
  exportLogs,
  startListening,
  stopListening,
  fetchHistoricalLogs,
  cleanup,
  setLevelFilter,
  setDeviceFilter,
  setSearchQuery,
  setMaxLogs,
} from '../logs.svelte.js';

// Mock Tauri service
vi.mock('../../services/tauri.js', () => ({
  tauriService: {
    getLogs: vi.fn(),
    exportLogs: vi.fn(),
  },
  queryLogs: vi.fn(),
  getLogStats: vi.fn(),
  getStorageStats: vi.fn(),
}));

describe('Logs Store', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.spyOn(console, 'log').mockImplementation(() => {});
    vi.spyOn(console, 'error').mockImplementation(() => {});

    // Clear any existing logs
    clearLogs();

    // Reset filters
    setLevelFilter('all');
    setDeviceFilter('all');
    setSearchQuery('');
    setMaxLogs(1000);

    // Stop any active event listeners
    stopListening();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  describe('LogEntry Class', () => {
    it('creates log entry with default values', () => {
      const entry = new LogEntry('info', 'Test message');

      expect(entry.id).toBeDefined();
      expect(entry.level).toBe('info');
      expect(entry.message).toBe('Test message');
      expect(entry.timestamp).toBeInstanceOf(Date);
      expect(entry.device).toBeNull();
      expect(entry.source).toBe('bridge');
    });

    it('creates log entry with custom values', () => {
      const timestamp = new Date('2023-01-01T00:00:00Z');
      const entry = new LogEntry('error', 'Error occurred', timestamp, 'ttl', 'frontend');

      expect(entry.level).toBe('error');
      expect(entry.message).toBe('Error occurred');
      expect(entry.timestamp).toBe(timestamp);
      expect(entry.device).toBe('ttl');
      expect(entry.source).toBe('frontend');
    });

    it('generates unique IDs for each entry', () => {
      const entry1 = new LogEntry('info', 'Message 1');
      const entry2 = new LogEntry('info', 'Message 2');

      expect(entry1.id).not.toBe(entry2.id);
    });

    it('includes timestamp in ID generation', () => {
      const beforeTime = Date.now();
      const entry = new LogEntry('info', 'Test');
      const afterTime = Date.now();

      const idTimestamp = parseInt(entry.id.split('-')[0]);
      expect(idTimestamp).toBeGreaterThanOrEqual(beforeTime);
      expect(idTimestamp).toBeLessThanOrEqual(afterTime);
    });
  });

  describe('Log Constants', () => {
    it('defines all log levels', () => {
      expect(LOG_LEVELS.DEBUG).toBe('debug');
      expect(LOG_LEVELS.INFO).toBe('info');
      expect(LOG_LEVELS.WARN).toBe('warn');
      expect(LOG_LEVELS.ERROR).toBe('error');
    });
  });

  describe('Initial State', () => {
    it('has correct initial state', () => {
      expect(getLogs()).toEqual([]);
      expect(getMaxLogs()).toBe(1000);
      expect(getIsListening()).toBe(false);
      expect(getLastError()).toBeNull();
      expect(getLevelFilter()).toBe('all');
      expect(getDeviceFilter()).toBe('all');
      expect(getSearchQuery()).toBe('');
    });
  });

  describe('Log Management', () => {
    it('adds log entries correctly', () => {
      log('info', 'Test message', 'ttl');

      const logs = getLogs();
      expect(logs).toHaveLength(1);
      expect(logs[0].level).toBe('info');
      expect(logs[0].message).toBe('Test message');
      expect(logs[0].device).toBe('ttl');
      expect(logs[0].source).toBe('frontend');
    });

    it('adds logs to the beginning of array (newest first)', () => {
      log('info', 'First message');
      log('warn', 'Second message');

      const logs = getLogs();
      expect(logs[0].message).toBe('Second message');
      expect(logs[1].message).toBe('First message');
    });

    it('maintains circular buffer with maxLogs limit', () => {
      setMaxLogs(3);

      log('info', 'Message 1');
      log('info', 'Message 2');
      log('info', 'Message 3');
      log('info', 'Message 4');

      const logs = getLogs();
      expect(logs).toHaveLength(3);
      expect(logs[0].message).toBe('Message 4');
      expect(logs[1].message).toBe('Message 3');
      expect(logs[2].message).toBe('Message 2');
    });

    it('clears all logs', () => {
      log('info', 'Test message 1');
      log('warn', 'Test message 2');

      expect(getLogs()).toHaveLength(2);

      clearLogs();

      expect(getLogs()).toHaveLength(0);
    });

    it('handles null and undefined device values', () => {
      log('info', 'Message with null device', null);
      log('warn', 'Message with undefined device', undefined);

      const logs = getLogs();
      expect(logs).toHaveLength(2);
      expect(logs[0].device).toBeNull();
      expect(logs[1].device).toBeNull();
    });
  });

  describe('Log Filtering', () => {
    // Filters use 100ms debounce, so we need fake timers
    beforeEach(() => {
      vi.useFakeTimers();
      // Add test logs
      log('debug', 'Debug message', 'ttl');
      log('info', 'Info message', 'kernel');
      log('warn', 'Warning message', 'ttl');
      log('error', 'Error message', 'pupil');
      log('info', 'Another info message', null);
    });

    afterEach(() => {
      vi.useRealTimers();
    });

    it('filters by log level', () => {
      setLevelFilter('error');
      vi.advanceTimersByTime(150); // Wait for debounce
      const filtered = getFilteredLogs();

      expect(filtered).toHaveLength(1);
      expect(filtered[0].level).toBe('error');
    });

    it('filters by device', () => {
      setDeviceFilter('ttl');
      vi.advanceTimersByTime(150);
      const filtered = getFilteredLogs();

      expect(filtered).toHaveLength(2);
      filtered.forEach(log => expect(log.device).toBe('ttl'));
    });

    it('filters by search query in message', () => {
      setSearchQuery('warning');
      vi.advanceTimersByTime(150);
      const filtered = getFilteredLogs();

      expect(filtered).toHaveLength(1);
      expect(filtered[0].message).toBe('Warning message');
    });

    it('filters by search query in device', () => {
      setSearchQuery('kernel');
      vi.advanceTimersByTime(150);
      const filtered = getFilteredLogs();

      expect(filtered).toHaveLength(1);
      expect(filtered[0].device).toBe('kernel');
    });

    it('filters by search query in source', () => {
      setSearchQuery('frontend');
      vi.advanceTimersByTime(150);
      const filtered = getFilteredLogs();

      expect(filtered.length).toBeGreaterThan(0);
      filtered.forEach(log => expect(log.source).toBe('frontend'));
    });

    it('applies case-insensitive search', () => {
      setSearchQuery('INFO');
      vi.advanceTimersByTime(150);
      const filtered = getFilteredLogs();

      expect(filtered).toHaveLength(2);
    });

    it('combines multiple filters', () => {
      setLevelFilter('info');
      setDeviceFilter('kernel');
      vi.advanceTimersByTime(150);
      const filtered = getFilteredLogs();

      expect(filtered).toHaveLength(1);
      expect(filtered[0].level).toBe('info');
      expect(filtered[0].device).toBe('kernel');
    });

    it('returns all logs when filters are set to "all"', () => {
      setLevelFilter('all');
      setDeviceFilter('all');
      setSearchQuery('');
      vi.advanceTimersByTime(150);
      const filtered = getFilteredLogs();

      expect(filtered).toHaveLength(5);
    });

    it('handles empty search query', () => {
      setSearchQuery('');
      vi.advanceTimersByTime(150);
      const filtered = getFilteredLogs();

      expect(filtered).toHaveLength(5);
    });

    it('handles whitespace-only search query', () => {
      setSearchQuery('   ');
      vi.advanceTimersByTime(150);
      const filtered = getFilteredLogs();

      expect(filtered).toHaveLength(5);
    });
  });

  // The viewer holds one page of a log database that can have thousands of entries. Filtering only that page hid
  // most matches, and live entries briefly showed through an active filter.
  describe('Filters apply to the whole log', () => {
    const page = logs => ({
      success: true,
      data: { logs, total_count: logs.length, has_more: false },
    });
    // Answers by filter, like the database: only the query for `level` returns `rows`.
    const answer = async (level, rows) => {
      const { queryLogs } = await import('../../services/tauri.js');
      queryLogs.mockImplementation(async options => page(options.level === level ? rows : []));
      return queryLogs;
    };
    afterEach(async () => {
      const { queryLogs } = await import('../../services/tauri.js');
      queryLogs.mockReset();
    });

    it('a level filter fetches matching entries from the whole log database', async () => {
      const queryLogs = await answer('error', [
        {
          level: 'error',
          message: 'error from an hour ago',
          timestamp: '2026-11-16T14:00:00Z',
          device: null,
          source: 'bridge',
        },
      ]);
      setLevelFilter('error');
      await vi.waitFor(() =>
        expect(queryLogs).toHaveBeenCalledWith(
          expect.objectContaining({ level: 'error', offset: 0 })
        )
      );
      await vi.waitFor(() =>
        expect(getFilteredLogs().map(entry => entry.message)).toEqual(['error from an hour ago'])
      );
    });

    it('changing a filter keeps live entries the database has not saved yet', async () => {
      await answer('warn', [
        {
          level: 'warn',
          message: 'saved a minute ago',
          timestamp: new Date(Date.now() - 60000).toISOString(),
          device: null,
          source: 'bridge',
        },
      ]);
      log('warn', 'just now, not saved yet');
      setLevelFilter('warn');
      await vi.waitFor(() =>
        expect(getFilteredLogs().map(entry => entry.message)).toEqual([
          'just now, not saved yet',
          'saved a minute ago',
        ])
      );
    });

    it('a new live entry that does not match the active filter never shows', async () => {
      await answer('error', []);
      setLevelFilter('error');
      log('info', 'routine message');
      expect(getFilteredLogs().map(entry => entry.message)).not.toContain('routine message');
      log('error', 'new failure');
      expect(getFilteredLogs().map(entry => entry.message)).toContain('new failure');
    });
  });

  describe('Device List Generation', () => {
    it('extracts unique device list from logs', () => {
      log('info', 'Message 1', 'ttl');
      log('warn', 'Message 2', 'kernel');
      log('error', 'Message 3', 'ttl');
      log('debug', 'Message 4', 'pupil');
      log('info', 'Message 5', null);

      const deviceList = getDeviceList();

      expect(deviceList).toEqual(['kernel', 'pupil', 'ttl']);
      expect(deviceList).not.toContain(null);
    });

    it('returns empty array when no devices in logs', () => {
      log('info', 'Message without device', null);

      const deviceList = getDeviceList();

      expect(deviceList).toEqual([]);
    });

    it('sorts device list alphabetically', () => {
      log('info', 'Message 1', 'zebra');
      log('warn', 'Message 2', 'apple');
      log('error', 'Message 3', 'banana');

      const deviceList = getDeviceList();

      expect(deviceList).toEqual(['apple', 'banana', 'zebra']);
    });
  });

  describe('Log Count Statistics', () => {
    beforeEach(() => {
      log('debug', 'Debug 1');
      log('debug', 'Debug 2');
      log('info', 'Info 1');
      log('warn', 'Warning 1');
      log('error', 'Error 1');
      log('error', 'Error 2');
      log('error', 'Error 3');
    });

    it('counts logs by level', () => {
      const counts = getLogCounts();

      expect(counts.total).toBe(7);
      expect(counts.debug).toBe(2);
      expect(counts.info).toBe(1);
      expect(counts.warn).toBe(1);
      expect(counts.error).toBe(3);
    });

    it('handles empty logs', () => {
      clearLogs();
      const counts = getLogCounts();

      expect(counts.total).toBe(0);
      expect(counts.debug).toBe(0);
      expect(counts.info).toBe(0);
      expect(counts.warn).toBe(0);
      expect(counts.error).toBe(0);
    });

    it('ignores unknown log levels in counts', () => {
      // Directly add log with unknown level (bypassing validation)
      const logs = getLogs();
      logs.unshift({
        id: 'test',
        level: 'unknown',
        message: 'Unknown level',
        timestamp: new Date(),
        device: null,
        source: 'test',
      });

      const counts = getLogCounts();

      expect(counts.total).toBe(8); // Total includes unknown level
      expect(counts).not.toHaveProperty('unknown');
    });
  });

  describe('Backend Log Fetching', () => {
    it('fetches logs from backend successfully', async () => {
      const { tauriService } = await import('../../services/tauri.js');
      const mockBackendLogs = [
        {
          level: 'info',
          message: 'Backend message 1',
          timestamp: new Date().toISOString(),
          device: 'ttl',
          source: 'backend',
        },
        {
          level: 'error',
          message: 'Backend error',
          timestamp: new Date().toISOString(),
          device: null,
          source: 'backend',
        },
      ];

      tauriService.getLogs.mockResolvedValue({
        success: true,
        data: mockBackendLogs,
      });

      await fetchHistoricalLogs();

      const logs = getLogs();
      expect(logs).toHaveLength(2);
      // Logs are stored in the order received from backend
      expect(logs[0].message).toBe('Backend message 1');
      expect(logs[1].message).toBe('Backend error');
      expect(logs[0].source).toBe('backend');
    });

    it('handles backend fetch errors gracefully', async () => {
      const { tauriService } = await import('../../services/tauri.js');
      tauriService.getLogs.mockRejectedValue(new Error('Backend unavailable'));

      await fetchHistoricalLogs();

      expect(getLastError()).toBe('Backend unavailable');
      expect(console.error).toHaveBeenCalledWith(
        'Failed to fetch historical logs:',
        expect.any(Error)
      );
    });

    it('avoids duplicate logs from backend', async () => {
      const { tauriService } = await import('../../services/tauri.js');
      const timestamp = new Date().toISOString();
      const mockLog = {
        level: 'info',
        message: 'Duplicate message',
        timestamp: timestamp,
        device: 'ttl',
      };

      tauriService.getLogs.mockResolvedValue({
        success: true,
        data: [mockLog, mockLog], // Same log twice
      });

      await fetchHistoricalLogs();

      const logs = getLogs();
      expect(logs).toHaveLength(1); // Should only add once
    });

    it('maintains circular buffer when fetching from backend', async () => {
      setMaxLogs(2);

      const { tauriService } = await import('../../services/tauri.js');
      const mockLogs = Array.from({ length: 5 }, (_, i) => ({
        level: 'info',
        message: `Backend message ${i}`,
        timestamp: new Date(Date.now() + i).toISOString(),
        device: null,
      }));

      tauriService.getLogs.mockResolvedValue({
        success: true,
        data: mockLogs,
      });

      await fetchHistoricalLogs();

      const logs = getLogs();
      expect(logs).toHaveLength(2); // Limited by maxLogs
    });
  });

  describe('Log Event Listening', () => {
    let mockListen;

    beforeEach(async () => {
      const eventModule = await import('@tauri-apps/api/event');
      mockListen = eventModule.listen;
      mockListen.mockClear();
    });

    it('sets up event listeners when starting to listen', async () => {
      await startListening();

      // Should set up listeners for log_batch and log_event
      expect(mockListen).toHaveBeenCalledWith('log_batch', expect.any(Function));
      expect(mockListen).toHaveBeenCalledWith('log_event', expect.any(Function));
    });

    it('stops listening when requested', async () => {
      await startListening();
      expect(getIsListening()).toBe(true);

      stopListening();
      expect(getIsListening()).toBe(false);
    });

    it('does not start multiple listeners', async () => {
      await startListening();
      const firstCallCount = mockListen.mock.calls.length;

      await startListening(); // Second call should be ignored

      expect(mockListen.mock.calls.length).toBe(firstCallCount);
    });
  });

  describe('Log Export', () => {
    let mockSave;
    let mockDownloadDir;
    let mockJoin;
    let mockTauriService;

    beforeEach(async () => {
      log('info', 'Export message 1', 'ttl');
      log('warn', 'Export message 2', 'kernel');
      log('error', 'Export message 3', null);

      // Get references to mocked modules
      const dialogModule = await import('@tauri-apps/plugin-dialog');
      const pathModule = await import('@tauri-apps/api/path');
      const tauriModule = await import('../../services/tauri.js');

      mockSave = dialogModule.save;
      mockDownloadDir = pathModule.downloadDir;
      mockJoin = pathModule.join;
      mockTauriService = tauriModule.tauriService;

      // Reset and setup default mock implementations
      mockDownloadDir.mockResolvedValue('/Users/test/Downloads');
      mockJoin.mockImplementation((...paths) => Promise.resolve(paths.join('/')));
      mockSave.mockResolvedValue('/Users/test/Downloads/logs.json');
    });

    it('exports filtered logs successfully', async () => {
      mockSave.mockResolvedValue('/tmp/logs.json');
      mockTauriService.exportLogs.mockResolvedValue({
        success: true,
        data: { path: '/tmp/logs.json' },
      });

      setLevelFilter('info');
      const result = await exportLogs();

      expect(result.path).toBe('/tmp/logs.json');
      expect(mockTauriService.exportLogs).toHaveBeenCalledWith(
        expect.arrayContaining([
          expect.objectContaining({
            level: 'info',
            message: 'Export message 1',
          }),
        ]),
        '/tmp/logs.json'
      );
    });

    it('shows save dialog with correct options', async () => {
      mockSave.mockResolvedValue('/tmp/logs.json');
      mockTauriService.exportLogs.mockResolvedValue({
        success: true,
        data: { path: '/tmp/logs.json' },
      });

      await exportLogs();

      expect(mockSave).toHaveBeenCalledWith(
        expect.objectContaining({
          filters: [{ name: 'JSON Files', extensions: ['json'] }],
          title: 'Export Logs',
        })
      );
    });

    it('returns null when user cancels save dialog', async () => {
      mockSave.mockResolvedValue(null); // User cancelled

      const result = await exportLogs();

      expect(result).toBeNull();
      expect(mockTauriService.exportLogs).not.toHaveBeenCalled();
    });

    it('exports logs in correct format', async () => {
      mockSave.mockResolvedValue('/tmp/logs.json');
      mockTauriService.exportLogs.mockResolvedValue({
        success: true,
        data: { path: '/tmp/logs.json' },
      });

      await exportLogs();

      const exportCall = mockTauriService.exportLogs.mock.calls[0][0];
      const exportedLog = exportCall[0];

      expect(exportedLog).toHaveProperty('timestamp');
      expect(exportedLog).toHaveProperty('level');
      expect(exportedLog).toHaveProperty('device');
      expect(exportedLog).toHaveProperty('source');
      expect(exportedLog).toHaveProperty('message');
      expect(exportedLog.timestamp).toMatch(/^\d{4}-\d{2}-\d{2}T/); // ISO format
    });

    it('handles export errors gracefully', async () => {
      mockSave.mockResolvedValue('/tmp/logs.json');
      mockTauriService.exportLogs.mockRejectedValue(new Error('Export failed'));

      await expect(exportLogs()).rejects.toThrow('Export failed');

      const logs = getLogs();
      const errorLog = logs.find(
        log => log.message.includes('Failed to export logs') && log.level === 'error'
      );
      expect(errorLog).toBeDefined();
    });

    it('adds success message after export', async () => {
      mockSave.mockResolvedValue('/tmp/logs.json');
      mockTauriService.exportLogs.mockResolvedValue({
        success: true,
        data: { path: '/tmp/logs.json' },
      });

      await exportLogs();

      const logs = getLogs();
      const successLog = logs.find(
        log => log.message.includes('Logs exported successfully') && log.level === 'info'
      );
      expect(successLog).toBeDefined();
    });

    it('handles backend export failure', async () => {
      mockSave.mockResolvedValue('/tmp/logs.json');
      mockTauriService.exportLogs.mockResolvedValue({
        success: false,
        error: 'Permission denied',
      });

      await expect(exportLogs()).rejects.toThrow('Permission denied');
    });

    it('handles downloadDir failure gracefully', async () => {
      mockDownloadDir.mockRejectedValue(new Error('Path not available'));
      mockSave.mockResolvedValue('/tmp/logs.json');
      mockTauriService.exportLogs.mockResolvedValue({
        success: true,
        data: { path: '/tmp/logs.json' },
      });

      // Should not throw, should fallback to filename only
      const result = await exportLogs();
      expect(result.path).toBe('/tmp/logs.json');
    });
  });

  describe('Filter Setters', () => {
    it('updates level filter', () => {
      setLevelFilter('error');
      expect(getLevelFilter()).toBe('error');

      setLevelFilter('all');
      expect(getLevelFilter()).toBe('all');
    });

    it('updates device filter', () => {
      setDeviceFilter('ttl');
      expect(getDeviceFilter()).toBe('ttl');

      setDeviceFilter('all');
      expect(getDeviceFilter()).toBe('all');
    });

    it('updates search query', () => {
      setSearchQuery('test query');
      expect(getSearchQuery()).toBe('test query');

      setSearchQuery('');
      expect(getSearchQuery()).toBe('');
    });

    it('updates max logs limit', () => {
      setMaxLogs(500);
      expect(getMaxLogs()).toBe(500);

      setMaxLogs(2000);
      expect(getMaxLogs()).toBe(2000);
    });
  });

  describe('Cleanup', () => {
    it('stops listening on cleanup', async () => {
      await startListening();
      expect(getIsListening()).toBe(true);

      cleanup();
      expect(getIsListening()).toBe(false);
    });

    it('can be called multiple times safely', () => {
      expect(() => {
        cleanup();
        cleanup();
      }).not.toThrow();
    });
  });

  describe('Initialization', () => {
    it('does not auto-start listening', () => {
      // After clearLogs in beforeEach, listening should not be active
      // The store uses lazy initialization
      expect(getIsListening()).toBe(false);
    });

    it('starts listening when explicitly called', async () => {
      await startListening();
      expect(getIsListening()).toBe(true);
    });
  });

  describe('Performance', () => {
    beforeEach(() => {
      vi.useFakeTimers();
    });

    afterEach(() => {
      vi.useRealTimers();
    });

    it('handles large number of logs efficiently', () => {
      const startTime = performance.now();

      // Add many logs (reduced count due to test environment overhead)
      for (let i = 0; i < 200; i++) {
        log('info', `Performance test message ${i}`, i % 2 === 0 ? 'ttl' : 'kernel');
      }

      const endTime = performance.now();
      const duration = endTime - startTime;

      expect(duration).toBeLessThan(500); // Relaxed threshold for test environment
      expect(getLogs().length).toBeGreaterThanOrEqual(200);
    });

    it('filters large log sets efficiently', () => {
      // Add logs with different levels
      for (let i = 0; i < 400; i++) {
        const levels = ['debug', 'info', 'warn', 'error'];
        log(levels[i % 4], `Message ${i}`, 'ttl');
      }

      setLevelFilter('error');

      // Advance past the 100ms debounce
      vi.advanceTimersByTime(150);

      const filtered = getFilteredLogs();

      expect(filtered).toHaveLength(100); // 1/4 of logs are error level
    });

    it('maintains efficient search performance', () => {
      // Add many logs
      for (let i = 0; i < 200; i++) {
        log('info', i % 100 === 0 ? 'special message' : `regular message ${i}`, 'device');
      }

      setSearchQuery('special');

      // Advance past the 100ms debounce
      vi.advanceTimersByTime(150);

      const filtered = getFilteredLogs();

      expect(filtered).toHaveLength(2); // 2 special messages (0 and 100)
    });
  });

  describe('Edge Cases', () => {
    it('handles undefined log parameters gracefully', () => {
      expect(() => {
        log(undefined, undefined, undefined);
      }).not.toThrow();

      const logs = getLogs();
      expect(logs).toHaveLength(1);
      expect(logs[0].level).toBeUndefined();
      expect(logs[0].message).toBeUndefined();
      expect(logs[0].device).toBeNull();
    });

    it('handles empty strings in log parameters', () => {
      log('', '', '');

      const logs = getLogs();
      expect(logs).toHaveLength(1);
      expect(logs[0].level).toBe('');
      expect(logs[0].message).toBe('');
      expect(logs[0].device).toBe('');
    });

    it('handles very long log messages', () => {
      const longMessage = 'x'.repeat(10000);
      log('info', longMessage, 'ttl');

      const logs = getLogs();
      expect(logs[0].message).toBe(longMessage);
    });

    it('handles special characters in search queries', () => {
      log('info', 'Message with [special] (characters) and {braces}');

      setSearchQuery('[special]');
      let filtered = getFilteredLogs();
      expect(filtered).toHaveLength(1);

      setSearchQuery('(characters)');
      filtered = getFilteredLogs();
      expect(filtered).toHaveLength(1);

      setSearchQuery('{braces}');
      filtered = getFilteredLogs();
      expect(filtered).toHaveLength(1);
    });

    it('handles concurrent log additions', () => {
      // Simulate concurrent log additions
      const promises = Array.from({ length: 100 }, (_, i) => {
        return Promise.resolve().then(() => {
          log('info', `Concurrent message ${i}`, 'ttl');
        });
      });

      return Promise.all(promises).then(() => {
        expect(getLogs()).toHaveLength(100);
      });
    });
  });
});
