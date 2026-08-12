//! One compatibility-window importer from legacy SQLite credentials.

use crate::db::Database;
use crate::secrets::CredentialStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationCheckpoint {
    BeforeWrite,
    AfterWrite,
    AfterReadback,
    BeforeLegacyDelete,
    BeforeLegacyDrop,
}

pub fn migrate_legacy_credentials(
    database: &Database,
    store: &dyn CredentialStore,
    mut checkpoint: impl FnMut(MigrationCheckpoint) -> anyhow::Result<()>,
) -> anyhow::Result<usize> {
    let legacy_table_exists = database.legacy_api_key_table_exists()?;
    let legacy_credentials = database.legacy_api_keys()?;
    for (provider_id, legacy_secret) in &legacy_credentials {
        checkpoint(MigrationCheckpoint::BeforeWrite)?;
        match store.read(provider_id)? {
            Some(existing) if existing != legacy_secret.as_bytes() => {
                anyhow::bail!(
                    "legacy credential for {provider_id} conflicts with the OS credential store"
                );
            }
            Some(_) => {}
            None => store.write(provider_id, legacy_secret.as_bytes())?,
        }
        checkpoint(MigrationCheckpoint::AfterWrite)?;

        let readback = store.read(provider_id)?.ok_or_else(|| {
            anyhow::anyhow!("OS credential readback was absent for {provider_id}")
        })?;
        if readback != legacy_secret.as_bytes() {
            anyhow::bail!("OS credential readback mismatch for {provider_id}");
        }
        checkpoint(MigrationCheckpoint::AfterReadback)?;
        checkpoint(MigrationCheckpoint::BeforeLegacyDelete)?;
    }
    if legacy_table_exists {
        checkpoint(MigrationCheckpoint::BeforeLegacyDrop)?;
        if !database.retire_legacy_api_keys_if_exact(&legacy_credentials)? {
            anyhow::bail!("legacy credentials changed during migration");
        }
    }
    Ok(legacy_credentials.len())
}

#[cfg(test)]
mod tests {
    use super::{MigrationCheckpoint, migrate_legacy_credentials};
    use crate::db::Database;
    use crate::secrets::{CredentialStore, SecretStoreError};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct FakeStore {
        values: Mutex<BTreeMap<String, Vec<u8>>>,
        corrupt_readback: bool,
    }

    impl CredentialStore for FakeStore {
        fn write(&self, provider_id: &str, secret: &[u8]) -> Result<(), SecretStoreError> {
            self.values
                .lock()
                .expect("fake store")
                .insert(provider_id.to_string(), secret.to_vec());
            Ok(())
        }

        fn read(&self, provider_id: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
            let value = self
                .values
                .lock()
                .expect("fake store")
                .get(provider_id)
                .cloned();
            Ok(value.map(|mut secret| {
                if self.corrupt_readback {
                    secret.push(b'!');
                }
                secret
            }))
        }

        fn delete(&self, provider_id: &str) -> Result<bool, SecretStoreError> {
            Ok(self
                .values
                .lock()
                .expect("fake store")
                .remove(provider_id)
                .is_some())
        }
    }

    #[test]
    fn migration_writes_reads_back_exact_bytes_then_deletes_plaintext() {
        let database = test_database("success");
        database
            .seed_legacy_api_key("anthropic", "exact-fixture-secret")
            .expect("seed legacy credential");
        let store = Arc::new(FakeStore::default());

        let migrated = migrate_legacy_credentials(&database, store.as_ref(), |_| Ok(()))
            .expect("migrate credential");

        assert_eq!(migrated, 1);
        assert_eq!(
            store.read("anthropic").expect("read store"),
            Some(b"exact-fixture-secret".to_vec())
        );
        assert!(
            !database
                .legacy_api_key_table_exists()
                .expect("legacy table state")
        );
        assert!(database.legacy_api_keys().expect("legacy rows").is_empty());
    }

