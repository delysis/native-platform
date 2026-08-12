//! Persistent gateway state and injected secret/cache-store boundaries.

use fte_types::{GatewayError, GatewayResponse, RequestId};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::Mutex;

pub trait SecretResolver: Send + Sync {
    fn resolve(&self, provider: &str) -> Result<Option<String>, GatewayError>;
}

pub trait ResponseStore: Send + Sync {
    fn put(&self, response: &GatewayResponse) -> Result<(), GatewayError>;
    fn get(&self, id: &str) -> Result<Option<GatewayResponse>, GatewayError>;
    fn delete(&self, id: &str) -> Result<bool, GatewayError>;
}

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
        let connection = Connection::open(path).map_err(store_error)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE IF NOT EXISTS gateway_responses (
                    id TEXT PRIMARY KEY NOT NULL,
                    request_id TEXT NOT NULL,
                    backend_id TEXT NOT NULL,
                    model_id TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    created_at INTEGER NOT NULL DEFAULT (unixepoch())
                 );",
            )
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
        let payload = self
            .connection
            .lock()
            .map_err(store_error)?
            .query_row(
                "SELECT payload_json FROM gateway_responses WHERE id = ?1",
                [id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(store_error)?;
        payload
            .map(|payload| serde_json::from_str(&payload).map_err(store_error))
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
    fn responses_round_trip_and_delete_additively() {
        let store = SqliteStore::in_memory().expect("open store");
        let response = GatewayResponse {
            id: "resp_test".to_string(),
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
}
