use rusqlite::Connection;

use crate::{Result, StoreError};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;
pub const CURRENT_STORE_SCHEMA_VERSION: u32 = 7;

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
    if version < 5 {
        transaction.execute_batch(include_str!(
            "../migrations/0005_generation_command_hardening.sql"
        ))?;
        transaction.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at_ms) VALUES (5, ?1)",
            [loom_types::now_unix_ms()],
        )?;
    }
    if version < 6 {
        transaction.execute_batch(include_str!("../migrations/0006_bounded_branch_index.sql"))?;
        transaction.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at_ms) VALUES (6, ?1)",
            [loom_types::now_unix_ms()],
        )?;
    }
    if version < 7 {
        transaction.execute_batch(include_str!("../migrations/0007_research_admission.sql"))?;
        transaction.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at_ms) VALUES (7, ?1)",
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

    #[test]
    fn version_five_adds_strict_immutable_generation_command_evidence_tables() {
        let mut connection = Connection::open_in_memory().expect("in-memory SQLite");
        configure(&connection).expect("configure SQLite");
        for migration in [
            include_str!("../migrations/0001_initial.sql"),
            include_str!("../migrations/0002_generation_provenance.sql"),
            include_str!("../migrations/0003_transient_drafts.sql"),
            include_str!("../migrations/0004_draft_generations.sql"),
        ] {
            connection
                .execute_batch(migration)
                .expect("apply pre-v5 migration");
        }
        connection
            .pragma_update(None, "user_version", 4_i64)
            .expect("mark version four");

        migrate(&mut connection).expect("migrate generation command evidence");

        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read migrated version");
        assert_eq!(version, CURRENT_STORE_SCHEMA_VERSION);
        for table in ["generation_terminal_evidence", "generation_command_events"] {
            let strict: i64 = connection
                .query_row(
                    "SELECT strict FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .expect("read strict table metadata");
            assert_eq!(strict, 1, "{table} must be STRICT");
        }
        for trigger in [
            "generation_terminal_evidence_are_immutable_update",
            "generation_terminal_evidence_are_immutable_delete",
            "generation_command_events_are_immutable_update",
            "generation_command_events_are_immutable_delete",
        ] {
            let count: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'trigger' AND name = ?1",
                    [trigger],
                    |row| row.get(0),
                )
                .expect("read migration trigger");
            assert_eq!(count, 1, "{trigger} must exist");
        }
    }

    #[test]
    fn version_six_adds_a_strict_immutable_monotonic_generation_index() {
        let mut connection = Connection::open_in_memory().expect("in-memory SQLite");
        configure(&connection).expect("configure SQLite");
        for migration in [
            include_str!("../migrations/0001_initial.sql"),
            include_str!("../migrations/0002_generation_provenance.sql"),
            include_str!("../migrations/0003_transient_drafts.sql"),
            include_str!("../migrations/0004_draft_generations.sql"),
            include_str!("../migrations/0005_generation_command_hardening.sql"),
        ] {
            connection
                .execute_batch(migration)
                .expect("apply pre-v6 migration");
        }
        connection
            .pragma_update(None, "user_version", 5_i64)
            .expect("mark version five");

        migrate(&mut connection).expect("migrate bounded branch index");

        let strict: i64 = connection
            .query_row(
                "SELECT strict FROM pragma_table_list
                 WHERE schema = 'main' AND name = 'generation_run_index'",
                [],
                |row| row.get(0),
            )
            .expect("read strict table metadata");
        assert_eq!(strict, 1);
        for trigger in [
            "generation_run_index_are_immutable_update",
            "generation_run_index_are_immutable_delete",
        ] {
            let count: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'trigger' AND name = ?1",
                    [trigger],
                    |row| row.get(0),
                )
                .expect("read generation index trigger");
            assert_eq!(count, 1, "{trigger} must exist");
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn version_seven_adds_fail_closed_append_only_research_admission() {
        let mut connection = Connection::open_in_memory().expect("in-memory SQLite");
        configure(&connection).expect("configure SQLite");
        for migration in [
            include_str!("../migrations/0001_initial.sql"),
            include_str!("../migrations/0002_generation_provenance.sql"),
            include_str!("../migrations/0003_transient_drafts.sql"),
            include_str!("../migrations/0004_draft_generations.sql"),
            include_str!("../migrations/0005_generation_command_hardening.sql"),
            include_str!("../migrations/0006_bounded_branch_index.sql"),
        ] {
            connection
                .execute_batch(migration)
                .expect("apply pre-v7 migration");
        }
        connection
            .pragma_update(None, "user_version", 6_i64)
            .expect("mark version six");

        migrate(&mut connection).expect("migrate research admission schema");

        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read migrated version");
        assert_eq!(version, CURRENT_STORE_SCHEMA_VERSION);

        for table in [
            "research_model_calls",
            "research_call_terminals",
            "research_output_projections",
            "research_generated_span_occurrences",
            "research_operation_graphs",
            "research_pipeline_operations",
            "research_pipeline_operation_inputs",
            "research_candidate_assemblies",
            "research_candidate_assembly_parts",
            "research_candidate_projections",
            "research_mixed_authorship_assemblies",
            "research_admission_records",
            "research_promotion_command_requests",
            "research_user_presence_events",
            "research_promotion_authorities",
            "research_legacy_candidates",
            "research_legacy_candidate_review_events",
        ] {
            let strict: i64 = connection
                .query_row(
                    "SELECT strict FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap_or_else(|error| panic!("read STRICT status for {table}: {error}"));
            assert_eq!(strict, 1, "{table} must be STRICT");
        }

        for (table, update_trigger, delete_trigger) in [
            (
                "research_model_calls",
                "research_model_calls_immutable_update",
                "research_model_calls_immutable_delete",
            ),
            (
                "research_call_terminals",
                "research_call_terminals_immutable_update",
                "research_call_terminals_immutable_delete",
            ),
            (
                "research_output_projections",
                "research_output_projections_immutable_update",
                "research_output_projections_immutable_delete",
            ),
            (
                "research_generated_span_occurrences",
                "research_generated_spans_immutable_update",
                "research_generated_spans_immutable_delete",
            ),
            (
                "research_operation_graphs",
                "research_operation_graphs_immutable_update",
                "research_operation_graphs_immutable_delete",
            ),
            (
                "research_pipeline_operations",
                "research_pipeline_operations_immutable_update",
                "research_pipeline_operations_immutable_delete",
            ),
            (
                "research_pipeline_operation_inputs",
                "research_pipeline_inputs_immutable_update",
                "research_pipeline_inputs_immutable_delete",
            ),
            (
                "research_candidate_assemblies",
                "research_candidate_assemblies_immutable_update",
                "research_candidate_assemblies_immutable_delete",
            ),
            (
                "research_candidate_assembly_parts",
                "research_assembly_parts_immutable_update",
                "research_assembly_parts_immutable_delete",
            ),
            (
                "research_candidate_projections",
                "research_candidate_projections_immutable_update",
                "research_candidate_projections_immutable_delete",
            ),
            (
                "research_mixed_authorship_assemblies",
                "research_mixed_assemblies_immutable_update",
                "research_mixed_assemblies_immutable_delete",
            ),
            (
                "research_admission_records",
                "research_admission_records_immutable_update",
                "research_admission_records_immutable_delete",
            ),
            (
                "research_promotion_command_requests",
                "research_promotion_command_requests_immutable_update",
                "research_promotion_command_requests_immutable_delete",
            ),
            (
                "research_user_presence_events",
                "research_user_presence_events_immutable_update",
                "research_user_presence_events_immutable_delete",
            ),
            (
                "research_promotion_authorities",
                "research_promotion_authorities_immutable_update",
                "research_promotion_authorities_immutable_delete",
            ),
            (
                "research_legacy_candidates",
                "research_legacy_candidates_immutable_update",
                "research_legacy_candidates_immutable_delete",
            ),
            (
                "research_legacy_candidate_review_events",
                "research_legacy_reviews_immutable_update",
                "research_legacy_reviews_immutable_delete",
            ),
        ] {
            for trigger in [update_trigger, delete_trigger] {
                let count: i64 = connection
                    .query_row(
                        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'trigger' AND name = ?1",
                        [trigger],
                        |row| row.get(0),
                    )
                    .expect("read immutability trigger");
                assert_eq!(count, 1, "{table} lacks {trigger}");
            }
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn research_admission_triggers_reject_claims_incomplete_graphs_and_presence_replay() {
        let mut connection = Connection::open_in_memory().expect("in-memory SQLite");
        configure(&connection).expect("configure SQLite");
        migrate(&mut connection).expect("migrate current schema");

        let hash = |byte: char| byte.to_string().repeat(64);
        let call_record = hash('a');
        let output_blob = hash('b');
        let token_blob = hash('c');
        let event_blob = hash('d');
        let receipt_blob = hash('e');
        let span_record = hash('f');
        for (blob_id, byte_len) in [
            (&call_record, 2_i64),
            (&output_blob, 5),
            (&token_blob, 4),
            (&event_blob, 2),
            (&receipt_blob, 2),
            (&span_record, 2),
        ] {
            connection
                .execute(
                    "INSERT INTO blobs(blob_id, byte_len, media_type, created_at_ms)
                     VALUES (?1, ?2, 'application/octet-stream', 1)",
                    rusqlite::params![blob_id, byte_len],
                )
                .expect("seed blob");
        }
        let fingerprint = hash('1');
        let verification = hash('2');
        connection
            .execute(
                "INSERT INTO research_model_calls(
                    call_id, campaign_id, stage_id, stage_attempt_id, trial_case_id,
                    seed_decimal, model_fingerprint, tokenizer_fingerprint, prompt_fingerprint,
                    sampler_fingerprint, control_program_fingerprint, evidence_class,
                    verification_audit_fingerprint, call_record_blob_id, created_at_ms
                 ) VALUES ('call-live', 'campaign', 'stage', 'attempt', 'case', '7',
                           ?1, ?1, ?1, ?1, ?1,
                           'live_base_writer_claim', ?2, ?3, 1)",
                rusqlite::params![fingerprint, verification, call_record],
            )
            .expect("seed live call");
        connection
            .execute(
                "INSERT INTO research_call_terminals(
                    call_id, status, raw_output_blob_id, raw_output_byte_len,
                    token_ids_blob_id, token_count, token_ids_fingerprint,
                    raw_event_stream_blob_id, backend_receipt_blob_id,
                    terminal_message, created_at_ms
                 ) VALUES ('call-live', 'completed', ?1, 5, ?2, 1, ?3, ?4, ?5, NULL, 1)",
                rusqlite::params![
                    output_blob,
                    token_blob,
                    fingerprint,
                    event_blob,
                    receipt_blob
                ],
            )
            .expect("seed live terminal");
        connection
            .execute(
                "INSERT INTO research_output_projections(
                    occurrence_id, call_id, raw_output_byte_len,
                    displayed_start_byte, displayed_end_byte,
                    endpoint_tail_start_byte, endpoint_tail_end_byte,
                    stop_suffix_start_byte, stop_suffix_end_byte, created_at_ms
                 ) VALUES ('span-live', 'call-live', 5, 0, 5, 5, 5, 5, 5, 1)",
                [],
            )
            .expect("seed output projection");
        connection
            .execute(
                "INSERT INTO research_generated_span_occurrences(
                    occurrence_id, call_id, raw_output_blob_id,
                    output_start_byte, output_end_byte, token_start, token_end,
                    evidence_class, extraction_receipt_fingerprint,
                    verification_audit_fingerprint, span_record_blob_id, created_at_ms
                 ) VALUES ('span-live', 'call-live', ?1, 0, 5, NULL, NULL,
                           'live_base_writer_claim', ?2, ?3, ?4, 1)",
                rusqlite::params![output_blob, fingerprint, verification, span_record],
            )
            .expect("seed admitted span evidence");

        assert!(
            connection
                .execute(
                    "UPDATE research_model_calls SET created_at_ms = 2 WHERE call_id = 'call-live'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "DELETE FROM research_model_calls WHERE call_id = 'call-live'",
                    []
                )
                .is_err()
        );

        let fixture_call_record = hash('3');
        connection
            .execute(
                "INSERT INTO blobs(blob_id, byte_len, media_type, created_at_ms)
                 VALUES (?1, 2, 'application/octet-stream', 1)",
                [&fixture_call_record],
            )
            .expect("fixture call blob");
        connection
            .execute(
                "INSERT INTO research_model_calls(
                    call_id, campaign_id, stage_id, stage_attempt_id, trial_case_id,
                    seed_decimal, model_fingerprint, tokenizer_fingerprint, prompt_fingerprint,
                    sampler_fingerprint, control_program_fingerprint, evidence_class,
                    verification_audit_fingerprint, call_record_blob_id, created_at_ms
                 ) VALUES ('call-fixture', 'campaign', 'stage', 'attempt', 'case', '7',
                           ?1, ?1, ?1, ?1, ?1,
                           'fixture', NULL, ?2, 1)",
                rusqlite::params![fingerprint, fixture_call_record],
            )
            .expect("seed fixture call");
        connection
            .execute(
                "INSERT INTO research_call_terminals(
                    call_id, status, raw_output_blob_id, raw_output_byte_len,
                    token_ids_blob_id, token_count, token_ids_fingerprint,
                    raw_event_stream_blob_id, backend_receipt_blob_id,
                    terminal_message, created_at_ms
                 ) VALUES ('call-fixture', 'completed', ?1, 5, ?2, 1, ?3, ?4, NULL, NULL, 1)",
                rusqlite::params![output_blob, token_blob, fingerprint, event_blob],
            )
            .expect("seed fixture terminal");
        connection
            .execute(
                "INSERT INTO research_output_projections(
                    occurrence_id, call_id, raw_output_byte_len,
                    displayed_start_byte, displayed_end_byte,
                    endpoint_tail_start_byte, endpoint_tail_end_byte,
                    stop_suffix_start_byte, stop_suffix_end_byte, created_at_ms
                 ) VALUES ('span-fixture', 'call-fixture', 5, 0, 5, 5, 5, 5, 5, 1)",
                [],
            )
            .expect("seed fixture projection");
        assert!(
            connection
                .execute(
                    "INSERT INTO research_generated_span_occurrences(
                        occurrence_id, call_id, raw_output_blob_id,
                        output_start_byte, output_end_byte, token_start, token_end,
                        evidence_class, extraction_receipt_fingerprint,
                        verification_audit_fingerprint, span_record_blob_id, created_at_ms
                     ) VALUES ('span-fixture', 'call-fixture', ?1, 0, 5, NULL, NULL,
                               'fixture', ?2, ?3, ?4, 1)",
                    rusqlite::params![output_blob, fingerprint, verification, span_record],
                )
                .is_err(),
            "a fixture declaration must not gain replay verification"
        );

        let graph_record = hash('4');
        let assembly_record = hash('5');
        let graph_fingerprint = hash('6');
        for (blob_id, byte_len) in [(&graph_record, 2_i64), (&assembly_record, 2)] {
            connection
                .execute(
                    "INSERT INTO blobs(blob_id, byte_len, media_type, created_at_ms)
                     VALUES (?1, ?2, 'application/octet-stream', 1)",
                    rusqlite::params![blob_id, byte_len],
                )
                .expect("graph artifact blob");
        }
        connection
            .execute(
                "INSERT INTO research_operation_graphs(
                    graph_fingerprint, graph_record_blob_id, output_operation_id,
                    node_count, created_at_ms
                 ) VALUES (?1, ?2, 'op-assemble', 3, 1)",
                rusqlite::params![graph_fingerprint, graph_record],
            )
            .expect("seed graph");
        for (position, operation_id, operation_kind, reference_id, evidence) in [
            (
                0_i64,
                "op-call",
                "model_call",
                "call-live",
                Some("live_base_writer_claim"),
            ),
            (1, "op-extract", "extract_span", "span-live", None),
            (2, "op-assemble", "assemble", "assembly", None),
        ] {
            connection
                .execute(
                    "INSERT INTO research_pipeline_operations(
                        graph_fingerprint, position, operation_id, operation_kind,
                        reference_id, evidence_class
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![
                        graph_fingerprint,
                        position,
                        operation_id,
                        operation_kind,
                        reference_id,
                        evidence
                    ],
                )
                .expect("seed graph operation");
        }
        connection
            .execute(
                "INSERT INTO research_candidate_assemblies(
                    assembly_id, graph_fingerprint, part_count,
                    part_order_fingerprint, assembled_blob_id, assembled_byte_len,
                    assembly_record_blob_id, created_at_ms
                 ) VALUES ('assembly', ?1, 1, ?2, ?3, 5, ?4, 1)",
                rusqlite::params![graph_fingerprint, fingerprint, output_blob, assembly_record],
            )
            .expect("seed assembly declaration");
        connection
            .execute(
                "INSERT INTO research_candidate_assembly_parts(
                    assembly_id, position, join_before, occurrence_id
                 ) VALUES ('assembly', 0, 'none', 'span-live')",
                [],
            )
            .expect("seed assembly part");
        assert!(
            connection
                .execute(
                    "INSERT INTO research_admission_records(
                        admission_record_id, subject_kind, subject_id, admitted_at_ms
                     ) VALUES ('forged', 'candidate_assembly', 'assembly', 1)",
                    [],
                )
                .is_err(),
            "a graph with missing edges must not be admitted"
        );
        connection
            .execute(
                "INSERT INTO research_pipeline_operation_inputs(
                    graph_fingerprint, operation_id, position, input_operation_id
                 ) VALUES (?1, 'op-extract', 0, 'op-call')",
                [&graph_fingerprint],
            )
            .expect("extract edge");
        connection
            .execute(
                "INSERT INTO research_pipeline_operation_inputs(
                    graph_fingerprint, operation_id, position, input_operation_id
                 ) VALUES (?1, 'op-assemble', 1, 'op-extract')",
                [&graph_fingerprint],
            )
            .expect("wrongly positioned assembly edge");
        assert!(
            connection
                .execute(
                    "INSERT INTO research_admission_records(
                        admission_record_id, subject_kind, subject_id, admitted_at_ms
                     ) VALUES ('permuted', 'candidate_assembly', 'assembly', 1)",
                    [],
                )
                .is_err(),
            "assembly graph inputs must preserve exact part positions"
        );

        let source_blob = hash('7');
        let source_artifact = "source-artifact";
        let authority_record = hash('8');
        let presence_blob = hash('9');
        let mixed_graph_record = hash('g');
        let mixed_graph_fingerprint = hash('k');
        let mixed_record = hash('h');
        for (blob_id, byte_len) in [
            (&source_blob, 5_i64),
            (&authority_record, 2),
            (&presence_blob, 2),
            (&mixed_graph_record, 2),
            (&mixed_record, 2),
        ] {
            connection
                .execute(
                    "INSERT INTO blobs(blob_id, byte_len, media_type, created_at_ms)
                     VALUES (?1, ?2, 'application/octet-stream', 1)",
                    rusqlite::params![blob_id, byte_len],
                )
                .expect("authority blob");
        }
        connection
            .execute_batch(&format!(
                "INSERT INTO artifacts(artifact_id, blob_id, artifact_kind, media_type, metadata_json, created_at_ms)
                 VALUES ('{source_artifact}', '{source_blob}', 'document_revision', 'text/plain', '{{}}', 1);
                 INSERT INTO documents(document_id, relative_path, document_kind, created_at_ms)
                 VALUES ('source-document', 'manuscript/source.md', 'prose', 1);
                 INSERT INTO revisions(revision_id, document_id, parent_revision_id, artifact_id, reason, created_at_ms)
                 VALUES ('source-revision', 'source-document', NULL, '{source_artifact}', 'source', 1);
                 INSERT INTO research_operation_graphs(
                    graph_fingerprint, graph_record_blob_id, output_operation_id,
                    node_count, created_at_ms
                 ) VALUES ('{mixed_graph_fingerprint}', '{mixed_graph_record}', 'mixed-op', 1, 1);
                 INSERT INTO research_pipeline_operations(
                    graph_fingerprint, position, operation_id, operation_kind,
                    reference_id, evidence_class
                 ) VALUES ('{mixed_graph_fingerprint}', 0, 'mixed-op', 'literal_text', '{source_blob}', NULL);
                 INSERT INTO research_mixed_authorship_assemblies(
                    mixed_assembly_id, output_blob_id, output_byte_len,
                    graph_fingerprint, mixed_record_blob_id, created_at_ms
                 ) VALUES ('mixed-subject', '{source_blob}', 5, '{mixed_graph_fingerprint}', '{mixed_record}', 1);
                 INSERT INTO research_admission_records(
                    admission_record_id, subject_kind, subject_id, admitted_at_ms
                 ) VALUES ('mixed-admission', 'mixed_authorship', 'mixed-subject', 1);"
            ))
            .expect("seed promotion subject");
        connection
            .execute(
                "INSERT INTO blobs(blob_id, byte_len, media_type, created_at_ms)
                 VALUES (?1, 2, 'application/octet-stream', 1)",
                [&fingerprint],
            )
            .expect("canonical promotion request blob");
        connection
            .execute(
                "INSERT INTO research_promotion_command_requests(
                    command_id, command_request_fingerprint,
                    canonical_request_blob_id, canonical_request_byte_len, project_id,
                    source_revision_id, source_blob_id, subject_kind, subject_id,
                    admission_record_id, intended_result_blob_id,
                    intended_result_byte_len, requested_at_ms, recorded_at_ms
                 ) VALUES (
                    'promotion-one', ?1, ?1, 2, 'PPPPPPPPPPPPPPPPPPPPPPPPPP',
                    'source-revision', ?2, 'mixed_authorship', 'mixed-subject',
                    'mixed-admission', ?2, 5, 10, 12
                 )",
                rusqlite::params![fingerprint, source_blob],
            )
            .expect("pre-mutation promotion request");
        connection
            .execute(
                "INSERT INTO research_user_presence_events(
                    event_receipt_blob_id, command_id, command_request_fingerprint,
                    actor, user_presence_kind,
                    session_fingerprint, monotonic_event_index,
                    occurred_at_ms, created_at_ms
                 ) VALUES (?1, 'promotion-one', ?2, 'human', 'editor_gesture', ?2, 1, 15, 15)",
                rusqlite::params![presence_blob, fingerprint],
            )
            .expect("first command presence");
        connection
            .execute(
                "INSERT INTO research_promotion_authorities(
                    command_id, command_request_fingerprint, actor, project_id,
                    source_revision_id, source_blob_id,
                    subject_kind, subject_id, admission_record_id,
                    intended_result_blob_id, intended_result_byte_len,
                    user_presence_kind, session_fingerprint, event_receipt_blob_id,
                    monotonic_event_index, occurred_at_ms,
                    authority_record_blob_id, intent_recorded_at_ms
                 ) VALUES (
                    'promotion-one', ?1, 'human', 'PPPPPPPPPPPPPPPPPPPPPPPPPP',
                    'source-revision', ?2, 'mixed_authorship', 'mixed-subject',
                    'mixed-admission', ?2, 5, 'editor_gesture', ?1, ?3,
                    1, 15, ?4, 16
                 )",
                rusqlite::params![fingerprint, source_blob, presence_blob, authority_record],
            )
            .expect("authority before mutation or receipt");
        let receipt_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM command_receipts WHERE command_id = 'promotion-one'",
                [],
                |row| row.get(0),
            )
            .expect("receipt count");
        assert_eq!(receipt_count, 0, "intent must precede its terminal receipt");
        assert!(
            connection
                .execute(
                    "INSERT INTO research_user_presence_events(
                        event_receipt_blob_id, command_id, command_request_fingerprint,
                        actor, user_presence_kind,
                        session_fingerprint, monotonic_event_index,
                        occurred_at_ms, created_at_ms
                     ) VALUES (?1, 'promotion-two', ?2, 'human', 'editor_gesture', ?2, 2, 15, 15)",
                    rusqlite::params![presence_blob, fingerprint],
                )
                .is_err(),
            "one presence receipt must not authorize multiple commands"
        );
        assert!(
            connection
                .execute(
                    "UPDATE research_user_presence_events SET occurred_at_ms = 16
                     WHERE event_receipt_blob_id = ?1",
                    [&presence_blob],
                )
                .is_err()
        );

        let settlement_table_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name = 'research_promotion_settlements'",
                [],
                |row| row.get(0),
            )
            .expect("settlement table count");
        assert_eq!(
            settlement_table_count, 0,
            "promotion application stays disabled"
        );
        assert!(
            connection
                .execute(
                    "DELETE FROM research_user_presence_events WHERE event_receipt_blob_id = ?1",
                    [&presence_blob],
                )
                .is_err()
        );
    }
}
