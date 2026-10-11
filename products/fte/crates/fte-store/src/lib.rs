//! Persistent gateway state and injected secret/cache-store boundaries.

use fte_types::{GatewayError, GatewayResponse, RequestId};
pub use fte_types::{ResponseStore, SecretResolver};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::path::Path;
use std::sync::Mutex;

pub struct SqliteStore {
    connection: Mutex<Connection>,
}

impl std::fmt::Debug for SqliteStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SqliteStore")
            .finish_non_exhaustive()
    }
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GatewayError> {
        let mut connection = Connection::open(path).map_err(store_error)?;
        // Check identity under a write reservation before changing schema or WAL.
        // Pre-release unversioned databases are deliberately unsupported.
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let application_id: i64 = transaction
            .pragma_query_value(None, "application_id", |row| row.get(0))
            .map_err(store_error)?;
        let version: i64 = transaction
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(store_error)?;
        let objects: i64 = transaction
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .map_err(store_error)?;
        if application_id == 0 && version == 0 && objects == 0 {
            transaction
                .execute_batch(
                    "CREATE TABLE gateway_responses (
                    id TEXT PRIMARY KEY NOT NULL,
                    request_id TEXT NOT NULL,
                    backend_id TEXT NOT NULL,
                    model_id TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    created_at INTEGER NOT NULL DEFAULT (unixepoch())
                 );
                 PRAGMA application_id = 1179927890;
                 PRAGMA user_version = 2;",
                )
                .map_err(store_error)?;
        } else if application_id != 1179927890 || version != 2 || objects != 1 {
            return Err(store_error(
                "unrecognized response store identity or schema version",
            ));
        }
        // Preparing the real queries checks the required column contract before
        // mutating connection settings, even for a marker-bearing damaged file.
        transaction.prepare("SELECT id, request_id, backend_id, model_id, payload_json, created_at FROM gateway_responses").map_err(store_error)?;
        transaction.commit().map_err(store_error)?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(store_error)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn in_memory() -> Result<Self, GatewayError> {
        Self::open(":memory:")
    }
}

impl ResponseStore for SqliteStore {
    fn put(&self, response: &GatewayResponse) -> Result<(), GatewayError> {
        let payload = serde_json::to_string(response).map_err(store_error)?;
        self.connection
            .lock()
            .map_err(store_error)?
            .execute(
                "INSERT OR REPLACE INTO gateway_responses
                 (id, request_id, backend_id, model_id, payload_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    response.id,
                    response.request_id.0,
                    response.route.backend_id,
                    response.route.model_id,
                    payload
                ],
            )
            .map_err(store_error)?;
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Option<GatewayResponse>, GatewayError> {
        let row = self
            .connection
            .lock()
            .map_err(store_error)?
            .query_row(
                "SELECT request_id, backend_id, model_id, payload_json FROM gateway_responses WHERE id = ?1",
                [id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)),
            )
            .optional()
            .map_err(store_error)?;
        row.map(|(request_id, backend_id, model_id, payload)| {
            let value: serde_json::Value = serde_json::from_str(&payload).map_err(store_error)?;
            if value.get("provider_response_id").is_none() {
                return Err(store_error(
                    "response payload lacks the current continuation field",
                ));
            }
            let response: GatewayResponse = serde_json::from_value(value).map_err(store_error)?;
            if response.id != id
                || response.request_id.0 != request_id
                || response.route.backend_id != backend_id
                || response.route.model_id != model_id
            {
                return Err(store_error(
                    "response payload identity differs from its stored owner",
                ));
            }
            Ok(response)
        })
        .transpose()
    }

    fn delete(&self, id: &str) -> Result<bool, GatewayError> {
        let affected = self
            .connection
            .lock()
            .map_err(store_error)?
            .execute("DELETE FROM gateway_responses WHERE id = ?1", [id])
            .map_err(store_error)?;
        Ok(affected > 0)
    }
}

