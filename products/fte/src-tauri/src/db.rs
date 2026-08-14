use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

const MAX_REQUEST_LOG_ROWS: i64 = 10_000;
const APPLICATION_ID: i64 = 0x4654_4531;
const SCHEMA_VERSION: i64 = 1;
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
        "CREATE TABLE request_log (id INTEGER PRIMARY KEY AUTOINCREMENT, timestamp DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP, provider_id TEXT NOT NULL, model_id TEXT NOT NULL, tokens_used INTEGER NOT NULL CHECK(tokens_used >= 0), latency_ms INTEGER NOT NULL CHECK(latency_ms >= 0), status_code INTEGER NOT NULL)",
    ),
];

#[derive(Debug, serde::Serialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub provider_id: String,
    pub model_id: String,
    pub tokens_used: u64,
    pub latency_ms: u64,
    pub status_code: i32,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct ProviderLogSummary {
    pub total_tokens: u64,
    pub avg_latency_ms: u64,
    pub request_count: u64,
    pub last_request_at: Option<String>,
    pub last_status_code: Option<i32>,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct GlobalLogSummary {
    pub total_tokens: u64,
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
        if state == DatabaseState::Fresh {
            db.init_schema()?;
        }
        Ok(db)
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow!("database lock was poisoned"))
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
                tokens_used INTEGER NOT NULL CHECK(tokens_used >= 0),
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
            PRAGMA user_version = 1;
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
                tokens_used: nonnegative_u64(row.get(3)?, 3)?,
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
                COUNT(*)
            FROM request_log
            ",
        )?;

        stmt.query_row([], |row| {
            let avg_latency: f64 = row.get(1)?;
            Ok(GlobalLogSummary {
                total_tokens: nonnegative_u64(row.get(0)?, 0)?,
                avg_latency_ms: avg_latency.max(0.0).round() as u64,
                request_count: nonnegative_u64(row.get(2)?, 2)?,
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
                )
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
        tokens: u32,
        latency: u64,
        status: i32,
    ) -> Result<()> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
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
    fn log_summaries_report_latest_status_and_real_aggregates() {
        let db = Database::new(test_database_path("summaries")).unwrap();
        db.log_request("provider", "model-a", 10, 100, 200).unwrap();
        db.log_request("provider", "model-b", 20, 300, 503).unwrap();

        let global = db.get_global_log_summary().unwrap();
        assert_eq!(global.total_tokens, 30);
        assert_eq!(global.avg_latency_ms, 200);
        assert_eq!(global.request_count, 2);

        let providers = db.get_provider_log_summaries().unwrap();
        let provider = providers.get("provider").unwrap();
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
            let db = Database::new(path.clone()).unwrap();
            db.save_local_model_configuration(&configuration).unwrap();
        }

        let reopened = Database::new(path).unwrap();
        assert_eq!(
            reopened.get_local_model_configuration().unwrap(),
            Some(configuration)
        );
    }

    #[test]
    fn fresh_database_is_versioned_and_reopens_only_as_the_current_schema() {
        let path = test_database_path("current-schema");
        {
            let db = Database::new(path.clone()).unwrap();
            let conn = db.connection().unwrap();
            assert_eq!(
                conn.query_row("PRAGMA application_id", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                APPLICATION_ID
            );
            assert_eq!(
                conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                SCHEMA_VERSION
            );
        }
        Database::new(path).expect("exact current database reopens");
    }

    #[test]
    fn synthetic_prohibited_plaintext_table_is_rejected_without_import_or_mutation() {
        let path = test_database_path("prohibited-plaintext-sentinel");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE api_keys (
                    provider_id TEXT PRIMARY KEY,
                    key_value TEXT NOT NULL
                );",
            )
            .unwrap();
        }
        let before = std::fs::read(&path).unwrap();

        let error = match Database::new(path.clone()) {
            Ok(_) => panic!("legacy schema must fail closed"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("unsupported legacy database"));
        assert!(error.to_string().contains("not imported"));
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn unversioned_populated_database_is_rejected_without_schema_adoption() {
        let path = test_database_path("unversioned-populated");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE operator_data (value TEXT NOT NULL);")
                .unwrap();
        }
        let before = std::fs::read(&path).unwrap();

        let error = match Database::new(path.clone()) {
            Ok(_) => panic!("unversioned populated database must fail closed"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("unsupported database"));
        assert!(error.to_string().contains("not imported"));
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn wrong_version_or_unexpected_schema_object_is_rejected() {
        for (label, mutation) in [
            ("future-version", "PRAGMA user_version = 2;"),
            ("unexpected-table", "CREATE TABLE unexpected(value TEXT);"),
            (
                "unexpected-view",
                "CREATE VIEW unexpected_view AS SELECT 1 AS value;",
            ),
        ] {
            let path = test_database_path(label);
            {
                let db = Database::new(path.clone()).unwrap();
                db.connection().unwrap().execute_batch(mutation).unwrap();
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
            let db = Database::new(path.clone()).unwrap();
            db.connection()
                .unwrap()
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
                .unwrap();
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
                .unwrap()
                .as_nanos()
        ))
    }
}
