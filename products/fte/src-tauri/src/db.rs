use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

const MAX_REQUEST_LOG_ROWS: i64 = 10_000;
const MAX_QUOTA_EVENT_AGE_SECS: i64 = 86_400;

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

#[derive(Debug, Clone)]
pub struct QuotaEvent {
    pub provider_id: String,
    pub model_id: String,
    pub occurred_at: i64,
    pub request_count: u32,
    pub tokens: u32,
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

        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "
            PRAGMA foreign_keys = ON;
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA secure_delete = ON;
            PRAGMA trusted_schema = OFF;
            ",
        )?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.init_schema()?;
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
            CREATE TABLE IF NOT EXISTS api_keys (
                provider_id TEXT PRIMARY KEY,
                key_value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS quota_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                occurred_at INTEGER NOT NULL,
                provider_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                request_count INTEGER NOT NULL DEFAULT 0 CHECK(request_count >= 0),
                tokens INTEGER NOT NULL DEFAULT 0 CHECK(tokens >= 0)
            );

            CREATE INDEX IF NOT EXISTS idx_quota_events_recent
                ON quota_events (occurred_at, provider_id, model_id);

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

            CREATE TABLE IF NOT EXISTS eval_cache (
                model_id TEXT PRIMARY KEY,
                eval_data_json TEXT NOT NULL,
                last_updated DATETIME DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS provider_health (
                provider_id TEXT PRIMARY KEY,
                health_data_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS master_profile (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            DROP TABLE IF EXISTS quota_state;

            -- Older builds asked users to store a reusable password. The app
            -- now stores only a non-secret hint and removes the unsafe field.
            DELETE FROM master_profile WHERE key = 'password';
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

    pub fn save_api_key(&self, provider_id: &str, key_value: &str) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO api_keys (provider_id, key_value) VALUES (?1, ?2)
             ON CONFLICT(provider_id) DO UPDATE SET key_value=excluded.key_value",
            params![provider_id, key_value],
        )?;
        Ok(())
    }

    pub fn delete_api_key(&self, provider_id: &str) -> Result<bool> {
        let conn = self.connection()?;
        Ok(conn.execute(
            "DELETE FROM api_keys WHERE provider_id = ?1",
            params![provider_id],
        )? > 0)
    }

    pub fn get_api_key(&self, provider_id: &str) -> Result<Option<String>> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT key_value FROM api_keys WHERE provider_id = ?1",
            params![provider_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn has_api_key(&self, provider_id: &str) -> Result<bool> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM api_keys WHERE provider_id = ?1)",
            params![provider_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    pub fn save_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn record_quota_event(
        &self,
        provider_id: &str,
        model_id: &str,
        occurred_at: i64,
        request_count: u32,
        tokens: u32,
    ) -> Result<()> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO quota_events
                (occurred_at, provider_id, model_id, request_count, tokens)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![occurred_at, provider_id, model_id, request_count, tokens],
        )?;
        tx.execute(
            "DELETE FROM quota_events WHERE occurred_at < ?1",
            params![occurred_at - MAX_QUOTA_EVENT_AGE_SECS],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_quota_events_since(&self, since_epoch: i64) -> Result<Vec<QuotaEvent>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT provider_id, model_id, occurred_at, request_count, tokens
             FROM quota_events
             WHERE occurred_at >= ?1
             ORDER BY occurred_at, id",
        )?;
        let rows = stmt.query_map(params![since_epoch], |row| {
            Ok(QuotaEvent {
                provider_id: row.get(0)?,
                model_id: row.get(1)?,
                occurred_at: row.get(2)?,
                request_count: row.get(3)?,
                tokens: row.get(4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
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
    fn reopening_database_removes_legacy_password_field() {
        let path = test_database_path("password-migration");
        {
            let db = Database::new(path.clone()).unwrap();
            db.save_profile_field("password", "reusable-secret")
                .unwrap();
            assert!(db.get_profile_field("password").unwrap().is_some());
        }

        let reopened = Database::new(path).unwrap();
        assert!(reopened.get_profile_field("password").unwrap().is_none());
    }

    #[test]
    fn quota_events_round_trip_without_synthetic_state() {
        let db = Database::new(test_database_path("quota-events")).unwrap();
        db.record_quota_event("provider", "model", 1_000, 1, 42)
            .unwrap();

        let events = db.get_quota_events_since(999).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].provider_id, "provider");
        assert_eq!(events[0].model_id, "model");
        assert_eq!(events[0].request_count, 1);
        assert_eq!(events[0].tokens, 42);
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
