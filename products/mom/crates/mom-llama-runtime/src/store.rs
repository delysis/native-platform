use anyhow::{Context, Result, anyhow};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
#[cfg(any(target_os = "macos", test))]
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", test))]
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const DATABASE_FILE: &str = "runtime.sqlite3";
const STORE_KEY_ENV: &str = "LLAMA_NATIVE_KIT_STORE_KEY_HEX";
#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "com.delysis.llama-native-kit.mom-llama.store.v1";

// RuntimeStore is intentionally cheap to reopen, but asking macOS Keychain for
// the same installation key on every repository operation can produce repeated
// authorization prompts for development-signed builds. Keep the key only in
// process memory after the first successful OS lookup. The cache is indexed by
// the hashed data-directory account, so isolated stores never share keys.
#[cfg(target_os = "macos")]
static INSTALLATION_KEYS: OnceLock<Mutex<HashMap<String, CachedInstallationKey>>> = OnceLock::new();

#[cfg(any(target_os = "macos", test))]
#[derive(Clone)]
enum CachedInstallationKey {
    Available([u8; 32]),
    Unavailable(String),
}

#[derive(Clone)]
pub(crate) struct RuntimeStore {
    path: PathBuf,
    key: [u8; 32],
}

pub(crate) struct DocumentMutations<'a> {
    store: &'a RuntimeStore,
    writes: Vec<(String, Vec<u8>, Vec<u8>)>,
    deletes: Vec<String>,
}

impl DocumentMutations<'_> {
    pub(crate) fn put_bytes(&mut self, namespace: &str, value: &[u8]) -> Result<()> {
        let (nonce, ciphertext) = self.store.encrypt_bytes(namespace, value)?;
        self.writes.push((namespace.to_string(), nonce, ciphertext));
        self.deletes.retain(|candidate| candidate != namespace);
        Ok(())
    }

    pub(crate) fn delete(&mut self, namespace: &str) {
        self.writes
            .retain(|(candidate, _, _)| candidate != namespace);
        if !self.deletes.iter().any(|candidate| candidate == namespace) {
            self.deletes.push(namespace.to_string());
        }
    }
}

impl RuntimeStore {
    pub(crate) fn current() -> Result<Self> {
        Self::open(&crate::config::resolve_data_dir())
    }

    pub(crate) fn open(data_dir: &Path) -> Result<Self> {
        fs::create_dir_all(data_dir)?;
        let key = resolve_store_key(data_dir)?;
        Self::open_with_key(data_dir, key)
    }

