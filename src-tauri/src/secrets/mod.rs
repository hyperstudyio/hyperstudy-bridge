//! Saved device credentials (the FRENZ band ID and product key), kept in the operating system's credential store:
//! the macOS Keychain, the Windows Credential Manager, or the Secret Service (GNOME Keyring, KWallet) on Linux.
//!
//! Every entry has the service name [`SERVICE`] and the secret's key (for example `frenz_product_key`) as its account.
//! The service name does not depend on the app identifier, so the installed app and a dev build
//! (`com.hyperstudy.bridge.recording-dev`) read and write the same saved key.
//!
//! Only the keys in [`SECRET_KEYS`] can be read or written. Values never go into the log.

pub mod stronghold_migration;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use keyring_core::{CredentialStore, Entry, Error as StoreError};
use tauri::State;
use tokio::sync::Mutex;
use tracing::{info, warn};

/// The credential-store service name shared by every Bridge build.
pub const SERVICE: &str = "io.hyperstudy.bridge";

/// The secrets the app keeps. Anything else is refused.
pub const SECRET_KEYS: [&str; 2] = ["frenz_device_id", "frenz_product_key"];

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("Unknown secret key: {0}")]
    UnknownKey(String),
    /// The message is the store's own error text, which never contains the secret.
    #[error("Credential store error for {key}: {message}")]
    Store { key: &'static str, message: String },
}

/// The allowlisted key that equals `key`.
fn allowed_key(key: &str) -> Result<&'static str, SecretError> {
    SECRET_KEYS
        .iter()
        .copied()
        .find(|allowed| *allowed == key)
        .ok_or_else(|| SecretError::UnknownKey(key.to_string()))
}

/// The allowlisted secrets in one credential store. Calls block (the OS store may show an access prompt).
pub struct SecretStore {
    store: Arc<CredentialStore>,
}

impl SecretStore {
    /// The platform's credential store, as chosen by the `keyring` crate.
    pub fn platform() -> Result<Self, String> {
        keyring::Entry::store_status()
            .as_ref()
            .map_err(|error| error.to_string())?;
        keyring_core::get_default_store()
            .map(Self::with_store)
            .ok_or_else(|| "no credential store is available on this system".to_string())
    }

    pub fn with_store(store: Arc<CredentialStore>) -> Self {
        Self { store }
    }

    fn entry(&self, key: &'static str) -> Result<Entry, SecretError> {
        self.store
            .build(SERVICE, key, None)
            .map_err(|error| store_error(key, error))
    }

    /// The saved value, or `None` when there is none.
    pub fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        let key = allowed_key(key)?;
        match self.entry(key)?.get_password() {
            Ok(value) if value.is_empty() => Ok(None),
            Ok(value) => Ok(Some(value)),
            Err(StoreError::NoEntry) => Ok(None),
            Err(error) => Err(store_error(key, error)),
        }
    }

    /// Saves each value, or removes the entry when the value is `None` or empty. Values equal to what is saved are
    /// not written again. Every key is checked before anything is written. Returns the keys that changed.
    pub fn set_many(
        &self,
        values: &HashMap<String, Option<String>>,
    ) -> Result<Vec<&'static str>, SecretError> {
        let mut checked = BTreeMap::new();
        for (key, value) in values {
            let value = value.as_deref().filter(|value| !value.is_empty());
            checked.insert(allowed_key(key)?, value);
        }

        let mut changed = Vec::new();
        for (key, value) in checked {
            if self.get(key)?.as_deref() == value {
                continue;
            }
            let entry = self.entry(key)?;
            match value {
                Some(value) => entry
                    .set_password(value)
                    .map_err(|error| store_error(key, error))?,
                None => match entry.delete_credential() {
                    Ok(()) | Err(StoreError::NoEntry) => {}
                    Err(error) => return Err(store_error(key, error)),
                },
            }
            info!(
                "{} saved credential {key}",
                if value.is_some() {
                    "Updated"
                } else {
                    "Removed"
                }
            );
            changed.push(key);
        }
        Ok(changed)
    }
}

fn store_error(key: &'static str, error: StoreError) -> SecretError {
    SecretError::Store {
        key,
        message: error.to_string(),
    }
}

/// The app's secret store behind one lock. Calls are serialized, and the one-time vault migration holds the lock
/// from app setup until it finishes, so no command reads the store before the migrated values are in it.
#[derive(Clone)]
pub struct SecretsState {
    store: Arc<Mutex<Result<SecretStore, String>>>,
}

