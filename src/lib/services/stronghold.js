import { Stronghold } from '@tauri-apps/plugin-stronghold';
import { appDataDir } from '@tauri-apps/api/path';

// Lazy-initialized singleton
let storePromise = null;
let strongholdInstance = null;

const VAULT_PASSWORD = 'hyperstudy-bridge-vault';
const CLIENT_NAME = 'hyperstudy-bridge';

async function getStore() {
  if (!storePromise) {
    storePromise = initStore();
  }
  return storePromise;
}

async function initStore() {
  const dataDir = await appDataDir();
  const vaultPath = `${dataDir}/vault.hold`;

  strongholdInstance = await Stronghold.load(vaultPath, VAULT_PASSWORD);

  let client;
  try {
    client = await strongholdInstance.loadClient(CLIENT_NAME);
  } catch {
    client = await strongholdInstance.createClient(CLIENT_NAME);
  }

  return client.getStore();
}

/**
 * Store a secret string in the encrypted vault.
 * @param {string} key - The key to store under
 * @param {string} value - The secret value to store
 */
let pendingWrite = Promise.resolve();
let dirty = false;

/** Persist one form's changed values in a single encrypted snapshot. */
export function setSecrets(values) {
  const update = pendingWrite.then(async () => {
    const store = await getStore();
    for (const [key, value] of Object.entries(values)) {
      const existing = await store.get(key);
      const previous = existing?.length ? new TextDecoder().decode(new Uint8Array(existing)) : '';
      if (previous === (value || '')) continue;
      // Retain dirty state on any failure; a later user save must commit again.
      dirty = true;
      if (value) await store.insert(key, Array.from(new TextEncoder().encode(value)));
      else await store.remove(key);
    }
    if (dirty) {
      await strongholdInstance.save();
      dirty = false;
    }
  });
  pendingWrite = update.catch(() => {}); // Serialize operations, never retry them.
  return update;
}

export function setSecret(key, value) {
  return setSecrets({ [key]: value });
}

/**
 * Retrieve a secret string from the encrypted vault.
 * @param {string} key - The key to retrieve
 * @returns {Promise<string|null>} The secret value, or null if not found
 */
export async function getSecret(key) {
  const store = await getStore();
  const data = await store.get(key);
  if (!data || data.length === 0) return null;
  return new TextDecoder().decode(new Uint8Array(data));
}

/**
 * Remove a secret from the encrypted vault.
 * @param {string} key - The key to remove
 */
export function removeSecret(key) {
  return setSecrets({ [key]: null });
}
