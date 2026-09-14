//! Signal's database and send ledger share one SQLCipher key held by the OS vault.
use std::{
    fs::{File, OpenOptions},
    path::Path,
};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use presage_store_sqlite::{OnNewIdentity, SqliteConnectOptions, SqliteStore};
use sha2::{Digest, Sha256};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};

pub struct Vault {
    pub store: SqliteStore,
    pub database: SqlitePool,
    protocol_database: SqlitePool,
    _lease: File,
}

impl Vault {
    pub async fn open(directory: &Path) -> Result<Self> {
        anyhow::ensure!(
            directory.is_absolute(),
            "Signal storage must have an absolute path"
        );
        std::fs::create_dir_all(directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        }
        let directory = directory.canonicalize()?;
        let lease = private_file(&directory.join("client.lock"), false)?;
        lease
            .try_lock_exclusive()
            .context("Signal is already running for this profile")?;
        let path = directory.join("signal.db");
        let exists = path.exists();
        let account = hex::encode(Sha256::digest(directory.as_os_str().as_encoded_bytes()));
        let entry = keyring::Entry::new("app.delysis.loom.signal", &account)?;
        let passphrase = match entry.get_password() {
            Ok(value) => value,
            Err(keyring::Error::NoEntry) if !exists => {
                let mut key = [0_u8; 32];
                getrandom::fill(&mut key)
                    .map_err(|_| anyhow::anyhow!("OS randomness unavailable"))?;
                let value = hex::encode(key);
                entry.set_password(&value)?;
                value
            }
            Err(error) => {
                return Err(error)
                    .context("Signal encryption key unavailable; storage was preserved");
            }
        };
        if passphrase.len() != 64 || !passphrase.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("Signal encryption key has an invalid format");
        }
        drop(private_file(&path, false)?);
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .pragma("key", format!("'{passphrase}'"))
            .pragma("cipher_memory_security", "ON")
            .pragma("secure_delete", "ON")
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true);
        let database = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await?;
        let protocol_database = match SqlitePoolOptions::new()
            .max_connections(10)
            .connect_with(options)
            .await
        {
            Ok(pool) => pool,
            Err(error) => {
                database.close().await;
                return Err(error.into());
            }
        };
        let initialized = initialize_store(&database, &protocol_database).await;
        let store = match initialized {
            Ok(store) => store,
            Err(error) => {
                protocol_database.close().await;
                database.close().await;
                return Err(error);
            }
        };
        Ok(Self {
            store,
            database,
            protocol_database,
            _lease: lease,
        })
    }

    /// SQLx owns native SQLite threads. Join both pools before process exit;
    /// simply dropping their handles leaves SQLCipher cleanup running detached.
    pub async fn close(&self) {
        self.protocol_database.close().await;
        self.database.close().await;
    }
}

async fn initialize_store(
    database: &SqlitePool,
    protocol_database: &SqlitePool,
) -> Result<SqliteStore> {
    let cipher: Option<String> = sqlx::query_scalar("PRAGMA cipher_version")
        .fetch_optional(database)
        .await?;
    anyhow::ensure!(
        cipher.is_some_and(|value| !value.is_empty()),
        "SQLCipher unavailable"
    );
    // Presage trusts an identity on first use. Changed identities remain
    // rejected; the UI must not silently replace them to restore a session.
    let store =
        SqliteStore::open_with_pool(protocol_database.clone(), OnNewIdentity::Reject).await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS loom_send_v1 (id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('uncertain', 'sent')), conversation TEXT NOT NULL, timestamp INTEGER NOT NULL, UNIQUE(conversation, timestamp))")
    .execute(database).await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS loom_retention_v1 (thread_id INTEGER NOT NULL, ts INTEGER NOT NULL, expires_at INTEGER, PRIMARY KEY(thread_id, ts))").execute(database).await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS loom_retention_expiry_v1 ON loom_retention_v1(expires_at)",
    )
    .execute(database)
    .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS loom_drafts_v1(conversation TEXT PRIMARY KEY, command TEXT NOT NULL, body TEXT NOT NULL)").execute(database).await?;
    crate::workspaces::initialize(database).await?;
    crate::identity::initialize(database).await?;
    Ok(store)
}

fn private_file(path: &Path, exclusive: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(!exclusive)
        .create_new(exclusive);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        bail!("Signal storage cannot be a symbolic link");
    }
    Ok(options.open(path)?)
}