fn store_error(error: impl std::fmt::Display) -> GatewayError {
    GatewayError {
        code: "gateway_store_error".to_string(),
        class: fte_types::ErrorClass::Internal,
        retryable: false,
        http_status: 500,
        request_id: RequestId::new(),
        provider: None,
        safe_detail: format!("gateway state storage failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fte_types::{BackendLocation, GatewayUsage, ResolvedRoute, TerminalStatus};

    #[test]
    fn foreign_and_future_databases_are_refused_without_mutation() {
        for (label, ddl) in [
            (
                "foreign",
                "CREATE TABLE private(value TEXT); INSERT INTO private VALUES ('preserved');",
            ),
            (
                "future",
                "PRAGMA application_id = 1179927890; PRAGMA user_version = 3;",
            ),
            (
                "prior",
                "CREATE TABLE gateway_responses(id TEXT PRIMARY KEY NOT NULL, request_id TEXT NOT NULL, backend_id TEXT NOT NULL, model_id TEXT NOT NULL, payload_json TEXT NOT NULL, created_at INTEGER NOT NULL DEFAULT (unixepoch()));
                 INSERT INTO gateway_responses VALUES ('old','request','backend','model','preserved',0);
                 PRAGMA application_id = 1179927890; PRAGMA user_version = 1;",
            ),
            (
                "unversioned",
                "CREATE TABLE gateway_responses(id TEXT PRIMARY KEY);",
            ),
            (
                "foreign-lookalike",
                "CREATE TABLE sqliteXmetadata(value TEXT); INSERT INTO sqliteXmetadata VALUES ('preserved');",
            ),
            (
                "foreign-case-lookalike",
                "CREATE TABLE SQLiteYmetadata(value TEXT); INSERT INTO SQLiteYmetadata VALUES ('preserved');",
            ),
        ] {
            let path = std::env::temp_dir()
                .join(format!("fte-store-{label}-{}.sqlite", RequestId::new().0));
            let connection = Connection::open(&path).expect("fixture");
            connection.execute_batch(ddl).expect("schema");
            drop(connection);
            let before = std::fs::read(&path).expect("before");
            assert!(SqliteStore::open(&path).is_err());
            assert_eq!(std::fs::read(&path).expect("after"), before);
            assert!(!std::path::PathBuf::from(format!("{}-wal", path.display())).exists());
            std::fs::remove_file(path).expect("cleanup");
        }
    }

    #[test]
    fn responses_round_trip_and_delete_additively() {
        let store = SqliteStore::in_memory().expect("open store");
        let response = GatewayResponse {
            id: "resp_test".to_string(),
            provider_response_id: None,
            request_id: RequestId::new(),
            model: "model".to_string(),
            route: ResolvedRoute {
                backend_id: "local".to_string(),
                model_id: "model".to_string(),
                display_name: "Model".to_string(),
                location: BackendLocation::LocalEmbedded,
                catalog_version: "test".to_string(),
            },
            output: vec![],
            usage: GatewayUsage::default(),
            output_groups: Vec::new(),
            status: TerminalStatus::Completed,
            previous_response_id: None,
        };
        store.put(&response).expect("put response");
        assert_eq!(
            store.get("resp_test").expect("get response"),
            Some(response)
        );
        assert!(store.delete("resp_test").expect("delete response"));
        assert!(store.get("resp_test").expect("get missing").is_none());
    }

    #[test]
    fn current_store_reopens_and_refuses_response_identity_drift() {
        let path =
            std::env::temp_dir().join(format!("fte-continuation-{}.sqlite", RequestId::new().0));
        let response = GatewayResponse {
            id: "local_B".into(),
            provider_response_id: Some("upstream_A".into()),
            request_id: RequestId::new(),
            model: "provider-model-version".into(),
            route: ResolvedRoute {
                backend_id: "hosted".into(),
                model_id: "selected-model".into(),
                display_name: "Model".into(),
                location: BackendLocation::Hosted,
                catalog_version: "test".into(),
            },
            output: vec![],
            output_groups: vec![],
            usage: GatewayUsage::default(),
            status: TerminalStatus::Completed,
            previous_response_id: None,
        };
        let store = SqliteStore::open(&path).expect("fresh current store");
        store.put(&response).expect("retained handle");
        drop(store);
        let store = SqliteStore::open(&path).expect("current store reopen");
        assert_eq!(
            store.get("local_B").expect("same identity"),
            Some(response.clone())
        );
        for pointer in [
            "/id",
            "/request_id",
            "/route/backend_id",
            "/route/model_id",
            "/provider_response_id",
        ] {
            let mut payload = serde_json::to_value(&response).expect("current payload");
            if pointer == "/provider_response_id" {
                payload
                    .as_object_mut()
                    .expect("response object")
                    .remove("provider_response_id");
            } else {
                *payload.pointer_mut(pointer).expect("identity field") =
                    serde_json::json!("wrong_owner");
            }
            let encoded = payload.to_string();
            store
                .connection
                .lock()
                .expect("store connection")
                .execute(
                    "UPDATE gateway_responses SET payload_json = ?1 WHERE id = 'local_B'",
                    [&encoded],
                )
                .expect("corrupt owner fixture");
            assert_eq!(
                store
                    .get("local_B")
                    .expect_err("no identity/default rescue")
                    .code,
                "gateway_store_error"
            );
            let preserved: String = store
                .connection
                .lock()
                .expect("store connection")
                .query_row(
                    "SELECT payload_json FROM gateway_responses WHERE id = 'local_B'",
                    [],
                    |row| row.get(0),
                )
                .expect("preserved incompatible bytes");
            assert_eq!(preserved, encoded);
        }
        drop(store);
        std::fs::remove_file(path).expect("remove owned store");
    }
}
