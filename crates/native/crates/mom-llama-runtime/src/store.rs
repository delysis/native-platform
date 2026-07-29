use anyhow::{Context, Result, anyhow};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DATABASE_FILE: &str = "runtime.sqlite3";
const STORE_KEY_ENV: &str = "MOM_LLAMA_STORE_KEY_HEX";
const KEYCHAIN_SERVICE: &str = "coop.mom-llama-lab.store.v1";

#[derive(Clone)]
pub(crate) struct RuntimeStore {
    path: PathBuf,
    key: [u8; 32],
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

fn resolve_store_key(data_dir: &Path) -> Result<[u8; 32]> {
    if crate::config::data_dir_override_is_set() {
        let mut hasher = Sha256::new();
        hasher.update(b"mom-llama-test-store-key-v1");
        hasher.update(data_dir.to_string_lossy().as_bytes());
        return Ok(hasher.finalize().into());
    }
    if let Ok(value) = std::env::var(STORE_KEY_ENV) {
        return decode_hex_key(&value);
    }
    #[cfg(target_os = "macos")]
    {
        const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
        let account = keychain_account(data_dir);
        match security_framework::passwords::get_generic_password(KEYCHAIN_SERVICE, &account) {
            Ok(key) => key
                .try_into()
                .map_err(|_| anyhow!("Keychain key is not 32 bytes")),
            Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => {
                let mut key = [0_u8; 32];
                getrandom::fill(&mut key)
                    .map_err(|error| anyhow!("store key generation failed: {error}"))?;
                security_framework::passwords::set_generic_password(
                    KEYCHAIN_SERVICE,
                    &account,
                    &key,
                )?;
                Ok(key)
            }
            Err(error) => Err(error.into()),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = data_dir;
        Err(anyhow!(
            "Set MOM_LLAMA_STORE_KEY_HEX on platforms without a supported OS credential store"
        ))
    }
}

fn keychain_account(data_dir: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data_dir.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())
}

fn decode_hex_key(input: &str) -> Result<[u8; 32]> {
    if input.len() != 64 {
        return Err(anyhow!(
            "MOM_LLAMA_STORE_KEY_HEX must contain 64 hex digits"
        ));
    }
    let mut key = [0_u8; 32];
    for (index, chunk) in input.as_bytes().chunks_exact(2).enumerate() {
        let text = std::str::from_utf8(chunk)?;
        key[index] = u8::from_str_radix(text, 16)
            .with_context(|| "MOM_LLAMA_STORE_KEY_HEX contains non-hex data")?;
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

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
}