    pub(crate) fn open_with_key(data_dir: &Path, key: [u8; 32]) -> Result<Self> {
        fs::create_dir_all(data_dir)?;
        let store = Self {
            path: data_dir.join(DATABASE_FILE),
            key,
        };
        let connection = store.connection()?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS encrypted_documents (
                namespace TEXT PRIMARY KEY NOT NULL,
                nonce BLOB NOT NULL,
                ciphertext BLOB NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS receipts (
                receipt_id TEXT PRIMARY KEY NOT NULL,
                command_id TEXT NOT NULL,
                nonce BLOB NOT NULL,
                ciphertext BLOB NOT NULL,
                created_at INTEGER NOT NULL
            );",
        )?;
        Ok(store)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn get<T>(&self, namespace: &str) -> Result<Option<T>>
    where
        T: DeserializeOwned,
    {
        let connection = self.connection()?;
        let encrypted = connection
            .query_row(
                "SELECT nonce, ciphertext FROM encrypted_documents WHERE namespace = ?1",
                [namespace],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        encrypted
            .map(|(nonce, ciphertext)| self.decrypt_json(namespace, &nonce, &ciphertext))
            .transpose()
    }

    /// Read a disposable encrypted cache document, quarantining corrupt bytes.
    ///
    /// This is deliberately separate from [`Self::get`]. Product records such
    /// as conversations, settings, and personas must continue to fail closed
    /// when authentication or decoding fails. A native prefix cache is only a
    /// performance hint: preserving its raw authenticated bytes for diagnosis,
    /// removing it from the live namespace, and returning a cache miss is both
    /// safe and availability-preserving.
    pub(crate) fn get_disposable_cache<T>(&self, namespace: &str) -> Result<Option<T>>
    where
        T: DeserializeOwned,
    {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let encrypted = transaction
            .query_row(
                "SELECT nonce, ciphertext FROM encrypted_documents WHERE namespace = ?1",
                [namespace],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        let Some((nonce, ciphertext)) = encrypted else {
            transaction.commit()?;
            return Ok(None);
        };
        match self.decrypt_json(namespace, &nonce, &ciphertext) {
            Ok(value) => {
                transaction.commit()?;
                Ok(Some(value))
            }
            Err(_) => {
                let quarantine_namespace =
                    disposable_cache_quarantine_namespace(namespace, &nonce, &ciphertext);
                transaction.execute(
                    "INSERT INTO encrypted_documents(namespace, nonce, ciphertext, updated_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![quarantine_namespace, nonce, ciphertext, timestamp_i64()],
                )?;
                transaction.execute(
                    "DELETE FROM encrypted_documents WHERE namespace = ?1",
                    [namespace],
                )?;
                transaction.commit()?;
                Ok(None)
            }
        }
    }

    pub(crate) fn put<T>(&self, namespace: &str, value: &T) -> Result<()>
    where
        T: Serialize,
    {
        let connection = self.connection()?;
        let (nonce, ciphertext) = self.encrypt_json(namespace, value)?;
        connection.execute(
            "INSERT INTO encrypted_documents(namespace, nonce, ciphertext, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(namespace) DO UPDATE SET
               nonce = excluded.nonce,
               ciphertext = excluded.ciphertext,
               updated_at = excluded.updated_at",
            params![namespace, nonce, ciphertext, timestamp_i64()],
        )?;
        Ok(())
    }

    pub(crate) fn get_bytes(&self, namespace: &str) -> Result<Option<Vec<u8>>> {
        let connection = self.connection()?;
        let encrypted = connection
            .query_row(
                "SELECT nonce, ciphertext FROM encrypted_documents WHERE namespace = ?1",
                [namespace],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        encrypted
            .map(|(nonce, ciphertext)| self.decrypt_bytes(namespace, &nonce, &ciphertext))
            .transpose()
    }

    pub(crate) fn put_bytes(&self, namespace: &str, value: &[u8]) -> Result<()> {
        let connection = self.connection()?;
        let (nonce, ciphertext) = self.encrypt_bytes(namespace, value)?;
        connection.execute(
            "INSERT INTO encrypted_documents(namespace, nonce, ciphertext, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(namespace) DO UPDATE SET
               nonce = excluded.nonce,
               ciphertext = excluded.ciphertext,
               updated_at = excluded.updated_at",
            params![namespace, nonce, ciphertext, timestamp_i64()],
        )?;
        Ok(())
    }

    /// Replace several encrypted documents in one SQLite transaction.
    ///
    /// Callers use this when a product operation spans independently versioned
    /// documents (for example a conversation, its draft, and attachment
    /// metadata). Either every encrypted value becomes visible, or none do.
    pub(crate) fn put_documents_atomically<I, N, V>(&self, documents: I) -> Result<()>
    where
        I: IntoIterator<Item = (N, V)>,
        N: Into<String>,
        V: AsRef<[u8]>,
    {
        let encrypted = documents
            .into_iter()
            .map(|(namespace, value)| {
                let namespace = namespace.into();
                let (nonce, ciphertext) = self.encrypt_bytes(&namespace, value.as_ref())?;
                Ok((namespace, nonce, ciphertext))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (namespace, nonce, ciphertext) in encrypted {
            transaction.execute(
                "INSERT INTO encrypted_documents(namespace, nonce, ciphertext, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(namespace) DO UPDATE SET
                   nonce = excluded.nonce,
                   ciphertext = excluded.ciphertext,
                   updated_at = excluded.updated_at",
                params![namespace, nonce, ciphertext, timestamp_i64()],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn delete(&self, namespace: &str) -> Result<bool> {
        Ok(self.connection()?.execute(
            "DELETE FROM encrypted_documents WHERE namespace = ?1",
            [namespace],
        )? > 0)
    }

    pub(crate) fn mutate<T, R>(
        &self,
        namespace: &str,
        default: impl FnOnce() -> T,
        mutation: impl FnOnce(&mut T) -> Result<R>,
    ) -> Result<R>
    where
        T: Serialize + DeserializeOwned,
    {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let encrypted = transaction
            .query_row(
                "SELECT nonce, ciphertext FROM encrypted_documents WHERE namespace = ?1",
                [namespace],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        let mut value = match encrypted {
            Some((nonce, ciphertext)) => self.decrypt_json(namespace, &nonce, &ciphertext)?,
            None => default(),
        };
        let result = mutation(&mut value)?;
        let (nonce, ciphertext) = self.encrypt_json(namespace, &value)?;
        transaction.execute(
            "INSERT INTO encrypted_documents(namespace, nonce, ciphertext, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(namespace) DO UPDATE SET
               nonce = excluded.nonce,
               ciphertext = excluded.ciphertext,
               updated_at = excluded.updated_at",
            params![namespace, nonce, ciphertext, timestamp_i64()],
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub(crate) fn mutate_documents<T, R>(
        &self,
        namespace: &str,
        default: impl FnOnce() -> T,
        mutation: impl FnOnce(&mut T, &mut DocumentMutations<'_>) -> Result<R>,
    ) -> Result<R>
    where
        T: Serialize + DeserializeOwned,
    {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let encrypted = transaction
            .query_row(
                "SELECT nonce, ciphertext FROM encrypted_documents WHERE namespace = ?1",
                [namespace],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        let mut value = match encrypted {
            Some((nonce, ciphertext)) => self.decrypt_json(namespace, &nonce, &ciphertext)?,
            None => default(),
        };
        let mut documents = DocumentMutations {
            store: self,
            writes: Vec::new(),
            deletes: Vec::new(),
        };
        let result = mutation(&mut value, &mut documents)?;
        let (nonce, ciphertext) = self.encrypt_json(namespace, &value)?;

        for deleted in documents.deletes {
            transaction.execute(
                "DELETE FROM encrypted_documents WHERE namespace = ?1",
                [deleted],
            )?;
        }
        for (document_namespace, document_nonce, document_ciphertext) in documents.writes {
            transaction.execute(
                "INSERT INTO encrypted_documents(namespace, nonce, ciphertext, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(namespace) DO UPDATE SET
                   nonce = excluded.nonce,
                   ciphertext = excluded.ciphertext,
                   updated_at = excluded.updated_at",
                params![
                    document_namespace,
                    document_nonce,
                    document_ciphertext,
                    timestamp_i64()
                ],
            )?;
        }
        transaction.execute(
            "INSERT INTO encrypted_documents(namespace, nonce, ciphertext, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(namespace) DO UPDATE SET
               nonce = excluded.nonce,
               ciphertext = excluded.ciphertext,
               updated_at = excluded.updated_at",
            params![namespace, nonce, ciphertext, timestamp_i64()],
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub(crate) fn import_json_once<T>(&self, namespace: &str, legacy_path: &Path) -> Result<bool>
    where
        T: Serialize + DeserializeOwned,
    {
        if self.get::<T>(namespace)?.is_some() || !legacy_path.is_file() {
            return Ok(false);
        }
        let raw = fs::read(legacy_path)?;
        let value = serde_json::from_slice::<T>(&raw)
            .with_context(|| format!("failed to import {}", legacy_path.display()))?;
        self.put(namespace, &value)?;
        let round_trip = self
            .get::<T>(namespace)?
            .ok_or_else(|| anyhow!("encrypted legacy import did not round-trip"))?;
        let expected = serde_json::to_vec(&value)?;
        let actual = serde_json::to_vec(&round_trip)?;
        if expected != actual {
            return Err(anyhow!("encrypted legacy import changed serialized data"));
        }
        Ok(true)
    }

    pub(crate) fn write_receipt<T>(
        &self,
        receipt_id: &str,
        command_id: &str,
        receipt: &T,
    ) -> Result<()>
    where
        T: Serialize,
    {
        let namespace = format!("receipt:{receipt_id}");
        let (nonce, ciphertext) = self.encrypt_json(&namespace, receipt)?;
        self.connection()?.execute(
            "INSERT OR REPLACE INTO receipts(receipt_id, command_id, nonce, ciphertext, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![receipt_id, command_id, nonce, ciphertext, timestamp_i64()],
        )?;
        Ok(())
    }

    fn connection(&self) -> Result<Connection> {
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        Ok(connection)
    }

    fn encrypt_json<T>(&self, namespace: &str, value: &T) -> Result<(Vec<u8>, Vec<u8>)>
    where
        T: Serialize,
    {
        let plaintext = serde_json::to_vec(value)?;
        self.encrypt_bytes(namespace, &plaintext)
    }

    fn encrypt_bytes(&self, namespace: &str, plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        let mut nonce = [0_u8; 24];
        getrandom::fill(&mut nonce).map_err(|error| anyhow!("nonce generation failed: {error}"))?;
        let cipher = XChaCha20Poly1305::new(Key::from_slice(&self.key));
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: namespace.as_bytes(),
                },
            )
            .map_err(|_| anyhow!("authenticated encryption failed"))?;
        Ok((nonce.to_vec(), ciphertext))
    }

    fn decrypt_json<T>(&self, namespace: &str, nonce: &[u8], ciphertext: &[u8]) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let plaintext = self.decrypt_bytes(namespace, nonce, ciphertext)?;
        Ok(serde_json::from_slice(&plaintext)?)
    }

    fn decrypt_bytes(&self, namespace: &str, nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
        if nonce.len() != 24 {
            return Err(anyhow!("encrypted record has an invalid nonce length"));
        }
        XChaCha20Poly1305::new(Key::from_slice(&self.key))
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad: namespace.as_bytes(),
                },
            )
            .map_err(|_| anyhow!("encrypted record authentication failed"))
    }
}

fn timestamp_i64() -> i64 {
    i64::try_from(crate::now_ms()).unwrap_or(i64::MAX)
}

fn disposable_cache_quarantine_namespace(
    namespace: &str,
    nonce: &[u8],
    ciphertext: &[u8],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(namespace.as_bytes());
    hasher.update(nonce);
    hasher.update(ciphertext);
    let digest = format!("{:x}", hasher.finalize());
    format!(
        "quarantine.disposable-cache.{}.{}",
        timestamp_i64(),
        &digest[..16]
    )
}

fn resolve_store_key(data_dir: &Path) -> Result<[u8; 32]> {
    // Unit tests share one process and may exercise the public test data-dir
    // override concurrently.  Store identity must follow the explicit path,
    // not the instantaneous value of that unrelated process-global switch.
    // `cfg!(test)` is immutable for this binary and keeps every unit-test open
    // on the same deterministic key derivation.
    let deterministic_test_store =
        deterministic_test_store(crate::config::data_dir_override_is_set());
    if let Some(key) = configured_store_key(
        data_dir,
        std::env::var(STORE_KEY_ENV).ok().as_deref(),
        deterministic_test_store,
        crate::config::insecure_development_store_enabled(),
    )? {
        return Ok(key);
    }
    #[cfg(target_os = "macos")]
    {
        let account = keychain_account(data_dir);
        cached_installation_key(&account, &INSTALLATION_KEYS, || {
            load_or_create_macos_key(&account)
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = data_dir;
        Err(anyhow!(
            "Set LLAMA_NATIVE_KIT_STORE_KEY_HEX on platforms without a supported OS credential store"
        ))
    }
}

const fn deterministic_test_store(data_dir_override_is_set: bool) -> bool {
    cfg!(test) || data_dir_override_is_set
}

fn configured_store_key(
    data_dir: &Path,
    environment_key: Option<&str>,
    deterministic_test_override: bool,
    insecure_development_store: bool,
) -> Result<Option<[u8; 32]>> {
    if let Some(value) = environment_key {
        return decode_hex_key(value).map(Some);
    }
    if deterministic_test_override {
        let mut hasher = Sha256::new();
        hasher.update(b"mom-llama-test-store-key-v1");
        hasher.update(data_dir.to_string_lossy().as_bytes());
        return Ok(Some(hasher.finalize().into()));
    }
    if insecure_development_store {
        // Debug bundles intentionally trade confidentiality for iteration speed.
        // The predictable key keeps the on-disk schema identical to production
        // without invoking Keychain, and the separate development data directory
        // prevents this store from ever being mistaken for the secure release store.
        let mut hasher = Sha256::new();
        hasher.update(b"mom-llama-insecure-development-store-key-v1");
        hasher.update(data_dir.to_string_lossy().as_bytes());
        return Ok(Some(hasher.finalize().into()));
    }
    Ok(None)
}

#[cfg(any(target_os = "macos", test))]
fn cached_installation_key(
    account: &str,
    cache: &OnceLock<Mutex<HashMap<String, CachedInstallationKey>>>,
    load: impl FnOnce() -> Result<[u8; 32]>,
) -> Result<[u8; 32]> {
    let cache = cache.get_or_init(|| Mutex::new(HashMap::new()));
    let mut keys = cache
        .lock()
        .map_err(|_| anyhow!("installation-key memory cache is poisoned"))?;
    if let Some(cached) = keys.get(account) {
        return match cached {
            CachedInstallationKey::Available(key) => Ok(*key),
            CachedInstallationKey::Unavailable(message) => Err(anyhow!(message.clone())),
        };
    }
    // Hold the lock across the first lookup so concurrent startup commands
    // cannot independently trigger the same Keychain authorization request.
    match load() {
        Ok(key) => {
            keys.insert(account.to_string(), CachedInstallationKey::Available(key));
            Ok(key)
        }
        Err(error) => {
            let message = format!("{error:#}");
            keys.insert(
                account.to_string(),
                CachedInstallationKey::Unavailable(message.clone()),
            );
            Err(anyhow!(message))
        }
    }
}

#[cfg(target_os = "macos")]
fn load_or_create_macos_key(account: &str) -> Result<[u8; 32]> {
    const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
    match security_framework::passwords::get_generic_password(KEYCHAIN_SERVICE, account) {
        Ok(key) => key
            .try_into()
            .map_err(|_| anyhow!("Keychain key is not 32 bytes")),
        Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => {
            let mut key = [0_u8; 32];
            getrandom::fill(&mut key)
                .map_err(|error| anyhow!("store key generation failed: {error}"))?;
            security_framework::passwords::set_generic_password(KEYCHAIN_SERVICE, account, &key)?;
            Ok(key)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(target_os = "macos")]
fn keychain_account(data_dir: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data_dir.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())
}

fn decode_hex_key(input: &str) -> Result<[u8; 32]> {
    if input.len() != 64 {
        return Err(anyhow!(
            "LLAMA_NATIVE_KIT_STORE_KEY_HEX must contain 64 hex digits"
        ));
    }
    let mut key = [0_u8; 32];
    for (index, chunk) in input.as_bytes().chunks_exact(2).enumerate() {
        let text = std::str::from_utf8(chunk)?;
        key[index] = u8::from_str_radix(text, 16)
            .with_context(|| "LLAMA_NATIVE_KIT_STORE_KEY_HEX contains non-hex data")?;
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[test]
    fn unit_test_key_selection_does_not_depend_on_the_mutable_data_dir_override() {
        assert!(deterministic_test_store(false));
        assert!(deterministic_test_store(true));
    }

    #[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
    struct SecretDocument {
        values: Vec<String>,
    }

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("mom-llama-store-{name}-{}", crate::now_ms()))
    }

    #[test]
    fn encrypted_document_round_trips_without_plaintext_at_rest() -> Result<()> {
        let data_dir = test_dir("roundtrip");
        let store = RuntimeStore::open_with_key(&data_dir, [7_u8; 32])?;
        let secret = "private practice note 4b7d9f";
        store.put(
            "conversations",
            &SecretDocument {
                values: vec![secret.to_string()],
            },
        )?;
        assert_eq!(
            store.get::<SecretDocument>("conversations")?,
            Some(SecretDocument {
                values: vec![secret.to_string()]
            })
        );
        let database = fs::read(store.path())?;
        assert!(
            !database
                .windows(secret.len())
                .any(|window| window == secret.as_bytes())
        );
        Ok(())
    }

    #[test]
    fn immediate_transactions_do_not_lose_concurrent_mutations() -> Result<()> {
        let data_dir = test_dir("concurrency");
        let store = RuntimeStore::open_with_key(&data_dir, [9_u8; 32])?;
        let workers = (0..8)
            .map(|index| {
                let store = store.clone();
                std::thread::spawn(move || {
                    store.mutate("values", SecretDocument::default, |document| {
                        document.values.push(format!("value-{index}"));
                        Ok(())
                    })
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker
                .join()
                .map_err(|_| anyhow!("mutation worker panicked"))??;
        }
        let document = store
            .get::<SecretDocument>("values")?
            .ok_or_else(|| anyhow!("document missing"))?;
        assert_eq!(document.values.len(), 8);
        Ok(())
    }

    #[test]
    fn metadata_and_blob_mutations_commit_or_roll_back_together() -> Result<()> {
        let data_dir = test_dir("multi-document");
        let store = RuntimeStore::open_with_key(&data_dir, [10_u8; 32])?;
        store.put(
            "metadata",
            &SecretDocument {
                values: vec!["old".to_string()],
            },
        )?;
        store.put_bytes("blob.old", b"old bytes")?;

        let failed: Result<()> = store.mutate_documents(
            "metadata",
            SecretDocument::default,
            |metadata, documents| {
                metadata.values = vec!["new".to_string()];
                documents.delete("blob.old");
                documents.put_bytes("blob.new", b"new bytes")?;
                Err(anyhow!("force rollback"))
            },
        );
        assert!(failed.is_err());
        assert_eq!(
            store.get::<SecretDocument>("metadata")?,
            Some(SecretDocument {
                values: vec!["old".to_string()]
            })
        );
        assert_eq!(store.get_bytes("blob.old")?, Some(b"old bytes".to_vec()));
        assert_eq!(store.get_bytes("blob.new")?, None);
        Ok(())
    }

    #[test]
    fn wrong_key_and_tampering_fail_closed() -> Result<()> {
        let data_dir = test_dir("tamper");
        let store = RuntimeStore::open_with_key(&data_dir, [1_u8; 32])?;
        store.put(
            "secret",
            &SecretDocument {
                values: vec!["sensitive".to_string()],
            },
        )?;
        let wrong = RuntimeStore::open_with_key(&data_dir, [2_u8; 32])?;
        assert!(wrong.get::<SecretDocument>("secret").is_err());
        let connection = Connection::open(store.path())?;
        connection.execute(
            "UPDATE encrypted_documents SET ciphertext = X'00' WHERE namespace = 'secret'",
            [],
        )?;
        assert!(store.get::<SecretDocument>("secret").is_err());
        Ok(())
    }

    #[test]
    fn disposable_cache_corruption_is_quarantined_but_product_corruption_is_not_masked()
    -> Result<()> {
        let data_dir = test_dir("cache-quarantine");
        let store = RuntimeStore::open_with_key(&data_dir, [17_u8; 32])?;
        store.put(
            "native-host-prefix-cache.mom-llama",
            &SecretDocument {
                values: vec!["disposable".to_string()],
            },
        )?;
        store.put(
            "conversations",
            &SecretDocument {
                values: vec!["must fail closed".to_string()],
            },
        )?;
        let connection = Connection::open(store.path())?;
        connection.execute(
            "UPDATE encrypted_documents SET ciphertext = X'00'
             WHERE namespace IN ('native-host-prefix-cache.mom-llama', 'conversations')",
            [],
        )?;

        assert_eq!(
            store.get_disposable_cache::<SecretDocument>("native-host-prefix-cache.mom-llama")?,
            None,
            "corrupt prefix state must become an ordinary cache miss"
        );
        assert!(
            store.get::<SecretDocument>("conversations").is_err(),
            "ordinary encrypted product data must remain fail-closed"
        );

        let live_cache_rows: i64 = connection.query_row(
            "SELECT COUNT(*) FROM encrypted_documents
             WHERE namespace = 'native-host-prefix-cache.mom-llama'",
            [],
            |row| row.get(0),
        )?;
        let quarantine_rows: i64 = connection.query_row(
            "SELECT COUNT(*) FROM encrypted_documents
             WHERE namespace LIKE 'quarantine.disposable-cache.%'",
            [],
            |row| row.get(0),
        )?;
        let product_rows: i64 = connection.query_row(
            "SELECT COUNT(*) FROM encrypted_documents WHERE namespace = 'conversations'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(live_cache_rows, 0);
        assert_eq!(quarantine_rows, 1);
        assert_eq!(product_rows, 1);
        Ok(())
    }

    #[test]
    fn explicit_key_has_priority_over_the_deterministic_test_override() -> Result<()> {
        let configured = "11".repeat(32);
        let key = configured_store_key(Path::new("/isolated/test"), Some(&configured), true, true)?
            .ok_or_else(|| anyhow!("configured key missing"))?;
        assert_eq!(key, [0x11; 32]);
        Ok(())
    }

    #[test]
    fn development_store_uses_a_prompt_free_predictable_key_only_when_enabled() -> Result<()> {
        let data_dir = Path::new("/isolated/development");
        let first = configured_store_key(data_dir, None, false, true)?
            .ok_or_else(|| anyhow!("development key missing"))?;
        let second = configured_store_key(data_dir, None, false, true)?
            .ok_or_else(|| anyhow!("development key missing"))?;
        assert_eq!(first, second);
        assert!(configured_store_key(data_dir, None, false, false)?.is_none());
        Ok(())
    }

    #[test]
    fn installation_key_provider_runs_once_per_process_and_account() -> Result<()> {
        use std::cell::Cell;

        let cache = OnceLock::new();
        let calls = Cell::new(0_u32);
        let first = cached_installation_key("account-a", &cache, || {
            calls.set(calls.get() + 1);
            Ok([3_u8; 32])
        })?;
        let second = cached_installation_key("account-a", &cache, || {
            calls.set(calls.get() + 1);
            Ok([4_u8; 32])
        })?;
        assert_eq!(first, [3_u8; 32]);
        assert_eq!(second, first);
        assert_eq!(calls.get(), 1);
        Ok(())
    }

    #[test]
    fn denied_installation_key_is_not_retried_until_relaunch() {
        use std::cell::Cell;

        let cache = OnceLock::new();
        let calls = Cell::new(0_u32);
        for _ in 0..2 {
            let error = cached_installation_key("denied-account", &cache, || {
                calls.set(calls.get() + 1);
                Err(anyhow!("user denied Keychain access"))
            })
            .expect_err("denied key must stay unavailable");
            assert!(error.to_string().contains("denied Keychain access"));
        }
        assert_eq!(calls.get(), 1);
    }
}
