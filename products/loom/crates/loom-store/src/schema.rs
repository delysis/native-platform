use rusqlite::Connection;

use crate::{Result, StoreError};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;
pub const CURRENT_STORE_SCHEMA_VERSION: u32 = 4;

pub(crate) fn configure(connection: &Connection) -> Result<()> {
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    connection.pragma_update(None, "busy_timeout", 5_000_i64)?;
    connection.pragma_update(None, "trusted_schema", "OFF")?;
    Ok(())
}

pub(crate) fn migrate(connection: &mut Connection) -> Result<()> {
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > CURRENT_STORE_SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchema {
            found: version,
            supported: CURRENT_STORE_SCHEMA_VERSION,
        });
    }

    let transaction = connection.transaction()?;
    if version < 1 {
        transaction.execute_batch(include_str!("../migrations/0001_initial.sql"))?;
        transaction.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at_ms) VALUES (1, ?1)",
            [loom_types::now_unix_ms()],
        )?;
    }
    if version < 2 {
        transaction.execute_batch(include_str!("../migrations/0002_generation_provenance.sql"))?;
        transaction.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at_ms) VALUES (2, ?1)",
            [loom_types::now_unix_ms()],
        )?;
    }
    if version < 3 {
        transaction.execute_batch(include_str!("../migrations/0003_transient_drafts.sql"))?;
        transaction.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at_ms) VALUES (3, ?1)",
            [loom_types::now_unix_ms()],
        )?;
    }
    if version < 4 {
        transaction.execute_batch(include_str!("../migrations/0004_draft_generations.sql"))?;
        transaction.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at_ms) VALUES (4, ?1)",
            [loom_types::now_unix_ms()],
        )?;
    }
    transaction.pragma_update(
        None,
        "user_version",
        i64::from(CURRENT_STORE_SCHEMA_VERSION),
    )?;
    transaction.commit()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_never_downgrades_a_future_database() {
        let mut connection = Connection::open_in_memory().expect("in-memory SQLite");
        configure(&connection).expect("configure SQLite");
        connection
            .pragma_update(
                None,
                "user_version",
                i64::from(CURRENT_STORE_SCHEMA_VERSION) + 1,
            )
            .expect("set future version");
        assert!(matches!(
            migrate(&mut connection),
            Err(StoreError::UnsupportedSchema { .. })
        ));
        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read version");
        assert_eq!(version, CURRENT_STORE_SCHEMA_VERSION + 1);
    }

    #[test]
    fn version_four_migrates_live_draft_and_preserves_monotonic_identity() {
        let mut connection = Connection::open_in_memory().expect("in-memory SQLite");
        configure(&connection).expect("configure SQLite");
        connection
            .execute_batch(include_str!("../migrations/0001_initial.sql"))
            .expect("schema one");
        connection
            .execute_batch(include_str!("../migrations/0002_generation_provenance.sql"))
            .expect("schema two");
        connection
            .execute_batch(include_str!("../migrations/0003_transient_drafts.sql"))
            .expect("schema three");
        connection
            .execute_batch(
                "INSERT INTO blobs(blob_id, byte_len, media_type, created_at_ms)
                 VALUES ('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 5, 'text/plain', 1);
                 INSERT INTO artifacts(artifact_id, blob_id, artifact_kind, media_type, metadata_json, created_at_ms)
                 VALUES ('artifact', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'document_revision', 'text/plain', '{}', 1);
                 INSERT INTO documents(document_id, relative_path, document_kind, created_at_ms)
                 VALUES ('document', 'manuscript/001.md', 'prose', 1);
                 INSERT INTO revisions(revision_id, document_id, parent_revision_id, artifact_id, reason, created_at_ms)
                 VALUES ('revision', 'document', NULL, 'artifact', 'initial', 1);
                 INSERT INTO transient_drafts(document_id, source_revision_id, draft_blob_id, storage_slot, draft_version, updated_at_ms)
                 VALUES ('document', 'revision', 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 1, 7, 1);
                 PRAGMA user_version = 3;",
            )
            .expect("version three fixture");

        migrate(&mut connection).expect("migrate live draft");

        let migrated: (i64, i64) = connection
            .query_row(
                "SELECT td.base_version, ds.last_version
                 FROM transient_drafts td
                 JOIN transient_draft_sequences ds USING (document_id)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("migrated draft identity");
        assert_eq!(migrated, (6, 7));
        assert!(
            connection
                .execute(
                    "UPDATE transient_draft_sequences SET last_version = 7 WHERE document_id = 'document'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "DELETE FROM transient_draft_sequences WHERE document_id = 'document'",
                    [],
                )
                .is_err()
        );
    }
}
