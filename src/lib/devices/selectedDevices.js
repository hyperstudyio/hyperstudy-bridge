import { DEVICE_CATALOG } from './catalog.js';

// The devices on Bridge's main screen, remembered across launches. Only ids and settings are kept: secrets (FRENZ
// band and product key) live in the vault, and status comes from the bridge at run time.
const STORAGE_KEY = 'hyperstudy-bridge:devices';

/** Remember the devices on the main screen. Storage can be unavailable; the list then lasts until quit. */
export function saveSelectedDevices(devices, storage = globalThis.localStorage) {
  const saved = devices.map(({ id, config }) => ({ id, config }));
  try {
    storage.setItem(STORAGE_KEY, JSON.stringify(saved));
  } catch {
    // Not persisted; nothing else depends on it.
  }
}

/**
 * The devices added last time, rebuilt from this version's catalog with their saved settings on top. A device this
 * version no longer offers (for example EyeLink) is left out; unreadable data restores nothing.
 */
export function restoreSelectedDevices(
  storage = globalThis.localStorage,
  catalog = DEVICE_CATALOG
) {
  let saved;
  try {
    saved = JSON.parse(storage.getItem(STORAGE_KEY) ?? '[]');
  } catch {
    return [];
  }
  if (!Array.isArray(saved)) return [];
  return saved.flatMap(entry => {
    const offered = catalog.find(device => device.id === entry?.id);
    if (!offered) return [];
    const device = structuredClone(offered);
    device.config = { ...device.config, ...(entry.config ?? {}) };
    return [device];
  });
}

/** Disconnect All acts on connected devices only, so it is offered only while one is connected. */
export function canDisconnectAll(devices) {
  return devices.some(device => device.status === 'connected');
}
