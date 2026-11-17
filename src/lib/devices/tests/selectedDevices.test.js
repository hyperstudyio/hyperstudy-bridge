import { describe, it, expect } from 'vitest';
import { DEVICE_CATALOG } from '../catalog.js';
import {
  saveSelectedDevices,
  restoreSelectedDevices,
  canDisconnectAll,
} from '../selectedDevices.js';

function memoryStorage(initial = {}) {
  const data = { ...initial };
  return {
    getItem: key => (key in data ? data[key] : null),
    setItem: (key, value) => {
      data[key] = String(value);
    },
    data,
  };
}

const catalogEntry = id => structuredClone(DEVICE_CATALOG.find(d => d.id === id));

// Bridge forgot every added device on quit, so each launch started from "No devices added yet".
describe('remembering added devices', () => {
  it('restores the devices added last time, with their settings', () => {
    const storage = memoryStorage();
    const frenz = catalogEntry('frenz');
    frenz.config.streamPoas = false;
    saveSelectedDevices([frenz, catalogEntry('ttl')], storage);

    const restored = restoreSelectedDevices(storage);
    expect(restored.map(d => d.id)).toEqual(['frenz', 'ttl']);
    expect(restored[0].name).toBe('Earable FRENZ Brainband');
    expect(restored[0].config.streamPoas).toBe(false);
    expect(restored[0].config.streamEegRaw).toBe(true);
  });

  it('saves only ids and settings, never runtime state', () => {
    const storage = memoryStorage();
    saveSelectedDevices(
      [{ ...catalogEntry('kernel'), status: 'connected', info: { ip: '10.0.0.2' } }],
      storage
    );
    const saved = JSON.parse(Object.values(storage.data)[0]);
    expect(saved).toEqual([{ id: 'kernel', config: catalogEntry('kernel').config }]);
  });

  it('drops a device this version no longer offers, and survives bad data', () => {
    const storage = memoryStorage();
    saveSelectedDevices(
      [{ id: 'eyelink', config: { ip: '100.1.1.1' } }, catalogEntry('pupil')],
      storage
    );
    expect(restoreSelectedDevices(storage).map(d => d.id)).toEqual(['pupil']);

    expect(
      restoreSelectedDevices(memoryStorage({ 'hyperstudy-bridge:devices': '{not json' }))
    ).toEqual([]);
    expect(
      restoreSelectedDevices(memoryStorage({ 'hyperstudy-bridge:devices': '{"id":"ttl"}' }))
    ).toEqual([]);
    expect(restoreSelectedDevices(memoryStorage())).toEqual([]);
  });

  it('starts empty when storage is unavailable', () => {
    const broken = {
      getItem: () => {
        throw new Error('blocked');
      },
      setItem: () => {
        throw new Error('blocked');
      },
    };
    expect(restoreSelectedDevices(broken)).toEqual([]);
    expect(() => saveSelectedDevices([catalogEntry('ttl')], broken)).not.toThrow();
  });
});

// Disconnect All was a solid red button, enabled even when nothing was connected.
describe('Disconnect All', () => {
  it('is available only while at least one device is connected', () => {
    expect(canDisconnectAll([])).toBe(false);
    expect(canDisconnectAll([{ id: 'ttl', status: 'disconnected' }])).toBe(false);
    expect(
      canDisconnectAll([
        { id: 'ttl', status: 'error' },
        { id: 'frenz', status: 'connected' },
      ])
    ).toBe(true);
  });
});
