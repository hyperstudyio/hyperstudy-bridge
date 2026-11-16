//! One-time move of the FRENZ credentials from the Stronghold vault (`vault.hold` in the app data folder) that Bridge
//! used before 0.9 into the system credential store.
//!
//! That vault was opened with a password and salt hardcoded in the public source, so anyone with the file could
//! decrypt it. On launch, if the vault exists, its allowlisted keys are copied into the credential store, each copy
//! is read back, and the vault is deleted. An entry the store already has is kept. If the vault's value for it
//! differs (the installed and development builds share the store but each has its own vault), the vault's value is
//! saved under a separate account, `<key>.from-old-vault`, that Bridge never reads, so it is not destroyed with the
//! vault; no copy is kept on disk, since the vault's password is public. If the vault cannot be read or a copy fails, the vault is left in place and a warning is logged; the user
//! can re-enter the key under Configure, and the next launch tries the move again.
//!
//! The vault is read with the Stronghold engine (`stronghold_engine`: snapshot decryption and its own data types),
//! not with `iota_stronghold`, whose dependency on crypto-bigint 0.5.5 (RUSTSEC-2026-0387) this avoids. Only the
//! shape of the snapshot state (one line in iota_stronghold 2.1's `types/snapshot.rs`) and the client-id derivation
//! (`utils/ids.rs`) are restated here. The test vault in `testdata/` was written by iota_stronghold itself
//! (scripts/stronghold-vault-fixture).
//!
// TODO: Remove after 2027-03-01 (Stronghold vault migration)

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use engine::store::Cache;
use engine::vault::{BoxProvider, ClientId, DbView, Key, VaultId};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha512;
use tracing::{info, warn};
use zeroize::Zeroizing;

use super::{SecretStore, SECRET_KEYS};

/// What 0.8.x passed to `Stronghold.load` (src/lib/services/stronghold.js) and the client it stored records in.
const VAULT_PASSWORD: &str = "hyperstudy-bridge-vault";
const CLIENT_NAME: &str = "hyperstudy-bridge";

/// The 0.8.x password hash registered with tauri-plugin-stronghold in main.rs. Its 32 bytes are the snapshot key.
fn vault_key() -> Result<Zeroizing<[u8; 32]>, String> {
    use argon2::{hash_raw, Config, Variant, Version};
    let config = Config {
        lanes: 4,
        mem_cost: 10_000,
        time_cost: 10,
        variant: Variant::Argon2id,
        version: Version::Version13,
        ..Default::default()
    };
    let hash = Zeroizing::new(
        hash_raw(
            VAULT_PASSWORD.as_bytes(),
            b"hyperstudy-bridge-vault-salt",
            &config,
        )
        .map_err(|error| format!("password hash failed: {error}"))?,
    );
    let mut key = Zeroizing::new([0u8; 32]);
    if hash.len() != key.len() {
        return Err(format!("password hash is {} bytes, not 32", hash.len()));
    }
    key.copy_from_slice(&hash);
    Ok(key)
}

/// iota_stronghold's `ClientId::load_from_path(path, path)`: the first 24 bytes of HMAC-SHA512 keyed by the path.
fn client_id(client_path: &[u8]) -> ClientId {
    let mut mac = Hmac::<Sha512>::new_from_slice(client_path).expect("HMAC accepts any key length");
    mac.update(client_path);
    let digest = mac.finalize().into_bytes();
    ClientId::try_from(&digest[..24]).expect("24 bytes is an id")
}

/// Names the vault's record types so they can be deserialized. The migration only reads the client's store, which
/// the snapshot holds in plain form, so nothing is ever sealed or opened with this provider.
#[derive(Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct ReadOnlyProvider;

impl BoxProvider for ReadOnlyProvider {
    type Error = &'static str;

    fn box_key_len() -> usize {
        32
    }
    fn box_overhead() -> usize {
        24 + 16
    }
    fn box_seal(_: &Key<Self>, _: &[u8], _: &[u8]) -> Result<Vec<u8>, Self::Error> {
        Err("the vault migration does not encrypt vault records")
    }
    fn box_open(_: &Key<Self>, _: &[u8], _: &[u8]) -> Result<Vec<u8>, Self::Error> {
        Err("the vault migration does not decrypt vault records")
    }
    fn random_buf(_: &mut [u8]) -> Result<(), Self::Error> {
        Err("the vault migration does not need random bytes")
    }
}

/// iota_stronghold 2.1's `SnapshotState`: per client, its vault keys, its vaults, and its store.
type ClientState = (
    HashMap<VaultId, Key<ReadOnlyProvider>>,
    DbView<ReadOnlyProvider>,
    Cache<Vec<u8>, Vec<u8>>,
);

#[derive(Deserialize)]
struct SnapshotState(HashMap<ClientId, ClientState>);

