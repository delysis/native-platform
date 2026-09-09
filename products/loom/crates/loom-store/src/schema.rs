use rusqlite::{Connection, TransactionBehavior};

use crate::{Result, StoreError};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;
pub const CURRENT_STORE_SCHEMA_VERSION: u32 = 15;
const APPLICATION_ID: u32 = 0x4c4f_4f4d;
const SCHEMA: &str = include_str!("../schema.sql");

/// These products are unreleased. Accept a fresh database or this exact schema;
/// leave incompatible databases untouched instead of maintaining upgrade paths.
pub(crate) fn initialize_schema(connection: &mut Connection, initializing: bool) -> Result<()> {
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let application: u32 = transaction.pragma_query_value(None, "application_id", |r| r.get(0))?;
    let version: u32 = transaction.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let objects = schema_objects(&transaction)?;
    if initializing && application == 0 && version == 0 && objects.is_empty() {
        transaction.execute_batch(SCHEMA)?;
        transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
        transaction.pragma_update(None, "user_version", CURRENT_STORE_SCHEMA_VERSION)?;
    } else {
        if version != CURRENT_STORE_SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema {
                found: version,
                supported: CURRENT_STORE_SCHEMA_VERSION,
            });
        }
        if application != APPLICATION_ID {
            return Err(StoreError::CorruptDatabase(
                "database is not a current Loom store".into(),
            ));
        }
        let expected = Connection::open_in_memory()?;
        expected.execute_batch(SCHEMA)?;
        if objects != schema_objects(&expected)? {
            return Err(StoreError::CorruptDatabase(
                "Loom schema does not match this build".into(),
            ));
        }
    }
    transaction.commit()?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "trusted_schema", "OFF")?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    Ok(())
}

fn schema_objects(connection: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    connection
        .prepare("SELECT name, sql FROM sqlite_schema WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY name")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incompatible_databases_are_rejected_without_mutation_or_wal_creation() {
        for setup in [
            "CREATE TABLE unrelated(value TEXT); INSERT INTO unrelated VALUES ('preserve');",
            "PRAGMA user_version = 14; CREATE TABLE manuscript(text TEXT);",
            "PRAGMA application_id = 1280266061; PRAGMA user_version = 16;",
            "PRAGMA user_version = 15;",
        ] {
            let directory = tempfile::tempdir().expect("temporary directory");
            let path = directory.path().join("store.sqlite3");
            Connection::open(&path)
                .expect("fixture")
                .execute_batch(setup)
                .expect("fixture schema");
            let before = std::fs::read(&path).expect("before bytes");
            let mut connection = Connection::open(&path).expect("open fixture");
            assert!(initialize_schema(&mut connection, true).is_err(), "{setup}");
            drop(connection);
            assert_eq!(
                std::fs::read(&path).expect("after bytes"),
                before,
                "{setup}"
            );
            assert!(!path.with_extension("sqlite3-wal").exists());
        }
    }

    #[test]
    fn current_schema_reopens_and_rejects_removed_constraints() {
        let mut connection = Connection::open_in_memory().expect("database");
        initialize_schema(&mut connection, true).expect("initialize");
        initialize_schema(&mut connection, true).expect("reopen");
        connection
            .execute_batch("DROP TRIGGER blobs_are_immutable_update")
            .expect("remove constraint");
        assert!(initialize_schema(&mut connection, true).is_err());
        let count: u32 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'blobs_are_immutable_update'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(
            count, 0,
            "reopening must not silently repair a foreign schema"
        );
    }

    #[test]
    fn current_schema_enforces_canonical_document_titles() {
        let mut connection = Connection::open_in_memory().expect("database");
        initialize_schema(&mut connection, true).expect("initialize");
        connection.execute("INSERT INTO documents(document_id, relative_path, document_kind, created_at_ms) VALUES ('document', 'manuscript/Untitled.md', 'prose', 1)", []).expect("document");
        for title in [
            "  padded  ",
            "\u{00a0}leading",
            "trailing\u{1680}",
            "\u{3000}leading",
            "embedded\ncontrol",
            "embedded\u{007f}",
            "embedded\u{0085}",
            "embedded\0nul",
        ] {
            assert!(
                connection
                    .execute(
                        "UPDATE documents SET display_title = ?1 WHERE document_id = 'document'",
                        [title]
                    )
                    .is_err(),
                "{title:?}"
            );
        }
        assert!(
            connection
                .execute(
                    "UPDATE documents SET display_title = ?1 WHERE document_id = 'document'",
                    ["é".repeat(129)]
                )
                .is_err()
        );
        connection
            .execute(
                "UPDATE documents SET display_title = ?1 WHERE document_id = 'document'",
                ["é".repeat(128)],
            )
            .expect("exact byte limit");
        connection
            .execute(
                "UPDATE documents SET display_title = ?1 WHERE document_id = 'document'",
                ["inner\u{00a0}space"],
            )
            .expect("interior unicode whitespace");
    }
}
