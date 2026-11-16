//! Writes a Stronghold vault the way Bridge 0.8.x did, holding test-only values, for the migration tests.
//!
//! The steps are the ones the app took: `Stronghold.load(vault, "hyperstudy-bridge-vault")` in
//! src/lib/services/stronghold.js, hashed by the argon2 function registered in src-tauri/src/main.rs, then
//! tauri-plugin-stronghold 2.3.2's `Stronghold::new`, `create_client("hyperstudy-bridge")`, `save_store_record` and
//! `save` (commit_with_keyprovider).
//!
//! TODO: Remove after 2027-03-01 (Stronghold vault migration)

use std::path::PathBuf;

use iota_stronghold::{KeyProvider, SnapshotPath, Stronghold};
use zeroize::Zeroizing;

/// The password and client name hardcoded in the 0.8.x frontend.
const VAULT_PASSWORD: &str = "hyperstudy-bridge-vault";
const CLIENT_NAME: &str = "hyperstudy-bridge";

/// The 0.8.x main.rs password hash, verbatim.
fn hash_password(password: &str) -> Vec<u8> {
    use argon2::{hash_raw, Config, Variant, Version};
    let config = Config {
        lanes: 4,
        mem_cost: 10_000,
        time_cost: 10,
        variant: Variant::Argon2id,
        version: Version::Version13,
        ..Default::default()
    };
    let salt = b"hyperstudy-bridge-vault-salt";
    hash_raw(password.as_ref(), salt, &config)
        .expect("failed to hash password")
        .to_vec()
}

fn main() {
    let path: PathBuf = std::env::args_os()
        .nth(1)
        .expect("usage: stronghold-vault-fixture <output vault path>")
        .into();
    if path.exists() {
        std::fs::remove_file(&path).expect("remove the old fixture");
    }

    let keyprovider =
        KeyProvider::try_from(Zeroizing::new(hash_password(VAULT_PASSWORD))).expect("key provider");
    let snapshot = SnapshotPath::from_path(&path);
    let stronghold = Stronghold::default();
    let client = stronghold
        .create_client(CLIENT_NAME)
        .expect("create client");
    let store = client.store();
    // Test-only values. `unrelated_key` checks that the migration copies only the keys it knows.
    for (key, value) in [
        ("frenz_device_id", "FRENZTEST01"),
        ("frenz_product_key", "test-only-product-key"),
        ("unrelated_key", "test-only-unrelated"),
    ] {
        store
            .insert(key.as_bytes().to_vec(), value.as_bytes().to_vec(), None)
            .expect("insert");
    }
    stronghold
        .commit_with_keyprovider(&snapshot, &keyprovider)
        .expect("save vault");
    println!("wrote {}", path.display());
}
