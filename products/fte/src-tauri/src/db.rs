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

#[cfg(all(test, feature = "unstable-w1-vertical-tests"))]
mod w1_tests {
    use super::*;
    use platform_vertical_fixtures_v0::{
        EquivalenceProjectionV0, ObservationEnvelopeV0, VerticalFixtureManifestV0, sha256_identity,
        validate_baseline, validate_manifest,
    };
    use serde::Deserialize;
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};

    const BASELINE_COMMIT: &str = "797500060047ccd10f9810fb4d5c8f374e00eb08";
    const MANIFEST_BYTES: &[u8] =
        include_bytes!("../../tests/fixtures/w1/v0/fte-database-rejection.manifest.json");
    const POLICY_BYTES: &[u8] =
        include_bytes!("../../tests/fixtures/w1/v0/fte-database-policy-v1.json");
    const PROJECTION_BYTES: &[u8] =
        include_bytes!("../../tests/fixtures/w1/v0/fte-database-rejection-projection.json");
    const SOURCE_BYTES: &[u8] =
        include_bytes!("../../tests/fixtures/w1/v0/fte-database-production-tree-d022e36.json");
    const SUPERSEDED_RECEIPT_BYTES: &[u8] =
        include_bytes!("../../receipts/R7-FTE-OS-CREDENTIAL-ACCEPTANCE.json");
    static W1_DATABASE_ID: AtomicU64 = AtomicU64::new(1);

    #[derive(Deserialize)]
    struct SourceDescriptor {
        schema: String,
        repository_id: String,
        commit: String,
        prefixes: Vec<SourcePrefix>,
        git_blobs: BTreeMap<String, String>,
        absent_paths: Vec<String>,
    }

    #[derive(Deserialize)]
    struct SourcePrefix {
        path: String,
        boundary: String,
        sha256: String,
        byte_len: u64,
    }

    fn repository_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .to_owned()
    }

    fn git_output(repository: &Path, args: &[&str]) -> Vec<u8> {
        let output = Command::new("git")
            .args(args)
            .current_dir(repository)
            .output()
            .expect("execute source identity command");
        assert!(
            output.status.success(),
            "source identity command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn production_prefix(bytes: &[u8]) -> &[u8] {
        let boundary = b"\n#[cfg(test)]";
        let position = bytes
            .windows(boundary.len())
            .position(|window| window == boundary)
            .expect("production/test source boundary");
        &bytes[..position + 1]
    }

    fn verify_production_source() {
        let descriptor: SourceDescriptor =
            serde_json::from_slice(SOURCE_BYTES).expect("source descriptor");
        assert_eq!(descriptor.schema, "delysis.production_source_roots.v0");
        assert_eq!(descriptor.repository_id, "delysis/free-token-energy");
        assert_eq!(descriptor.commit, BASELINE_COMMIT);
        let repository = repository_root();
        let ancestry = Command::new("git")
            .args(["merge-base", "--is-ancestor", BASELINE_COMMIT, "HEAD"])
            .current_dir(&repository)
            .status()
            .expect("execute baseline ancestry check");
        assert!(
            ancestry.success(),
            "fixture commit must descend from baseline"
        );

        for prefix in descriptor.prefixes {
            assert_eq!(prefix.boundary, "first_cfg_test");
            let current = std::fs::read(repository.join(&prefix.path)).expect("read source");
            let baseline = git_output(
                &repository,
                &["show", &format!("{}:{}", descriptor.commit, prefix.path)],
            );
            for bytes in [production_prefix(&current), production_prefix(&baseline)] {
                let identity = sha256_identity("fte.database.production.prefix", bytes);
                assert_eq!(identity.digest.hex, prefix.sha256);
                assert_eq!(identity.length, prefix.byte_len);
            }
        }

        for (path, expected_oid) in descriptor.git_blobs {
            let working_tree =
                git_output(&repository, &["hash-object", "--no-filters", "--", &path]);
            assert_eq!(
                String::from_utf8(working_tree).unwrap().trim(),
                expected_oid
            );
            for revision in [&descriptor.commit, "HEAD"] {
                let actual = git_output(&repository, &["rev-parse", &format!("{revision}:{path}")]);
                assert_eq!(String::from_utf8(actual).unwrap().trim(), expected_oid);
            }
        }

        for path in descriptor.absent_paths {
            assert!(
                !repository.join(&path).exists(),
                "retired path returned: {path}"
            );
            for revision in [&descriptor.commit, "HEAD"] {
                let status = Command::new("git")
                    .args(["cat-file", "-e", &format!("{revision}:{path}")])
                    .current_dir(&repository)
                    .stderr(Stdio::null())
                    .status()
                    .expect("check retired source path");
                assert!(
                    !status.success(),
                    "retired path exists at {revision}:{path}"
                );
            }
        }
    }

    fn temporary_database(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "fte-w1-database-{label}-{}-{}.sqlite",
            std::process::id(),
            W1_DATABASE_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn tables(conn: &Connection) -> BTreeSet<String> {
        let mut statement = conn
            .prepare(
                "SELECT name FROM sqlite_schema
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                 ORDER BY name",
            )
            .expect("prepare table inventory");
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query table inventory")
            .collect::<rusqlite::Result<_>>()
            .expect("collect table inventory")
    }

    #[test]
    fn w1_legacy_database_is_explicitly_unsupported() {
        let manifest: VerticalFixtureManifestV0 =
            serde_json::from_slice(MANIFEST_BYTES).expect("parse database manifest");
        validate_manifest(&manifest).expect("valid database manifest");
        let case = &manifest.cases[0];
        assert_eq!(case.source.commit, BASELINE_COMMIT);
        assert_eq!(
            sha256_identity(case.source.production_tree.id.clone(), SOURCE_BYTES),
            case.source.production_tree
        );
        assert_eq!(
            sha256_identity(case.inputs[0].identity.id.clone(), POLICY_BYTES),
            case.inputs[0].identity
        );
        assert_eq!(
            sha256_identity(case.expected_projection.id.clone(), PROJECTION_BYTES),
            case.expected_projection
        );
        assert_eq!(
            sha256_identity(
                manifest.negative_evidence[0].artifact.id.clone(),
                SUPERSEDED_RECEIPT_BYTES
            ),
            manifest.negative_evidence[0].artifact
        );
        verify_production_source();

        let policy: Value = serde_json::from_slice(POLICY_BYTES).expect("parse database policy");
        assert_eq!(policy["historical_database_available"], false);
        assert_eq!(policy["backward_compatibility_required"], false);
        assert_eq!(
            policy["legacy_sentinel"]["purpose"],
            "adversarial unsupported-input sentinel, not a historical database fixture"
        );

        let legacy_schema = policy["legacy_sentinel"]["schema_sql"]
            .as_str()
            .expect("legacy sentinel schema");
        let legacy_identity = sha256_identity(
            "fte.database.legacy_sentinel.schema",
            legacy_schema.as_bytes(),
        );
        assert_eq!(case.state_identities[0].baseline.identity, legacy_identity);
        let legacy_path = temporary_database("legacy-sentinel");
        {
            let conn = Connection::open(&legacy_path).expect("create unsupported sentinel");
            conn.execute_batch(legacy_schema)
                .expect("create unsupported schema");
            conn.execute(
                "INSERT INTO api_keys (provider_id, key_value) VALUES (?1, ?2)",
                params![
                    "sentinel",
                    policy["legacy_sentinel"]["secret_value"].as_str()
                ],
            )
            .expect("insert noncredential sentinel");
        }
        let legacy_before = std::fs::read(&legacy_path).expect("read sentinel before rejection");
        let legacy_error = match Database::new(legacy_path.clone()) {
            Ok(_) => panic!("legacy sentinel must not open"),
            Err(error) => error,
        };
        assert!(
            legacy_error
                .to_string()
                .contains("unsupported legacy database")
        );
        assert_eq!(
            std::fs::read(&legacy_path).expect("read sentinel after rejection"),
            legacy_before,
            "rejection must not mutate the unsupported database"
        );

        let unversioned_path = temporary_database("unversioned-sentinel");
        {
            let conn = Connection::open(&unversioned_path).expect("create unversioned sentinel");
            conn.execute_batch(
                policy["unversioned_sentinel"]["schema_sql"]
                    .as_str()
                    .expect("unversioned sentinel schema"),
            )
            .expect("create unversioned schema");
        }
        let unversioned_before =
            std::fs::read(&unversioned_path).expect("read unversioned sentinel");
        let unversioned_error = match Database::new(unversioned_path.clone()) {
            Ok(_) => panic!("unversioned populated database must not open"),
            Err(error) => error,
        };
        assert!(
            unversioned_error
                .to_string()
                .contains("unsupported database")
        );
        assert_eq!(
            std::fs::read(&unversioned_path).expect("reread unversioned sentinel"),
            unversioned_before
        );

        let fresh_path = temporary_database("fresh-current");
        let (application_id, schema_version, current_tables) = {
            let db = Database::new(fresh_path.clone()).expect("open fresh database");
            let conn = db.connection().expect("lock fresh database");
            let application_id = conn
                .query_row("PRAGMA application_id", [], |row| row.get::<_, i64>(0))
                .expect("read application ID");
            let schema_version = conn
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("read schema version");
            (application_id, schema_version, tables(&conn))
        };
        assert_eq!(application_id, APPLICATION_ID);
        assert_eq!(schema_version, SCHEMA_VERSION);
        let expected_tables = policy["fresh_database"]["tables"]
            .as_array()
            .expect("current table policy")
            .iter()
            .map(|value| value.as_str().expect("table name").to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(current_tables, expected_tables);
        let signature = format!(
            "application_id={application_id:#010x};user_version={schema_version};tables={}",
            current_tables.iter().cloned().collect::<Vec<_>>().join(",")
        );
        assert_eq!(
            signature,
            policy["fresh_database"]["signature"]
                .as_str()
                .expect("current schema signature")
        );
        Database::new(fresh_path.clone()).expect("exact current database reopens");
        {
            let conn = Connection::open(&fresh_path).expect("open current database sentinel");
            conn.execute_batch(
                policy["unexpected_object_sentinel"]["schema_sql"]
                    .as_str()
                    .expect("unexpected object sentinel schema"),
            )
            .expect("add unexpected schema object sentinel");
        }
        let unexpected_error = match Database::new(fresh_path) {
            Ok(_) => panic!("unexpected schema object must not be adopted"),
            Err(error) => error,
        };
        assert!(
            unexpected_error
                .to_string()
                .contains("unsupported database")
        );

        let wrong_definition_path = temporary_database("wrong-definition");
        {
            let db = Database::new(wrong_definition_path.clone()).expect("open current database");
            db.connection()
                .expect("lock current database")
                .execute_batch(
                    policy["same_name_wrong_definition_sentinel"]["schema_sql"]
                        .as_str()
                        .expect("same-name wrong-definition sentinel schema"),
                )
                .expect("replace canonical definition with sentinel");
        }
        let wrong_definition_error = match Database::new(wrong_definition_path) {
            Ok(_) => panic!("same-name wrong-definition schema must not be adopted"),
            Err(error) => error,
        };
        assert!(
            wrong_definition_error
                .to_string()
                .contains("unsupported database")
        );

        let projection: EquivalenceProjectionV0 = serde_json::from_value(json!({
            "ordered_events": [
                {"sequence": 0, "operation_id": "fte.database.unsupported_legacy", "attempt_id": "database.open.legacy.1", "correlation_id": "fte.database.policy.w1", "kind": "failed", "payload": null},
                {"sequence": 1, "operation_id": "fte.database.fresh_current", "attempt_id": "database.open.fresh.1", "correlation_id": "fte.database.policy.w1", "kind": "completed", "payload": null}
            ],
            "durable_state": [{
                "state_id": "fte.database.legacy_sentinel_schema",
                "schema_id": "delysis.w1.fte.unsupported_legacy_sentinel.v1",
                "before": legacy_identity,
                "after": legacy_identity,
                "disposition": "unchanged"
            }],
            "lifecycle": [
                {"operation_id": "fte.database.unsupported_legacy", "attempt_id": "database.open.legacy.1", "correlation_id": "fte.database.policy.w1", "terminal": "failed", "released": true},
                {"operation_id": "fte.database.fresh_current", "attempt_id": "database.open.fresh.1", "correlation_id": "fte.database.policy.w1", "terminal": "completed", "released": true}
            ],
            "ownership": {"active_operations": 0, "retained_tasks": 0, "expected_workers": 0, "joined_workers": 0},
            "output_facts": {
                "historical_database_available": {"kind": "boolean", "value": false},
                "legacy_opened": {"kind": "boolean", "value": false},
                "legacy_bytes_unchanged": {"kind": "boolean", "value": true},
                "legacy_import_module_present": {"kind": "boolean", "value": false},
                "unversioned_database_rejected": {"kind": "boolean", "value": true},
                "unexpected_schema_object_rejected": {"kind": "boolean", "value": true},
                "same_name_wrong_definition_rejected": {"kind": "boolean", "value": true},
                "fresh_database_opened": {"kind": "boolean", "value": true},
                "current_database_reopened": {"kind": "boolean", "value": true},
                "application_id": {"kind": "integer", "value": application_id},
                "schema_version": {"kind": "integer", "value": schema_version}
            },
            "fail_closed_facts": [
                "plaintext legacy schema was rejected before product schema mutation",
                "unversioned populated schema was rejected before adoption",
                "same-name foreign schema definitions were rejected before use",
                "unsupported input was not represented as historical migration evidence",
                "no credential store or hosted network authority was required"
            ]
        }))
        .expect("construct production-derived database projection");
        let expected: EquivalenceProjectionV0 =
            serde_json::from_slice(PROJECTION_BYTES).expect("parse expected database projection");
        assert_eq!(projection, expected);
        let observation: ObservationEnvelopeV0 = serde_json::from_value(json!({
            "schema": "delysis.vertical_observation.v0",
            "vertical_id": manifest.vertical_id,
            "case_id": case.case_id,
            "implementation_revision": case.source.commit,
            "observed_prerequisites": [],
            "evidence": {
                "schema": "delysis.evidence_claim.v0",
                "tier": "reproducible",
                "threat_model": "production database opening rejects generated unsupported-input sentinels without claiming historical migration equivalence",
                "exact_source": case.source.production_tree.digest,
                "exact_runtime_or_artifact": case.inputs[0].identity.digest,
                "execution_kind": "fixture",
                "omitted_claims": manifest.omitted_claims,
                "negative_evidence": []
            },
            "projection": projection
        }))
        .expect("construct database observation");
        validate_baseline(
            &manifest,
            &case.case_id,
            PROJECTION_BYTES,
            &[],
            &observation,
        )
        .expect("central protocol accepts the explicit no-legacy contract");
    }
}