impl SecretsState {
    /// `store` is the error text when the platform store could not be opened; every call then reports it.
    pub fn new(store: Result<SecretStore, String>) -> Self {
        if let Err(error) = &store {
            warn!("System credential store unavailable; saved device keys cannot be read or saved: {error}");
        }
        Self {
            store: Arc::new(Mutex::new(store)),
        }
    }

    async fn run<T: Send + 'static>(
        &self,
        call: impl FnOnce(&SecretStore) -> Result<T, SecretError> + Send + 'static,
    ) -> Result<T, String> {
        let guard = self.store.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || match &*guard {
            Ok(store) => call(store).map_err(|error| error.to_string()),
            Err(error) => Err(format!(
                "The system credential store is unavailable: {error}"
            )),
        })
        .await
        .map_err(|error| format!("Credential store task failed: {error}"))?
    }

    pub async fn get(&self, key: String) -> Result<Option<String>, String> {
        self.run(move |store| store.get(&key)).await
    }

    pub async fn set_many(
        &self,
        values: HashMap<String, Option<String>>,
    ) -> Result<Vec<&'static str>, String> {
        self.run(move |store| store.set_many(&values)).await
    }

    /// Moves the keys of a pre-0.9 Stronghold vault into the store, in the background. Takes the lock before
    /// returning, so commands issued after this call wait for the migration.
    // TODO: Remove after 2027-03-01 (Stronghold vault migration)
    pub fn migrate_stronghold_vault(&self, vault: PathBuf) -> tokio::task::JoinHandle<()> {
        let guard = self.store.clone().try_lock_owned();
        tokio::task::spawn_blocking(move || {
            let Ok(guard) = guard else {
                warn!("Skipped the Stronghold vault migration: the credential store was in use");
                return;
            };
            match &*guard {
                Ok(store) => {
                    stronghold_migration::migrate(store, &vault);
                }
                Err(_) if vault.exists() => warn!(
                    "Left the Stronghold vault at {} in place: the system credential store is unavailable",
                    vault.display()
                ),
                Err(_) => {}
            }
        })
    }
}

/// The saved value for `key`, or `null`.
#[tauri::command]
pub async fn get_secret(
    state: State<'_, SecretsState>,
    key: String,
) -> Result<Option<String>, String> {
    state.get(key).await
}