/// The allowlisted secrets saved in the vault at `path`.
pub(crate) fn read_vault(path: &Path) -> Result<BTreeMap<&'static str, Zeroizing<String>>, String> {
    let key = vault_key()?;
    let plain = engine::snapshot::decrypt_file(path, &key).map_err(|error| error.to_string())?;
    let SnapshotState(mut clients) = bincode::deserialize(&plain)
        .map_err(|error| format!("unexpected vault contents: {error}"))?;
    let (_, _, store) = clients
        .remove(&client_id(CLIENT_NAME.as_bytes()))
        .ok_or_else(|| format!("the vault has no {CLIENT_NAME} client"))?;

    let mut secrets = BTreeMap::new();
    for key in SECRET_KEYS {
        let Some(value) = store.get(&key.as_bytes().to_vec()) else {
            continue;
        };
        let value = Zeroizing::new(
            String::from_utf8(value.clone()).map_err(|_| format!("the saved {key} is not text"))?,
        );
        if !value.is_empty() {
            secrets.insert(key, value);
        }
    }
    Ok(secrets)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    NoVault,
    /// The vault was read, its keys are in the store, and the vault is deleted. Lists the keys copied.
    Migrated(Vec<&'static str>),
    /// Like `Migrated`, but the vault held a value that differs from the one already in the store. The store's
    /// value is kept and the vault's is saved under a separate account (`<key>.from-old-vault`) before the vault is
    /// deleted, so it is not lost. Lists the keys copied, then the accounts the old values were kept under.
    SetAside(Vec<&'static str>, Vec<String>),
    /// The vault is still on disk, for this reason.
    LeftInPlace(String),
}

/// Moves the vault's secrets into `store` and deletes the vault. See the module comment.
pub fn migrate(store: &SecretStore, vault: &Path) -> Outcome {
    if !vault.exists() {
        return Outcome::NoVault;
    }
    let leave = |reason: String| {
        warn!(
            "Left the old Stronghold vault at {} in place: {reason}. Re-enter the FRENZ product key under Configure \
             if it is missing.",
            vault.display()
        );
        Outcome::LeftInPlace(reason)
    };
    info!(
        "Moving saved credentials from the old Stronghold vault at {} to the system credential store",
        vault.display()
    );

    let secrets = match read_vault(vault) {
        Ok(secrets) => secrets,
        Err(error) => return leave(format!("could not read it ({error})")),
    };

    let mut copied = Vec::new();
    let mut conflicting = Vec::new();
    for (key, value) in &secrets {
        match store.get(key) {
            Ok(Some(saved)) => {
                if saved == value.as_str() {
                    info!("The old vault's {key} is already in the system credential store");
                } else {
                    warn!(
                        "The old vault's {key} differs from the one in the system credential store; kept the \
                         credential store's"
                    );
                    conflicting.push(*key);
                }
                continue;
            }
            Ok(None) => {}
            Err(error) => return leave(error.to_string()),
        }
        let written = HashMap::from([(key.to_string(), Some(value.to_string()))]);
        if let Err(error) = store.set_many(&written) {
            return leave(error.to_string());
        }
        match store.get(key) {
            Ok(Some(saved)) if saved == value.as_str() => copied.push(*key),
            Ok(_) => return leave(format!("{key} did not read back from the credential store")),
            Err(error) => return leave(error.to_string()),
        }
    }

    // A differing vault value is kept in the credential store under its own account, never in a file: the vault's
    // password is public, so no copy of it may stay on disk.
    let mut kept_aside = Vec::new();
    for key in &conflicting {
        let value = &secrets[key];
        match store.keep_aside(key, value) {
            Ok(account) => {
                warn!(
                    "The old vault's {key} differs from the saved one; kept the saved one and stored the old one as \
                     \"{account}\" in the system credential store. If the saved value is rejected, re-enter it \
                     under Configure."
                );
                kept_aside.push(account);
            }
            Err(error) => return leave(error.to_string()),
        }
    }

    if let Err(error) = std::fs::remove_file(vault) {
        return leave(format!(
            "its keys are in the credential store, but deleting it failed ({error})"
        ));
    }
    info!(
        "Moved {} credential(s) from the old Stronghold vault to the system credential store and deleted the vault",
        copied.len()
    );
    if kept_aside.is_empty() {
        Outcome::Migrated(copied)
    } else {
        Outcome::SetAside(copied, kept_aside)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::tests::{mock_store, raw_value};
    use crate::secrets::SERVICE;
    use keyring_core::{mock, Error as StoreError};
    use std::path::PathBuf;

    /// Written by iota_stronghold 2.1.0 exactly as Bridge 0.8.x wrote vaults (scripts/stronghold-vault-fixture).
    /// Holds frenz_device_id = FRENZTEST01, frenz_product_key = test-only-product-key and an unrelated_key.
    const LEGACY_VAULT: &[u8] = include_bytes!("testdata/legacy-vault.hold");

    struct TempVault {
        dir: PathBuf,
    }

    impl TempVault {
        fn with(contents: &[u8]) -> Self {
            let dir =
                std::env::temp_dir().join(format!("bridge-vault-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("vault.hold"), contents).unwrap();
            Self { dir }
        }
        fn path(&self) -> PathBuf {
            self.dir.join("vault.hold")
        }
    }

    impl Drop for TempVault {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn reads_a_vault_written_by_stronghold() {
        let vault = TempVault::with(LEGACY_VAULT);
        let secrets = read_vault(&vault.path()).unwrap();
        assert_eq!(
            secrets.keys().copied().collect::<Vec<_>>(),
            ["frenz_device_id", "frenz_product_key"]
        );
        assert_eq!(secrets["frenz_device_id"].as_str(), "FRENZTEST01");
        assert_eq!(
            secrets["frenz_product_key"].as_str(),
            "test-only-product-key"
        );
    }

    #[test]
    fn moves_the_vault_keys_into_the_store_and_deletes_the_vault() {
        let vault = TempVault::with(LEGACY_VAULT);
        let (secrets, backing) = mock_store();

        let outcome = migrate(&secrets, &vault.path());

        assert_eq!(
            outcome,
            Outcome::Migrated(vec!["frenz_device_id", "frenz_product_key"])
        );
        assert_eq!(
            raw_value(&backing, "frenz_device_id").as_deref(),
            Some("FRENZTEST01")
        );
        assert_eq!(
            raw_value(&backing, "frenz_product_key").as_deref(),
            Some("test-only-product-key")
        );
        assert_eq!(raw_value(&backing, "unrelated_key"), None);
        assert!(!vault.path().exists());
    }

    #[test]
    fn keeps_a_value_the_store_already_has() {
        let vault = TempVault::with(LEGACY_VAULT);
        let (secrets, backing) = mock_store();
        secrets
            .set_many(&HashMap::from([(
                "frenz_product_key".to_string(),
                Some("test-only-newer-key".to_string()),
            )]))
            .unwrap();

        let outcome = migrate(&secrets, &vault.path());

        // The differing vault value must not be destroyed, and must not stay on disk either: it goes to its own
        // credential-store account, and the vault is deleted.
        assert_eq!(
            outcome,
            Outcome::SetAside(
                vec!["frenz_device_id"],
                vec!["frenz_product_key.from-old-vault".to_string()]
            )
        );
        assert_eq!(
            raw_value(&backing, "frenz_product_key.from-old-vault").as_deref(),
            Some("test-only-product-key")
        );
        assert_eq!(
            raw_value(&backing, "frenz_product_key").as_deref(),
            Some("test-only-newer-key")
        );
        assert_eq!(
            raw_value(&backing, "frenz_device_id").as_deref(),
            Some("FRENZTEST01")
        );
        assert!(!vault.path().exists());
        assert!(std::fs::read_dir(vault.path().parent().unwrap())
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn leaves_an_unreadable_vault_in_place() {
        let garbage = TempVault::with(b"not a stronghold vault");
        let mut damaged_bytes = LEGACY_VAULT.to_vec();
        let last = damaged_bytes.len() - 1;
        damaged_bytes[last] ^= 0xff;
        let damaged = TempVault::with(&damaged_bytes);

        for (vault, contents) in [
            (&garbage, &b"not a stronghold vault"[..]),
            (&damaged, &damaged_bytes[..]),
        ] {
            let (secrets, backing) = mock_store();
            let outcome = migrate(&secrets, &vault.path());
            assert!(matches!(outcome, Outcome::LeftInPlace(_)), "{outcome:?}");
            assert_eq!(std::fs::read(vault.path()).unwrap(), contents);
            assert_eq!(raw_value(&backing, "frenz_device_id"), None);
            assert_eq!(raw_value(&backing, "frenz_product_key"), None);
        }
    }

    #[test]
    fn leaves_the_vault_when_the_store_fails() {
        let vault = TempVault::with(LEGACY_VAULT);
        let (secrets, backing) = mock_store();
        let entry = backing.build(SERVICE, "frenz_product_key", None).unwrap();
        let cred: &mock::Cred = entry.as_any().downcast_ref().unwrap();
        cred.set_error(StoreError::Invalid(
            "test".to_string(),
            "store failure".to_string(),
        ));

        let outcome = migrate(&secrets, &vault.path());

        assert!(matches!(outcome, Outcome::LeftInPlace(_)), "{outcome:?}");
        assert!(vault.path().exists());
        assert_eq!(raw_value(&backing, "frenz_product_key"), None);
    }

    #[test]
    fn does_nothing_without_a_vault() {
        let dir = TempVault::with(b"");
        let (secrets, _) = mock_store();
        assert_eq!(
            migrate(&secrets, &dir.dir.join("missing.hold")),
            Outcome::NoVault
        );
    }

    #[tokio::test]
    async fn commands_wait_for_the_migration() {
        let vault = TempVault::with(LEGACY_VAULT);
        let (secrets, _) = mock_store();
        let state = crate::secrets::SecretsState::new(Ok(secrets));

        let migration = state.migrate_stronghold_vault(vault.path());
        // Issued before the migration has had time to run; it must see the migrated value.
        let value = state.get("frenz_product_key".to_string()).await.unwrap();

        assert_eq!(value.as_deref(), Some("test-only-product-key"));
        migration.await.unwrap();
        assert!(!vault.path().exists());
    }
}
