use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

const MAX_REQUEST_LOG_ROWS: i64 = 10_000;
const APPLICATION_ID: i64 = 0x4654_4531;
const SCHEMA_VERSION: i64 = 2;
const CURRENT_SCHEMA_OBJECTS: [(&str, &str, &str); 4] = [
    (
        "index",
        "idx_request_log_provider",
        "CREATE INDEX idx_request_log_provider ON request_log (provider_id, id DESC)",
    ),
    (
        "table",
        "local_model_configuration",
        "CREATE TABLE local_model_configuration (slot INTEGER PRIMARY KEY CHECK(slot = 1), model_path TEXT NOT NULL CHECK(length(model_path) > 0), expected_sha256 TEXT)",
    ),
    (
        "table",
        "master_profile",
        "CREATE TABLE master_profile (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
    ),
    (
        "table",
        "request_log",
        "CREATE TABLE request_log (id INTEGER PRIMARY KEY AUTOINCREMENT, timestamp DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP, provider_id TEXT NOT NULL, model_id TEXT NOT NULL, tokens_used INTEGER CHECK(tokens_used >= 0), latency_ms INTEGER NOT NULL CHECK(latency_ms >= 0), status_code INTEGER NOT NULL)",
    ),
];

#[derive(Debug, serde::Serialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub provider_id: String,
    pub model_id: String,
    pub tokens_used: Option<u64>,
    pub latency_ms: u64,
    pub status_code: i32,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct ProviderLogSummary {
    pub total_tokens: u64,
    pub unknown_usage_requests: u64,
    pub avg_latency_ms: u64,
    pub request_count: u64,
    pub last_request_at: Option<String>,
    pub last_status_code: Option<i32>,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct GlobalLogSummary {
    pub total_tokens: u64,
    pub unknown_usage_requests: u64,
    pub avg_latency_ms: u64,
    pub request_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalModelConfiguration {
    pub model_path: String,
    pub expected_sha256: Option<String>,
}

pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    pub fn new(db_path: PathBuf) -> Result<Self> {
        create_parent_if_missing(&db_path)?;
        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open database at {}", db_path.display()))?;
        harden_file_permissions(&db_path)?;

        conn.pragma_update(None, "trusted_schema", false)?;
        let state = classify_database(&conn, &db_path)?;

        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "
            PRAGMA foreign_keys = ON;
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA secure_delete = ON;
            ",
        )?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        match state {
            DatabaseState::Fresh => db.init_schema()?,
            DatabaseState::VersionOne => db.upgrade_version_one(&db_path)?,
            DatabaseState::Current => {}
        }
        Ok(db)
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow!("database lock was poisoned"))
    }

    fn upgrade_version_one(&self, path: &Path) -> Result<()> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        match classify_database(&tx, path)? {
            DatabaseState::Current => return Ok(()),
            DatabaseState::VersionOne => {}
            DatabaseState::Fresh => {
                anyhow::bail!("database identity changed before the v1 upgrade")
            }
        }
        tx.execute_batch(
            "ALTER TABLE request_log RENAME TO request_log_v1;
            DROP INDEX idx_request_log_provider;",
        )?;
        tx.execute_batch(CURRENT_SCHEMA_OBJECTS[3].2)?;
        tx.execute_batch(
            "INSERT INTO request_log SELECT * FROM request_log_v1;
            DROP TABLE request_log_v1;",
        )?;
        tx.execute_batch(CURRENT_SCHEMA_OBJECTS[0].2)?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.commit()?;
        Ok(())
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.connection()?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS request_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
                provider_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                tokens_used INTEGER CHECK(tokens_used >= 0),
                latency_ms INTEGER NOT NULL CHECK(latency_ms >= 0),
                status_code INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_request_log_provider
                ON request_log (provider_id, id DESC);

            CREATE TABLE IF NOT EXISTS master_profile (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS local_model_configuration (
                slot INTEGER PRIMARY KEY CHECK(slot = 1),
                model_path TEXT NOT NULL CHECK(length(model_path) > 0),
                expected_sha256 TEXT
            );

            PRAGMA application_id = 0x46544531;
            PRAGMA user_version = 2;
            ",
        )?;
        Ok(())
    }

    pub fn save_profile_field(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.connection()?;
        if value.is_empty() {
            conn.execute("DELETE FROM master_profile WHERE key = ?1", params![key])?;
        } else {
            conn.execute(
                "INSERT INTO master_profile (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )?;
        }
        Ok(())
    }

    pub fn get_profile_field(&self, key: &str) -> Result<Option<String>> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT value FROM master_profile WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn get_master_profile(&self) -> Result<HashMap<String, String>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT key, value FROM master_profile ORDER BY key")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let pairs = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(pairs.into_iter().collect())
    }

    pub fn save_local_model_configuration(
        &self,
        configuration: &LocalModelConfiguration,
    ) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO local_model_configuration (slot, model_path, expected_sha256)
             VALUES (1, ?1, ?2)
             ON CONFLICT(slot) DO UPDATE SET
                 model_path=excluded.model_path,
                 expected_sha256=excluded.expected_sha256",
            params![
                &configuration.model_path,
                configuration.expected_sha256.as_deref()
            ],
        )?;
        Ok(())
    }

    pub fn get_local_model_configuration(&self) -> Result<Option<LocalModelConfiguration>> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT model_path, expected_sha256
             FROM local_model_configuration
             WHERE slot = 1",
            [],
            |row| {
                Ok(LocalModelConfiguration {
                    model_path: row.get(0)?,
                    expected_sha256: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn delete_local_model_configuration(&self) -> Result<()> {
        let conn = self.connection()?;
        conn.execute("DELETE FROM local_model_configuration WHERE slot = 1", [])?;
        Ok(())
    }

    pub fn get_recent_logs(&self, limit: u32) -> Result<Vec<LogEntry>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT timestamp, provider_id, model_id, tokens_used, latency_ms, status_code
             FROM request_log
             ORDER BY id DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit.clamp(1, 500)], |row| {
            Ok(LogEntry {
                timestamp: row.get(0)?,
                provider_id: row.get(1)?,
                model_id: row.get(2)?,
                tokens_used: row
                    .get::<_, Option<i64>>(3)?
                    .map(|value| nonnegative_u64(value, 3))
                    .transpose()?,
                latency_ms: nonnegative_u64(row.get(4)?, 4)?,
                status_code: row.get(5)?,
            })
        })?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn get_global_log_summary(&self) -> Result<GlobalLogSummary> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "
            SELECT
                COALESCE(SUM(tokens_used), 0),
                COALESCE(AVG(latency_ms), 0),
                COUNT(*),
                COUNT(*) - COUNT(tokens_used)
            FROM request_log
            ",
        )?;

        stmt.query_row([], |row| {
            let avg_latency: f64 = row.get(1)?;
            Ok(GlobalLogSummary {
                total_tokens: nonnegative_u64(row.get(0)?, 0)?,
                avg_latency_ms: avg_latency.max(0.0).round() as u64,
                request_count: nonnegative_u64(row.get(2)?, 2)?,
                unknown_usage_requests: nonnegative_u64(row.get(3)?, 3)?,
            })
        })
        .map_err(Into::into)
    }

    pub fn get_provider_log_summaries(&self) -> Result<HashMap<String, ProviderLogSummary>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "
            SELECT
                rl.provider_id,
                COALESCE(SUM(rl.tokens_used), 0),
                COALESCE(AVG(rl.latency_ms), 0),
                COUNT(*),
                MAX(rl.timestamp),
                (
                    SELECT r2.status_code
                    FROM request_log r2
                    WHERE r2.provider_id = rl.provider_id
                    ORDER BY r2.id DESC
                    LIMIT 1
                ),
                COUNT(*) - COUNT(rl.tokens_used)
            FROM request_log rl
            GROUP BY rl.provider_id
            ",
        )?;

        let rows = stmt.query_map([], |row| {
            let avg_latency: f64 = row.get(2)?;
            Ok((
                row.get::<_, String>(0)?,
                ProviderLogSummary {
                    total_tokens: nonnegative_u64(row.get(1)?, 1)?,
                    avg_latency_ms: avg_latency.max(0.0).round() as u64,
                    request_count: nonnegative_u64(row.get(3)?, 3)?,
                    last_request_at: row.get(4)?,
                    last_status_code: row.get(5)?,
                    unknown_usage_requests: nonnegative_u64(row.get(6)?, 6)?,
                },
            ))
        })?;

        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect())
    }

    pub fn log_request(
        &self,
        provider: &str,
        model: &str,
        tokens: impl Into<Option<u32>>,
        latency: u64,
        status: i32,
    ) -> Result<()> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        let tokens = tokens.into();
        let latency = i64::try_from(latency).unwrap_or(i64::MAX);
        tx.execute(
            "INSERT INTO request_log
                (provider_id, model_id, tokens_used, latency_ms, status_code)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![provider, model, tokens, latency, status],
        )?;
        tx.execute(
            "DELETE FROM request_log
             WHERE id < (
                 SELECT id FROM request_log
                 ORDER BY id DESC
                 LIMIT 1 OFFSET ?1
             )",
            params![MAX_REQUEST_LOG_ROWS - 1],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DatabaseState {
    Fresh,
    Current,
    VersionOne,
}

fn classify_database(conn: &Connection, path: &Path) -> Result<DatabaseState> {
    let application_id = conn.query_row("PRAGMA application_id", [], |row| row.get::<_, i64>(0))?;
    let schema_version = conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
    let mut statement = conn.prepare(
        "SELECT type, name, sql FROM sqlite_schema
         WHERE name NOT LIKE 'sqlite_%'
         ORDER BY type, name",
    )?;
    let schema_objects = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;

    if application_id == 0 && schema_version == 0 && schema_objects.is_empty() {
        return Ok(DatabaseState::Fresh);
    }

    if schema_objects
        .iter()
        .any(|(kind, name, _)| kind == "table" && name == "api_keys")
    {
        anyhow::bail!(
            "unsupported legacy database at {}: plaintext api_keys storage is not imported; move or remove this database and start with a fresh FTE store",
            path.display()
        );
    }

    let expected_schema_objects = CURRENT_SCHEMA_OBJECTS
        .into_iter()
        .map(|(kind, name, sql)| {
            (
                kind.to_owned(),
                name.to_owned(),
                Some(normalize_schema_sql(sql)),
            )
        })
        .collect::<BTreeSet<_>>();
    let schema_objects = schema_objects
        .into_iter()
        .map(|(kind, name, sql)| (kind, name, sql.map(|sql| normalize_schema_sql(&sql))))
        .collect::<BTreeSet<_>>();
    let version_one_schema_objects = CURRENT_SCHEMA_OBJECTS
        .into_iter()
        .map(|(kind, name, sql)| {
            (
                kind.to_owned(),
                name.to_owned(),
                Some(normalize_schema_sql(&sql.replace(
                    "tokens_used INTEGER CHECK",
                    "tokens_used INTEGER NOT NULL CHECK",
                ))),
            )
        })
        .collect::<BTreeSet<_>>();
    if application_id == APPLICATION_ID
        && schema_version == 1
        && schema_objects == version_one_schema_objects
    {
        return Ok(DatabaseState::VersionOne);
    }
    if application_id == APPLICATION_ID
        && schema_version == SCHEMA_VERSION
        && schema_objects == expected_schema_objects
    {
        return Ok(DatabaseState::Current);
    }

    anyhow::bail!(
        "unsupported database at {}: expected FTE application_id {APPLICATION_ID:#x}, schema version {SCHEMA_VERSION}, and the exact current schema object set; legacy and foreign databases are not imported",
        path.display()
    )
}

fn normalize_schema_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn nonnegative_u64(value: i64, column: usize) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn create_parent_if_missing(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        harden_directory_permissions(parent)?;
    }
    Ok(())
}