/// Saves several secrets at once; a `null` or empty value removes that secret. Returns the keys that changed.
#[tauri::command]
pub async fn set_secrets(
    state: State<'_, SecretsState>,
    values: HashMap<String, Option<String>>,
) -> Result<Vec<&'static str>, String> {
    state.set_many(values).await
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use keyring_core::api::{CredentialApi, CredentialStoreApi};
    use keyring_core::{mock, Credential};
    use std::any::Any;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A store backed by the keyring mock (in memory, never the real keychain).
    pub(crate) fn mock_store() -> (SecretStore, Arc<CredentialStore>) {
        let store: Arc<CredentialStore> = mock::Store::new().unwrap();
        (SecretStore::with_store(store.clone()), store)
    }

    /// Reads an entry straight from the backing store, bypassing the allowlist.
    pub(crate) fn raw_value(store: &Arc<CredentialStore>, account: &str) -> Option<String> {
        match store.build(SERVICE, account, None).unwrap().get_password() {
            Ok(value) => Some(value),
            Err(StoreError::NoEntry) => None,
            Err(error) => panic!("mock store error: {error}"),
        }
    }

    fn values(pairs: &[(&str, Option<&str>)]) -> HashMap<String, Option<String>> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.map(str::to_string)))
            .collect()
    }

    /// Wraps the mock store and counts writes (sets and deletes) that reach it.
    struct CountingStore {
        inner: Arc<CredentialStore>,
        writes: Arc<AtomicUsize>,
    }

    struct CountingCred {
        inner: Entry,
        writes: Arc<AtomicUsize>,
    }

    impl CredentialApi for CountingCred {
        fn set_secret(&self, secret: &[u8]) -> keyring_core::Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.inner.set_secret(secret)
        }
        fn get_secret(&self) -> keyring_core::Result<Vec<u8>> {
            self.inner.get_secret()
        }
        fn delete_credential(&self) -> keyring_core::Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.inner.delete_credential()
        }
        fn get_credential(&self) -> keyring_core::Result<Option<Arc<Credential>>> {
            Ok(None)
        }
        fn get_specifiers(&self) -> Option<(String, String)> {
            self.inner.get_specifiers()
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    impl CredentialStoreApi for CountingStore {
        fn vendor(&self) -> String {
            "counting test store".to_string()
        }
        fn id(&self) -> String {
            "counting".to_string()
        }
        fn build(
            &self,
            service: &str,
            user: &str,
            mods: Option<&HashMap<&str, &str>>,
        ) -> keyring_core::Result<Entry> {
            let inner = self.inner.build(service, user, mods)?;
            Ok(Entry::new_with_credential(Arc::new(CountingCred {
                inner,
                writes: self.writes.clone(),
            })))
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[test]
    fn refuses_keys_outside_the_allowlist() {
        let (secrets, backing) = mock_store();
        assert!(matches!(
            secrets.get("unrelated_key"),
            Err(SecretError::UnknownKey(_))
        ));

        // One unknown key rejects the whole call, before any write.
        let result = secrets.set_many(&values(&[
            ("frenz_device_id", Some("FRENZTEST01")),
            ("unrelated_key", Some("test-only")),
        ]));
        assert!(matches!(result, Err(SecretError::UnknownKey(key)) if key == "unrelated_key"));
        assert_eq!(raw_value(&backing, "frenz_device_id"), None);
        assert_eq!(raw_value(&backing, "unrelated_key"), None);
    }

    #[test]
    fn saves_reads_and_removes_secrets() {
        let (secrets, backing) = mock_store();
        assert_eq!(secrets.get("frenz_product_key").unwrap(), None);

        let changed = secrets
            .set_many(&values(&[
                ("frenz_device_id", Some("FRENZTEST01")),
                ("frenz_product_key", Some("test-only-key")),
            ]))
            .unwrap();
        assert_eq!(changed, ["frenz_device_id", "frenz_product_key"]);
        assert_eq!(
            secrets.get("frenz_device_id").unwrap().as_deref(),
            Some("FRENZTEST01")
        );
        assert_eq!(
            raw_value(&backing, "frenz_product_key").as_deref(),
            Some("test-only-key")
        );

        // null removes, and so does an empty string.
        secrets
            .set_many(&values(&[("frenz_product_key", None)]))
            .unwrap();
        assert_eq!(secrets.get("frenz_product_key").unwrap(), None);
        secrets
            .set_many(&values(&[("frenz_device_id", Some(""))]))
            .unwrap();
        assert_eq!(raw_value(&backing, "frenz_device_id"), None);

        // Removing what is not there is not an error.
        assert!(secrets
            .set_many(&values(&[("frenz_device_id", None)]))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn skips_values_that_are_already_saved() {
        let writes = Arc::new(AtomicUsize::new(0));
        let secrets = SecretStore::with_store(Arc::new(CountingStore {
            inner: mock::Store::new().unwrap(),
            writes: writes.clone(),
        }));
        let form = values(&[
            ("frenz_device_id", Some("FRENZTEST01")),
            ("frenz_product_key", Some("test-only-key")),
        ]);

        secrets.set_many(&form).unwrap();
        assert_eq!(writes.load(Ordering::SeqCst), 2);
        assert!(secrets.set_many(&form).unwrap().is_empty());
        assert_eq!(writes.load(Ordering::SeqCst), 2);

        let changed = secrets
            .set_many(&values(&[
                ("frenz_device_id", Some("FRENZTEST01")),
                ("frenz_product_key", Some("test-only-other")),
            ]))
            .unwrap();
        assert_eq!(changed, ["frenz_product_key"]);
        assert_eq!(writes.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn store_errors_name_the_key_but_not_the_value() {
        let (secrets, backing) = mock_store();
        let entry = backing.build(SERVICE, "frenz_product_key", None).unwrap();
        let mock: &mock::Cred = entry.as_any().downcast_ref().unwrap();
        mock.set_error(StoreError::Invalid(
            "test".to_string(),
            "store failure".to_string(),
        ));

        let error = secrets
            .set_many(&values(&[("frenz_product_key", Some("test-only-key"))]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("frenz_product_key"), "{error}");
        assert!(!error.contains("test-only-key"), "{error}");
    }

    #[tokio::test]
    async fn commands_report_an_unavailable_store() {
        let state = SecretsState::new(Err("no secret service".to_string()));
        let error = state
            .get("frenz_product_key".to_string())
            .await
            .unwrap_err();
        assert!(error.contains("no secret service"), "{error}");
    }
}
