import { invoke } from '@tauri-apps/api/core';

// Saved device credentials (the FRENZ band ID and product key). The Rust side keeps them in the operating system's
// credential store (macOS Keychain, Windows Credential Manager, Secret Service on Linux) and accepts only the keys
// it knows; see src-tauri/src/secrets. Installed and dev builds share the same saved values.

let pendingWrite = Promise.resolve();

/**
 * Save one form's secrets in one call. A null or empty value removes that secret; values equal to what is saved
 * are not written again. Calls run one after another, in the order they were made.
 * @param {Record<string, string|null>} values
 * @returns {Promise<string[]>} The keys that changed.
 */
export function setSecrets(values) {
  const update = pendingWrite.then(() => invoke('set_secrets', { values }));
  pendingWrite = update.catch(() => {}); // Keep the order after a failure; never retry.
  return update;
}

export function setSecret(key, value) {
  return setSecrets({ [key]: value });
}

export function removeSecret(key) {
  return setSecrets({ [key]: null });
}

/**
 * The saved value for `key`, or null when there is none.
 * @param {string} key
 * @returns {Promise<string|null>}
 */
export async function getSecret(key) {
  return (await invoke('get_secret', { key })) || null;
}