#[cfg(unix)]
fn harden_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to secure {}", path.display()))
}

#[cfg(not(unix))]
fn harden_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn harden_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to secure {}", path.display()))
}

#[cfg(not(unix))]
fn harden_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DATABASE_ID: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn stale_v1_preflight_accepts_an_exact_upgrade_committed_by_another_opener() {
        let path = test_database_path("two-upgraders");
        let connection = Connection::open(&path).expect("first preflight connection");
        for (_, _, sql) in CURRENT_SCHEMA_OBJECTS
            .iter()
            .filter(|(kind, _, _)| *kind == "table")
        {
            connection
                .execute_batch(&sql.replace(
                    "tokens_used INTEGER CHECK",
                    "tokens_used INTEGER NOT NULL CHECK",
                ))
                .expect("v1 schema");
        }
        connection
            .execute_batch(CURRENT_SCHEMA_OBJECTS[0].2)
            .expect("index");
        connection
            .pragma_update(None, "application_id", APPLICATION_ID)
            .expect("app identity");
        connection
            .pragma_update(None, "user_version", 1)
            .expect("v1 identity");
        connection
            .execute_batch("INSERT INTO master_profile VALUES ('name', 'preserved');")
            .expect("profile");
        assert_eq!(
            classify_database(&connection, &path).expect("initial preflight"),
            DatabaseState::VersionOne
        );
        let stale_opener = Database {
            conn: Arc::new(Mutex::new(connection)),
        };
        // The first opener is paused after classifying v1. Another opener wins
        // the write transaction and completes the identical supported upgrade.
        let winner = Database::new(path.clone()).expect("other opener upgrades");
        stale_opener
            .upgrade_version_one(&path)
            .expect("already-upgraded exact identity is success");
        assert_eq!(
            classify_database(&winner.connection().expect("winner connection"), &path)
                .expect("current schema"),
            DatabaseState::Current
        );
        assert_eq!(
            winner
                .connection()
                .expect("winner connection")
                .query_row(
                    "SELECT value FROM master_profile WHERE key='name'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .expect("profile retained"),
            "preserved"
        );
        for (pragma, value) in [("user_version", 99), ("application_id", 42)] {
            {
                let conn = winner.connection().expect("winner connection");
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)
                    .expect("restore supported version");
                conn.pragma_update(None, pragma, value)
                    .expect("concurrent identity change");
            }
            stale_opener
                .upgrade_version_one(&path)
                .expect_err("stale opener must not accept foreign/future identity");
            let actual = winner
                .connection()
                .expect("winner connection")
                .query_row(&format!("PRAGMA {pragma}"), [], |row| row.get::<_, i64>(0))
                .expect("identity retained");
            assert_eq!(actual, value);
        }
    }

    #[test]
    fn exact_version_one_upgrade_preserves_rows_and_allows_unknown_usage_on_reopen() {
        let path = test_database_path("exact-v1-upgrade");
        {
            let conn = Connection::open(&path).expect("v1 connection");
            // Construct only the exact supported prior application schema.
            for (_, _, sql) in CURRENT_SCHEMA_OBJECTS
                .iter()
                .filter(|(kind, _, _)| *kind == "table")
            {
                conn.execute_batch(&sql.replace(
                    "tokens_used INTEGER CHECK",
                    "tokens_used INTEGER NOT NULL CHECK",
                ))
                .expect("v1 tables");
            }
            conn.execute_batch(CURRENT_SCHEMA_OBJECTS[0].2)
                .expect("v1 index");
            conn.pragma_update(None, "application_id", APPLICATION_ID)
                .expect("app id");
            conn.pragma_update(None, "user_version", 1)
                .expect("version");
            conn.execute_batch("INSERT INTO request_log (provider_id, model_id, tokens_used, latency_ms, status_code)
                VALUES ('old', 'zero', 0, 3, 200), ('old', 'known', 7, 4, 200);
                INSERT INTO master_profile VALUES ('name', 'preserved');").expect("v1 rows");
        }
        let database = Database::new(path.clone()).expect("upgrade exact v1");
        let logs = database.get_recent_logs(10).expect("preserved logs");
        assert_eq!(logs[0].tokens_used, Some(7));
        assert_eq!(logs[1].tokens_used, Some(0));
        database
            .log_request("new", "unknown", None, 5, 499)
            .expect("unknown usage");
        drop(database);
        let database = Database::new(path.clone()).expect("v2 reopen");
        let logs = database.get_recent_logs(10).expect("reopened logs");
        assert_eq!(logs.len(), 3);
        assert_eq!(logs[0].tokens_used, None);
        assert_eq!(logs[2].tokens_used, Some(0));
        let conn = database.connection().expect("connection");
        assert_eq!(
            conn.query_row(
                "SELECT value FROM master_profile WHERE key='name'",
                [],
                |row| row.get::<_, String>(0)
            )
            .expect("profile"),
            "preserved"
        );
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("version"),
            2
        );
    }

    #[test]
    fn log_summaries_report_latest_status_and_real_aggregates() {
        let db = Database::new(test_database_path("summaries"))
            .expect("log_summaries_report_latest_status_and_real_aggregates: expected success");
        db.log_request("provider", "model-a", 10, 100, 200)
            .expect("log_summaries_report_latest_status_and_real_aggregates: expected success");
        db.log_request("provider", "model-b", 20, 300, 503)
            .expect("log_summaries_report_latest_status_and_real_aggregates: expected success");

        let global = db
            .get_global_log_summary()
            .expect("log_summaries_report_latest_status_and_real_aggregates: expected success");
        assert_eq!(global.total_tokens, 30);
        assert_eq!(global.avg_latency_ms, 200);
        assert_eq!(global.request_count, 2);

        let providers = db
            .get_provider_log_summaries()
            .expect("log_summaries_report_latest_status_and_real_aggregates: expected success");
        let provider = providers
            .get("provider")
            .expect("log_summaries_report_latest_status_and_real_aggregates: expected success");
        assert_eq!(provider.total_tokens, 30);
        assert_eq!(provider.avg_latency_ms, 200);
        assert_eq!(provider.request_count, 2);
        assert_eq!(provider.last_status_code, Some(503));
    }

    #[test]
    fn local_model_configuration_survives_database_reopen() {
        let path = test_database_path("local-model-reopen");
        let configuration = LocalModelConfiguration {
            model_path: "/private/models/local.gguf".to_string(),
            expected_sha256: Some("a".repeat(64)),
        };
        {
            let db = Database::new(path.clone())
                .expect("local_model_configuration_survives_database_reopen: expected success");
            db.save_local_model_configuration(&configuration)
                .expect("local_model_configuration_survives_database_reopen: expected success");
        }

        let reopened = Database::new(path)
            .expect("local_model_configuration_survives_database_reopen: expected success");
        assert_eq!(
            reopened
                .get_local_model_configuration()
                .expect("local_model_configuration_survives_database_reopen: expected success"),
            Some(configuration)
        );
    }

    #[test]
    fn fresh_database_is_versioned_and_reopens_only_as_the_current_schema() {
        let path = test_database_path("current-schema");
        {
            let db = Database::new(path.clone()).expect("fresh_database_is_versioned_and_reopens_only_as_the_current_schema: expected success");
            let conn = db.connection().expect("fresh_database_is_versioned_and_reopens_only_as_the_current_schema: expected success");
            assert_eq!(
                conn.query_row("PRAGMA application_id", [], |row| row.get::<_, i64>(0))
                    .expect("fresh_database_is_versioned_and_reopens_only_as_the_current_schema: expected success"),
                APPLICATION_ID
            );
            assert_eq!(
                conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .expect("fresh_database_is_versioned_and_reopens_only_as_the_current_schema: expected success"),
                SCHEMA_VERSION
            );
        }
        Database::new(path).expect("exact current database reopens");
    }

    #[test]
    fn synthetic_prohibited_plaintext_table_is_rejected_without_import_or_mutation() {
        let path = test_database_path("prohibited-plaintext-sentinel");
        {
            let conn = Connection::open(&path).expect("synthetic_prohibited_plaintext_table_is_rejected_without_import_or_mutation: expected success");
            conn.execute_batch(
                "CREATE TABLE api_keys (
                    provider_id TEXT PRIMARY KEY,
                    key_value TEXT NOT NULL
                );",
            )
            .expect("synthetic_prohibited_plaintext_table_is_rejected_without_import_or_mutation: expected success");
        }
        let before = std::fs::read(&path).expect("synthetic_prohibited_plaintext_table_is_rejected_without_import_or_mutation: expected success");

        let error = match Database::new(path.clone()) {
            Ok(_) => panic!("legacy schema must fail closed"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("unsupported legacy database"));
        assert!(error.to_string().contains("not imported"));
        assert_eq!(std::fs::read(path).expect("synthetic_prohibited_plaintext_table_is_rejected_without_import_or_mutation: expected success"), before);
    }

    #[test]
    fn unversioned_populated_database_is_rejected_without_schema_adoption() {
        let path = test_database_path("unversioned-populated");
        {
            let conn = Connection::open(&path).expect("unversioned_populated_database_is_rejected_without_schema_adoption: expected success");
            conn.execute_batch("CREATE TABLE operator_data (value TEXT NOT NULL);")
                .expect("unversioned_populated_database_is_rejected_without_schema_adoption: expected success");
        }
        let before = std::fs::read(&path).expect(
            "unversioned_populated_database_is_rejected_without_schema_adoption: expected success",
        );

        let error = match Database::new(path.clone()) {
            Ok(_) => panic!("unversioned populated database must fail closed"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("unsupported database"));
        assert!(error.to_string().contains("not imported"));
        assert_eq!(std::fs::read(path).expect("unversioned_populated_database_is_rejected_without_schema_adoption: expected success"), before);
    }

    #[test]
    fn wrong_version_or_unexpected_schema_object_is_rejected() {
        for (label, mutation) in [
            ("future-version", "PRAGMA user_version = 3;"),
            ("unexpected-table", "CREATE TABLE unexpected(value TEXT);"),
            (
                "unexpected-view",
                "CREATE VIEW unexpected_view AS SELECT 1 AS value;",
            ),
        ] {
            let path = test_database_path(label);
            {
                let db = Database::new(path.clone()).expect(
                    "wrong_version_or_unexpected_schema_object_is_rejected: expected success",
                );
                db.connection()
                    .expect(
                        "wrong_version_or_unexpected_schema_object_is_rejected: expected success",
                    )
                    .execute_batch(mutation)
                    .expect(
                        "wrong_version_or_unexpected_schema_object_is_rejected: expected success",
                    );
            }

            let error = match Database::new(path) {
                Ok(_) => panic!("unsupported current-schema mutation must fail closed"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("unsupported database"));
        }
    }

    #[test]
    fn same_schema_names_with_wrong_definitions_are_rejected() {
        let path = test_database_path("same-names-wrong-definitions");
        {
            let db = Database::new(path.clone())
                .expect("same_schema_names_with_wrong_definitions_are_rejected: expected success");
            db.connection()
                .expect("same_schema_names_with_wrong_definitions_are_rejected: expected success")
                .execute_batch(
                    "DROP INDEX idx_request_log_provider;
                     DROP TABLE request_log;
                     CREATE TABLE request_log (
                         id INTEGER PRIMARY KEY,
                         provider_id TEXT NOT NULL
                     );
                     CREATE INDEX idx_request_log_provider
                         ON request_log (provider_id, id DESC);",
                )
                .expect("same_schema_names_with_wrong_definitions_are_rejected: expected success");
        }

        let error = match Database::new(path) {
            Ok(_) => panic!("same-name foreign schema must fail closed"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("unsupported database"));
    }

    fn test_database_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "free-token-energy-db-{label}-{}-{}-{}.sqlite",
            std::process::id(),
            TEST_DATABASE_ID.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("test_database_path: expected success")
                .as_nanos()
        ))
    }
}