    #[test]
    fn every_crash_boundary_preserves_the_current_plaintext_row() {
        for boundary in [
            MigrationCheckpoint::BeforeWrite,
            MigrationCheckpoint::AfterWrite,
            MigrationCheckpoint::AfterReadback,
            MigrationCheckpoint::BeforeLegacyDelete,
            MigrationCheckpoint::BeforeLegacyDrop,
        ] {
            let database = test_database(&format!("crash-{boundary:?}"));
            database
                .seed_legacy_api_key("anthropic", "exact-fixture-secret")
                .expect("seed legacy credential");
            let store = FakeStore::default();

            let error = migrate_legacy_credentials(&database, &store, |checkpoint| {
                if checkpoint == boundary {
                    anyhow::bail!("simulated crash at {checkpoint:?}");
                }
                Ok(())
            })
            .expect_err("checkpoint must interrupt migration");

            assert!(error.to_string().contains("simulated crash"));
            assert_eq!(
                database.legacy_api_keys().expect("legacy rows"),
                vec![("anthropic".to_string(), "exact-fixture-secret".to_string())]
            );
            assert!(
                database
                    .legacy_api_key_table_exists()
                    .expect("legacy table state")
            );
        }
    }

    #[test]
    fn mismatched_readback_never_deletes_plaintext() {
        let database = test_database("mismatch");
        database
            .seed_legacy_api_key("anthropic", "exact-fixture-secret")
            .expect("seed legacy credential");
        let store = FakeStore {
            corrupt_readback: true,
            ..FakeStore::default()
        };

        let error = migrate_legacy_credentials(&database, &store, |_| Ok(()))
            .expect_err("mismatched readback must fail");

        assert!(error.to_string().contains("readback mismatch"));
        assert_eq!(database.legacy_api_keys().expect("legacy rows").len(), 1);
        assert!(
            database
                .legacy_api_key_table_exists()
                .expect("legacy table state")
        );
    }

    #[test]
    fn a_different_existing_os_secret_is_never_overwritten() {
        let database = test_database("conflict");
        database
            .seed_legacy_api_key("anthropic", "stale-legacy-secret")
            .expect("seed legacy credential");
        let store = FakeStore::default();
        store
            .write("anthropic", b"newer-os-secret")
            .expect("seed OS secret");

        let error = migrate_legacy_credentials(&database, &store, |_| Ok(()))
            .expect_err("conflicting authorities must fail closed");

        assert!(error.to_string().contains("conflicts"));
        assert_eq!(
            store.read("anthropic").expect("read store"),
            Some(b"newer-os-secret".to_vec())
        );
        assert_eq!(database.legacy_api_keys().expect("legacy rows").len(), 1);
        assert!(
            database
                .legacy_api_key_table_exists()
                .expect("legacy table state")
        );
    }

    #[test]
    fn a_later_failure_retains_every_plaintext_row_for_safe_retry() {
        let database = test_database("multi-row-failure");
        database
            .seed_legacy_api_key("anthropic", "first-secret")
            .expect("seed first credential");
        database
            .seed_legacy_api_key("gemini", "second-secret")
            .expect("seed second credential");
        let store = FakeStore::default();
        store
            .write("gemini", b"conflicting-secret")
            .expect("seed conflict");

        let error = migrate_legacy_credentials(&database, &store, |_| Ok(()))
            .expect_err("second-row conflict must fail the batch");

        assert!(error.to_string().contains("conflicts"));
        assert_eq!(
            database.legacy_api_keys().expect("legacy rows"),
            vec![
                ("anthropic".to_string(), "first-secret".to_string()),
                ("gemini".to_string(), "second-secret".to_string()),
            ]
        );
        assert!(
            database
                .legacy_api_key_table_exists()
                .expect("legacy table state")
        );
    }

    #[test]
    fn fresh_database_is_a_noop_without_creating_plaintext_storage() {
        let database = test_database("fresh-noop");
        let store = FakeStore::default();

        assert_eq!(
            migrate_legacy_credentials(&database, &store, |_| Ok(())).expect("no-op migration"),
            0
        );
        assert!(
            !database
                .legacy_api_key_table_exists()
                .expect("legacy table state")
        );
    }

    #[test]
    fn empty_preexisting_plaintext_table_is_retired() {
        let database = test_database("empty-retirement");
        database
            .create_legacy_api_key_table()
            .expect("create legacy table fixture");
        let store = FakeStore::default();

        assert_eq!(
            migrate_legacy_credentials(&database, &store, |_| Ok(())).expect("retire empty table"),
            0
        );
        assert!(
            !database
                .legacy_api_key_table_exists()
                .expect("legacy table state")
        );
    }

    fn test_database(label: &str) -> Database {
        let path = std::env::temp_dir().join(format!(
            "free-token-energy-secret-migration-{label}-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
        ));
        Database::new(path).expect("test database")
    }
}
