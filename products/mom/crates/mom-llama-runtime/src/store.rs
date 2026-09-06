use anyhow::{Context, Result, anyhow};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
#[cfg(any(target_os = "macos", test))]
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", test))]
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const DATABASE_FILE: &str = "runtime.sqlite3";
const STORE_APPLICATION_ID: i64 = 0x4d4f4d31; // MOM1
const STORE_SCHEMA_VERSION: i64 = 1;
const STORE_SCHEMA: &str = "CREATE TABLE encrypted_documents (
                namespace TEXT PRIMARY KEY NOT NULL,
                nonce BLOB NOT NULL,
                ciphertext BLOB NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE receipts (
                receipt_id TEXT PRIMARY KEY NOT NULL,
                command_id TEXT NOT NULL,
                nonce BLOB NOT NULL,
                ciphertext BLOB NOT NULL,
                created_at INTEGER NOT NULL
            );";

/// Build privately, then publish a complete database without replacing any
/// existing path. The hard-link operation arbitrates independent processes as
/// well as threads; readers never observe our empty initialization file.
fn initialize_store(data_dir: &Path, destination: &Path) -> Result<()> {
    struct Candidate(PathBuf);
    impl Drop for Candidate {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let path = data_dir.join(format!(".runtime-init-{}.sqlite3", uuid::Uuid::new_v4()));
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let candidate = Candidate(path);
    {
        let mut connection =
            Connection::open_with_flags(&candidate.0, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        // Keep every committed byte in this one file before linking it.
        connection.pragma_update(None, "journal_mode", "DELETE")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(STORE_SCHEMA)?;
        transaction.pragma_update(None, "application_id", STORE_APPLICATION_ID)?;
        transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
        validate_schema(&transaction)?;
        transaction.commit()?;
    }
    match fs::hard_link(&candidate.0, destination) {
        Ok(()) => {
            #[cfg(unix)]
            fs::File::open(data_dir)?.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Legacy v0 has the identical physical schema. Do not stamp it or rewrite
/// encrypted records: older binaries and retained keys remain compatible.
fn validate_schema(connection: &Connection) -> Result<()> {
    let application_id: i64 =
        connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if !matches!(
        (application_id, version),
        (0, 0) | (STORE_APPLICATION_ID, STORE_SCHEMA_VERSION)
    ) {
        return Err(anyhow!("unsupported Mom store identity or schema version"));
    }
    let mut statement = connection
        .prepare("SELECT sql FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' ORDER BY name")?;
    let actual = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let normalize = |sql: &str| sql.split_whitespace().collect::<Vec<_>>().join(" ");
    let expected = STORE_SCHEMA
        .split(';')
        .filter(|sql| !sql.trim().is_empty())
        .map(normalize)
        .collect::<Vec<_>>();
    if actual.iter().map(|sql| normalize(sql)).collect::<Vec<_>>() != expected {
        return Err(anyhow!("unsupported Mom store physical schema"));
    }
    Ok(())
}

// SQLite can decline to invoke the busy handler when changing journal mode
// would deadlock with a competing reader/initializer. Drop the entire failed
// connection before retrying, so no read lock survives into the next attempt.
// Retry only SQLITE_BUSY; identity, corruption and I/O errors stay terminal.
fn retry_store_busy<T>(mut operation: impl FnMut() -> Result<T>) -> Result<T> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match operation() {
            Err(error)
                if matches!(
                    error.downcast_ref::<rusqlite::Error>(),
                    Some(rusqlite::Error::SqliteFailure(code, _))
                        if code.code == rusqlite::ErrorCode::DatabaseBusy
                ) && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

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

pub(crate) struct DocumentMutations<'store, 'transaction, 'connection> {
    store: &'store RuntimeStore,
    transaction: &'transaction Transaction<'connection>,
    writes: Vec<(String, Vec<u8>, Vec<u8>)>,
    deletes: Vec<String>,
    receipt_writes: Vec<(String, String, Vec<u8>, Vec<u8>)>,
}

pub(crate) struct DocumentSnapshot<'store, 'transaction, 'connection> {
    store: &'store RuntimeStore,
    transaction: &'transaction Transaction<'connection>,
}

impl DocumentSnapshot<'_, '_, '_> {
    pub(crate) fn get<T>(&self, namespace: &str) -> Result<Option<T>>
    where
        T: DeserializeOwned,
    {
        get_document(self.store, self.transaction, namespace)
    }
}

impl DocumentMutations<'_, '_, '_> {
    pub(crate) fn get<T>(&self, namespace: &str) -> Result<Option<T>>
    where
        T: DeserializeOwned,
    {
        get_document(self.store, self.transaction, namespace)
    }

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

    pub(crate) fn put_receipt<T>(
        &mut self,
        receipt_id: &str,
        command_id: &str,
        receipt: &T,
    ) -> Result<()>
    where
        T: Serialize,
    {
        let namespace = format!("receipt:{receipt_id}");
        let (nonce, ciphertext) = self.store.encrypt_json(&namespace, receipt)?;
        self.receipt_writes.push((
            receipt_id.to_string(),
            command_id.to_string(),
            nonce,
            ciphertext,
        ));
        Ok(())
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
        Self::open_with_key_at_creation(data_dir, key, || {})
    }

    fn open_with_key_at_creation(
        data_dir: &Path,
        key: [u8; 32],
        before_creation: impl FnOnce(),
    ) -> Result<Self> {
        fs::create_dir_all(data_dir)?;
        let store = Self {
            path: data_dir.join(DATABASE_FILE),
            key,
        };
        if !store.path.exists() {
            before_creation();
            initialize_store(data_dir, &store.path)?;
        }
        // Validate the published winner read-only, including a foreign database
        // that appeared while our private candidate was being initialized.
        let connection =
            Connection::open_with_flags(&store.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        validate_schema(&connection)?;
        drop(connection);
        // Validate again on the exact connection before applying write pragmas.
        let _connection = store.connection()?;
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

    #[cfg(test)]
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

    /// Mutate two encrypted documents under one immediate SQLite transaction.
    ///
    /// This is the narrow boundary for product facts that must become visible
    /// together, while retaining a separate typed owner for each document.
    #[cfg(test)]
    pub(crate) fn mutate_pair<A, B, R>(
        &self,
        first_namespace: &str,
        first_default: impl FnOnce() -> A,
        second_namespace: &str,
        second_default: impl FnOnce() -> B,
        mutation: impl FnOnce(&mut A, &mut B) -> Result<R>,
    ) -> Result<R>
    where
        A: Serialize + DeserializeOwned,
        B: Serialize + DeserializeOwned,
    {
        if first_namespace == second_namespace {
            anyhow::bail!("paired encrypted document namespaces must be distinct");
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let first_encrypted = transaction
            .query_row(
                "SELECT nonce, ciphertext FROM encrypted_documents WHERE namespace = ?1",
                [first_namespace],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        let second_encrypted = transaction
            .query_row(
                "SELECT nonce, ciphertext FROM encrypted_documents WHERE namespace = ?1",
                [second_namespace],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        let mut first = match first_encrypted {
            Some((nonce, ciphertext)) => self.decrypt_json(first_namespace, &nonce, &ciphertext)?,
            None => first_default(),
        };
        let mut second = match second_encrypted {
            Some((nonce, ciphertext)) => {
                self.decrypt_json(second_namespace, &nonce, &ciphertext)?
            }
            None => second_default(),
        };
        let result = mutation(&mut first, &mut second)?;
        let (first_nonce, first_ciphertext) = self.encrypt_json(first_namespace, &first)?;
        let (second_nonce, second_ciphertext) = self.encrypt_json(second_namespace, &second)?;
        for (namespace, nonce, ciphertext) in [
            (first_namespace, first_nonce, first_ciphertext),
            (second_namespace, second_nonce, second_ciphertext),
        ] {
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
        Ok(result)
    }

    pub(crate) fn mutate_documents<T, R>(
        &self,
        namespace: &str,
        default: impl FnOnce() -> T,
        mutation: impl FnOnce(&mut T, &mut DocumentMutations<'_, '_, '_>) -> Result<R>,
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
            transaction: &transaction,
            writes: Vec::new(),
            deletes: Vec::new(),
            receipt_writes: Vec::new(),
        };
        let result = mutation(&mut value, &mut documents)?;
        let (nonce, ciphertext) = self.encrypt_json(namespace, &value)?;

        let DocumentMutations {
            writes,
            deletes,
            receipt_writes,
            ..
        } = documents;
        for deleted in deletes {
            transaction.execute(
                "DELETE FROM encrypted_documents WHERE namespace = ?1",
                [deleted],
            )?;
        }
        for (document_namespace, document_nonce, document_ciphertext) in writes {
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
        for (receipt_id, command_id, receipt_nonce, receipt_ciphertext) in receipt_writes {
            transaction.execute(
                "INSERT INTO receipts(receipt_id, command_id, nonce, ciphertext, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    receipt_id,
                    command_id,
                    receipt_nonce,
                    receipt_ciphertext,
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

    pub(crate) fn read_documents<R>(
        &self,
        read: impl FnOnce(&DocumentSnapshot<'_, '_, '_>) -> Result<R>,
    ) -> Result<R> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let snapshot = DocumentSnapshot {
            store: self,
            transaction: &transaction,
        };
        read(&snapshot)
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
        retry_store_busy(|| self.connection_once())
    }

    fn connection_once(&self) -> Result<Connection> {
        let connection =
            Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        connection.busy_timeout(Duration::from_millis(100))?;
        validate_schema(&connection)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(Duration::from_secs(5))?;
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

fn get_document<T>(
    store: &RuntimeStore,
    transaction: &Transaction<'_>,
    namespace: &str,
) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    let encrypted = transaction
        .query_row(
            "SELECT nonce, ciphertext FROM encrypted_documents WHERE namespace = ?1",
            [namespace],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()?;
    encrypted
        .map(|(nonce, ciphertext)| store.decrypt_json(namespace, &nonce, &ciphertext))
        .transpose()
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

#[cfg(any(target_os = "macos", test))]
fn clear_cached_installation_key_failure(
    account: &str,
    cache: &OnceLock<Mutex<HashMap<String, CachedInstallationKey>>>,
) -> Result<bool> {
    let Some(cache) = cache.get() else {
        return Ok(false);
    };
    let mut keys = cache
        .lock()
        .map_err(|_| anyhow!("installation-key memory cache is poisoned"))?;
    let failed = matches!(
        keys.get(account),
        Some(CachedInstallationKey::Unavailable(_))
    );
    if failed {
        keys.remove(account);
    }
    Ok(failed)
}

pub(crate) fn prepare_secure_store_retry() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let data_dir = crate::config::resolve_data_dir();
        let account = keychain_account(&data_dir);
        clear_cached_installation_key_failure(&account, &INSTALLATION_KEYS)?;
    }
    Ok(())
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
    use crate::conversation_store::{CONVERSATIONS_NAMESPACE, Conversation, ConversationDb};
    use llama_native_cache::PrefixCacheValue;
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;

    #[test]
    fn journal_mode_contention_retries_after_releasing_the_failed_connection() -> Result<()> {
        let dir = test_dir("journal-mode-contention");
        fs::create_dir_all(&dir)?;
        let path = dir.join(DATABASE_FILE);
        initialize_store(&dir, &path)?;
        let reader = Connection::open(&path)?;
        reader.execute_batch("BEGIN; SELECT * FROM encrypted_documents;")?;
        let store = RuntimeStore {
            path,
            key: [42; 32],
        };
        let (busy_tx, busy_rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            retry_store_busy(|| {
                let result = store.connection_once();
                if result.is_err() {
                    let _ = busy_tx.try_send(());
                }
                result
            })
        });
        busy_rx.recv_timeout(Duration::from_secs(10))?;
        reader.execute_batch("ROLLBACK")?;
        let connection = worker.join().expect("journal mode worker")?;
        let mode: String = connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        assert_eq!(mode, "wal");
        Ok(())
    }

    #[test]
    fn concurrent_first_opens_publish_one_complete_store() -> Result<()> {
        let dir = test_dir("concurrent-first-opens");
        let ready = std::sync::Arc::new(std::sync::Barrier::new(2));
        let workers = (0..2)
            .map(|index| {
                let dir = dir.clone();
                let ready = std::sync::Arc::clone(&ready);
                std::thread::spawn(move || -> Result<()> {
                    let store = RuntimeStore::open_with_key_at_creation(&dir, [42; 32], || {
                        ready.wait();
                    })?;
                    store.put(&format!("concurrent-{index}"), &index)?;
                    Ok(())
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().expect("initializer thread")?;
        }
        let store = RuntimeStore::open_with_key(&dir, [42; 32])?;
        for index in 0..2 {
            assert_eq!(
                store.get::<u32>(&format!("concurrent-{index}"))?,
                Some(index)
            );
        }
        assert!(fs::read_dir(&dir)?.all(|entry| {
            !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".runtime-init-")
        }));
        Ok(())
    }

    #[test]
    fn concurrent_foreign_creation_is_never_replaced_or_stamped() -> Result<()> {
        let dir = test_dir("foreign-first-open-winner");
        let path = dir.join(DATABASE_FILE);
        let mut before = Vec::new();
        let result = RuntimeStore::open_with_key_at_creation(&dir, [42; 32], || {
            let connection = Connection::open(&path).expect("foreign creator");
            connection.execute_batch("CREATE TABLE foreign_data(value TEXT); INSERT INTO foreign_data VALUES ('preserved');").expect("foreign schema");
            drop(connection);
            before = fs::read(&path).expect("foreign bytes");
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&path)?, before);
        assert!(!dir.join("runtime.sqlite3-wal").exists());
        assert!(!dir.join("runtime.sqlite3-shm").exists());
        assert!(fs::read_dir(&dir)?.all(|entry| {
            !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".runtime-init-")
        }));
        Ok(())
    }

    #[test]
    fn schema_preflight_accepts_exact_legacy_and_reopens_without_stamping() -> Result<()> {
        let dir = test_dir("exact-legacy-schema");
        fs::create_dir_all(&dir)?;
        let path = dir.join(DATABASE_FILE);
        let connection = Connection::open(&path)?;
        // Frozen original physical schema, independent of the current constant.
        connection.execute_batch(
            "CREATE TABLE encrypted_documents (
                namespace TEXT PRIMARY KEY NOT NULL,
                nonce BLOB NOT NULL,
                ciphertext BLOB NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE receipts (
                receipt_id TEXT PRIMARY KEY NOT NULL,
                command_id TEXT NOT NULL,
                nonce BLOB NOT NULL,
                ciphertext BLOB NOT NULL,
                created_at INTEGER NOT NULL
            );",
        )?;
        drop(connection);
        let store = RuntimeStore::open_with_key(&dir, [42; 32])?;
        store.put("legacy-record", &serde_json::json!({"preserve": true}))?;
        drop(store);
        let reopened = RuntimeStore::open_with_key(&dir, [42; 32])?;
        assert_eq!(
            reopened.get::<serde_json::Value>("legacy-record")?,
            Some(serde_json::json!({"preserve": true}))
        );
        let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        for pragma in ["application_id", "user_version"] {
            let value: i64 = connection.pragma_query_value(None, pragma, |row| row.get(0))?;
            assert_eq!(value, 0, "legacy identifiers must remain unchanged");
        }
        Ok(())
    }

    #[test]
    fn schema_preflight_rejects_changed_identity_on_an_existing_store_handle() -> Result<()> {
        let dir = test_dir("schema-reopen");
        let store = RuntimeStore::open_with_key(&dir, [42; 32])?;
        store.put("preserved", &"original")?;
        let connection = Connection::open(store.path())?;
        connection.pragma_update(None, "user_version", 99)?;
        drop(connection);
        let before = fs::read(store.path())?;
        assert!(store.put("preserved", &"changed").is_err());
        assert!(RuntimeStore::open_with_key(&dir, [42; 32]).is_err());
        assert_eq!(fs::read(store.path())?, before);
        Ok(())
    }

    #[test]
    fn schema_preflight_rejects_foreign_future_and_partial_without_mutation() -> Result<()> {
        for (label, sql) in [
            ("foreign", "PRAGMA application_id=1234;"),
            ("future", "PRAGMA user_version=99;"),
            (
                "partial",
                "CREATE TABLE encrypted_documents(namespace TEXT);",
            ),
            ("extra", "CREATE TABLE alien(value TEXT);"),
        ] {
            let dir = test_dir(label);
            fs::create_dir_all(&dir)?;
            let path = dir.join(DATABASE_FILE);
            let connection = Connection::open(&path)?;
            if matches!(label, "foreign" | "future") {
                connection.execute_batch(STORE_SCHEMA)?;
            }
            connection.execute_batch(sql)?;
            drop(connection);
            let before = fs::read(&path)?;
            assert!(
                RuntimeStore::open_with_key(&dir, [42; 32]).is_err(),
                "{label}"
            );
            assert_eq!(
                fs::read(&path)?,
                before,
                "rejected store was changed: {label}"
            );
            assert!(!dir.join("runtime.sqlite3-wal").exists());
            assert!(!dir.join("runtime.sqlite3-shm").exists());
        }
        Ok(())
    }

    #[test]
    fn unit_test_key_selection_does_not_depend_on_the_mutable_data_dir_override() {
        assert!(deterministic_test_store(false));
        assert!(deterministic_test_store(true));
    }

    #[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
    struct SecretDocument {
        values: Vec<String>,
    }

    #[derive(Debug, Deserialize)]
    struct W1PriorStoreFixture {
        schema: String,
        fixture_key_hex: String,
        credential_scope: String,
        physical_schema: String,
        logical_versions: BTreeMap<String, String>,
        import_namespace: String,
        conversation: Conversation,
    }

    #[derive(Debug, Deserialize)]
    struct W1CacheCorruptionFixture {
        schema: String,
        fixture_key_hex: String,
        native_prefix_namespace: String,
        authoritative_namespace: String,
        authoritative_conversation: Conversation,
        tampered_ciphertext_hex: String,
        native_prefix_disposition: String,
    }

    struct RemovePlaintextOnDrop(PathBuf);

    impl Drop for RemovePlaintextOnDrop {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
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
    fn paired_document_mutation_has_one_commit_and_rollback_boundary() -> Result<()> {
        let data_dir = test_dir("paired-documents");
        let store = RuntimeStore::open_with_key(&data_dir, [31_u8; 32])?;
        store.put(
            "first",
            &SecretDocument {
                values: vec!["first-old".to_string()],
            },
        )?;
        store.put(
            "second",
            &SecretDocument {
                values: vec!["second-old".to_string()],
            },
        )?;
        let failed: Result<()> = store.mutate_pair(
            "first",
            SecretDocument::default,
            "second",
            SecretDocument::default,
            |first, second| {
                first.values = vec!["first-rolled-back".to_string()];
                second.values = vec!["second-rolled-back".to_string()];
                Err(anyhow!("force paired rollback"))
            },
        );
        assert!(failed.is_err());
        assert_eq!(
            store.get::<SecretDocument>("first")?.expect("first"),
            SecretDocument {
                values: vec!["first-old".to_string()]
            }
        );
        assert_eq!(
            store.get::<SecretDocument>("second")?.expect("second"),
            SecretDocument {
                values: vec!["second-old".to_string()]
            }
        );

        store.mutate_pair(
            "first",
            SecretDocument::default,
            "second",
            SecretDocument::default,
            |first, second| {
                first.values = vec!["first-new".to_string()];
                second.values = vec!["second-new".to_string()];
                Ok(())
            },
        )?;
        assert_eq!(
            store.get::<SecretDocument>("first")?.expect("first"),
            SecretDocument {
                values: vec!["first-new".to_string()]
            }
        );
        assert_eq!(
            store.get::<SecretDocument>("second")?.expect("second"),
            SecretDocument {
                values: vec!["second-new".to_string()]
            }
        );
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
        store.put_documents_atomically([("blob.old", b"old bytes")])?;

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
    fn exact_effect_receipt_and_terminal_journal_commit_once_or_roll_back_together() -> Result<()> {
        let data_dir = test_dir("exact-effect-receipt");
        let store = RuntimeStore::open_with_key(&data_dir, [44_u8; 32])?;
        store.put(
            "approval-journal",
            &SecretDocument {
                values: vec!["resuming".to_string()],
            },
        )?;
        let receipt = SecretDocument {
            values: vec!["approval-id".to_string(), "call-sha256".to_string()],
        };

        let rolled_back: Result<()> = store.mutate_documents(
            "approval-journal",
            SecretDocument::default,
            |journal, documents| {
                journal.values = vec!["failed-but-rolled-back".to_string()];
                documents.put_receipt("exact-receipt", "mcp-call", &receipt)?;
                Err(anyhow!("inject failure after queued receipt"))
            },
        );
        assert!(rolled_back.is_err());
        assert_eq!(
            store
                .get::<SecretDocument>("approval-journal")?
                .expect("approval journal"),
            SecretDocument {
                values: vec!["resuming".to_string()]
            }
        );
        let receipt_count = |store: &RuntimeStore| -> Result<i64> {
            Ok(store.connection()?.query_row(
                "SELECT COUNT(*) FROM receipts WHERE receipt_id = 'exact-receipt'",
                [],
                |row| row.get(0),
            )?)
        };
        assert_eq!(receipt_count(&store)?, 0);

        store.mutate_documents(
            "approval-journal",
            SecretDocument::default,
            |journal, documents| {
                journal.values = vec!["failed-outcome-unknown".to_string()];
                documents.put_receipt("exact-receipt", "mcp-call", &receipt)?;
                Ok(())
            },
        )?;
        assert_eq!(receipt_count(&store)?, 1);
        assert_eq!(
            store
                .get::<SecretDocument>("approval-journal")?
                .expect("terminal approval journal"),
            SecretDocument {
                values: vec!["failed-outcome-unknown".to_string()]
            }
        );

        let duplicate: Result<()> = store.mutate_documents(
            "approval-journal",
            SecretDocument::default,
            |journal, documents| {
                journal.values = vec!["illegitimate-rewrite".to_string()];
                documents.put_receipt("exact-receipt", "mcp-call", &receipt)?;
                Ok(())
            },
        );
        assert!(duplicate.is_err(), "exact receipt identity is insert-only");
        assert_eq!(receipt_count(&store)?, 1);
        assert_eq!(
            store
                .get::<SecretDocument>("approval-journal")?
                .expect("unchanged terminal journal"),
            SecretDocument {
                values: vec!["failed-outcome-unknown".to_string()]
            }
        );
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
    fn prior_logical_store_import_cleans_plaintext_and_reopens_with_fixture_only_key() -> Result<()>
    {
        let fixture_bytes = include_bytes!("../fixtures/compat/prior-store-v1.json");
        assert_eq!(
            format!("{:x}", Sha256::digest(fixture_bytes)),
            "7e44507f4ee444becf112ed1853e9cfb301618aadac19b1909a41cacf95c6ccf"
        );
        let fixture: W1PriorStoreFixture = serde_json::from_slice(fixture_bytes)?;
        assert_eq!(
            fixture.schema,
            "mom_llama.w1.redacted_logical_store_import_fixture.v1"
        );
        assert_eq!(
            fixture.credential_scope,
            "deterministic_fixture_only_not_personal_keychain"
        );
        assert_eq!(
            fixture.physical_schema,
            "runtime.sqlite3/encrypted_documents.v1"
        );
        assert_eq!(fixture.import_namespace, CONVERSATIONS_NAMESPACE);
        assert_eq!(
            fixture
                .logical_versions
                .get("conversations")
                .map(String::as_str),
            Some(CONVERSATIONS_NAMESPACE)
        );
        assert_eq!(
            fixture.logical_versions.get("drafts").map(String::as_str),
            Some("drafts.v2")
        );
        assert_eq!(
            fixture
                .logical_versions
                .get("attachments")
                .map(String::as_str),
            Some("mom_llama.attachments.v3")
        );
        assert_eq!(
            fixture.logical_versions.get("personas").map(String::as_str),
            Some("personas.v1")
        );

        let data_dir = test_dir("w1-prior-store");
        fs::create_dir_all(&data_dir)?;
        let legacy_path = data_dir.join("conversations.json");
        let _plaintext_cleanup = RemovePlaintextOnDrop(legacy_path.clone());
        let expected = ConversationDb {
            conversations: vec![fixture.conversation],
            selected_conversation_id: Some("mom-w1-prior-store".to_string()),
        };
        fs::write(&legacy_path, serde_json::to_vec_pretty(&expected)?)?;
        let key = decode_hex_key(&fixture.fixture_key_hex)?;
        let store = RuntimeStore::open_with_key(&data_dir, key)?;
        assert!(store.import_json_once::<ConversationDb>(CONVERSATIONS_NAMESPACE, &legacy_path)?);
        assert_eq!(
            store.get::<ConversationDb>(CONVERSATIONS_NAMESPACE)?,
            Some(expected.clone())
        );
        assert!(
            !store.import_json_once::<ConversationDb>(CONVERSATIONS_NAMESPACE, &legacy_path)?,
            "import must be idempotent once the encrypted document exists"
        );
        fs::remove_file(&legacy_path)?;
        assert!(
            !legacy_path.exists(),
            "the plaintext import artifact must not remain beside the encrypted store"
        );

        drop(store);
        let reopened = RuntimeStore::open_with_key(&data_dir, key)?;
        assert_eq!(
            reopened.get::<ConversationDb>(CONVERSATIONS_NAMESPACE)?,
            Some(expected.clone())
        );
        let wrong_key = RuntimeStore::open_with_key(&data_dir, [0x43; 32])?;
        assert!(
            wrong_key
                .get::<ConversationDb>(CONVERSATIONS_NAMESPACE)
                .is_err()
        );
        let database = fs::read(reopened.path())?;
        let redacted_content = expected.conversations[0].messages[0].content.as_bytes();
        assert!(
            !database
                .windows(redacted_content.len())
                .any(|window| window == redacted_content)
        );
        let connection = Connection::open(reopened.path())?;
        let user_version: i64 =
            connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        assert_eq!(user_version, STORE_SCHEMA_VERSION);
        let encrypted_rows: i64 = connection.query_row(
            "SELECT COUNT(*) FROM encrypted_documents WHERE namespace = ?1",
            [CONVERSATIONS_NAMESPACE],
            |row| row.get(0),
        )?;
        assert_eq!(encrypted_rows, 1);

        assert_eq!(
            reopened.get::<ConversationDb>(CONVERSATIONS_NAMESPACE)?,
            Some(expected)
        );
        Ok(())
    }

    #[test]
    fn native_prefix_corruption_quarantines_only_disposable_state_and_reopens_cold() -> Result<()> {
        let fixture: W1CacheCorruptionFixture =
            serde_json::from_str(include_str!("../fixtures/compat/cache-corruption-v1.json"))?;
        assert_eq!(
            fixture.schema,
            "mom_llama.w1.disposable_cache_corruption_fixture.v1"
        );
        assert_eq!(
            fixture.native_prefix_disposition,
            "quarantine_then_cold_miss"
        );
        let data_dir = test_dir("w1-native-prefix-corruption");
        let key = decode_hex_key(&fixture.fixture_key_hex)?;
        let store = RuntimeStore::open_with_key(&data_dir, key)?;
        let native_prefix: Vec<PrefixCacheValue> = serde_json::from_slice(include_bytes!(
            "../fixtures/compat/cache-native-prefix-state-v1.json"
        ))?;
        store.put(&fixture.native_prefix_namespace, &native_prefix)?;
        let authoritative = ConversationDb {
            selected_conversation_id: Some(fixture.authoritative_conversation.id.clone()),
            conversations: vec![fixture.authoritative_conversation],
        };
        store.put(&fixture.authoritative_namespace, &authoritative)?;
        assert_eq!(fixture.tampered_ciphertext_hex, "00");
        let tampered = vec![0_u8];
        Connection::open(store.path())?.execute(
            "UPDATE encrypted_documents SET ciphertext = ?1 WHERE namespace = ?2",
            params![tampered, fixture.native_prefix_namespace],
        )?;

        assert_eq!(
            store
                .get_disposable_cache::<Vec<PrefixCacheValue>>(&fixture.native_prefix_namespace)?,
            None
        );
        assert_eq!(
            store.get::<ConversationDb>(&fixture.authoritative_namespace)?,
            Some(authoritative.clone())
        );
        drop(store);
        let reopened = RuntimeStore::open_with_key(&data_dir, key)?;
        assert_eq!(
            reopened
                .get_disposable_cache::<Vec<PrefixCacheValue>>(&fixture.native_prefix_namespace)?,
            None
        );
        assert_eq!(
            reopened.get::<ConversationDb>(&fixture.authoritative_namespace)?,
            Some(authoritative)
        );
        let connection = Connection::open(reopened.path())?;
        let quarantine_rows: i64 = connection.query_row(
            "SELECT COUNT(*) FROM encrypted_documents WHERE namespace LIKE 'quarantine.disposable-cache.%'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(quarantine_rows, 1);
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
    fn denied_installation_key_retries_only_after_explicit_cache_clear() -> Result<()> {
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
        assert!(clear_cached_installation_key_failure(
            "denied-account",
            &cache
        )?);
        let key = cached_installation_key("denied-account", &cache, || {
            calls.set(calls.get() + 1);
            Ok([31_u8; 32])
        })?;
        assert_eq!(key, [31_u8; 32]);
        assert_eq!(calls.get(), 2);
        assert!(
            !clear_cached_installation_key_failure("denied-account", &cache)?,
            "a successful cached key must never be evicted by retry preparation"
        );
        Ok(())
    }
}
