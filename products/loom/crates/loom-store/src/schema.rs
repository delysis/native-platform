use rusqlite::Connection;

use crate::{Result, StoreError};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;
pub const CURRENT_STORE_SCHEMA_VERSION: u32 = 10;

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
    if version < 8 {
        transaction.execute_batch(include_str!(
            "../migrations/0008_verified_inference_batches.sql"
        ))?;
        transaction.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at_ms) VALUES (8, ?1)",
            [loom_types::now_unix_ms()],
        )?;
    }
    if version < 9 {
        transaction.execute_batch(include_str!(
            "../migrations/0009_research_execution_ledger.sql"
        ))?;
        transaction.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at_ms) VALUES (9, ?1)",
            [loom_types::now_unix_ms()],
        )?;
    }
    if version < 10 {
        transaction.execute_batch(include_str!("../migrations/0010_token_piece_evidence.sql"))?;
        transaction.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at_ms) VALUES (10, ?1)",
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
    fn version_ten_adds_immutable_token_piece_evidence_without_rewriting_version_nine() {
        let mut connection = Connection::open_in_memory().expect("in-memory SQLite");
        configure(&connection).expect("configure SQLite");
        for migration in [
            include_str!("../migrations/0001_initial.sql"),
            include_str!("../migrations/0002_generation_provenance.sql"),
            include_str!("../migrations/0003_transient_drafts.sql"),
            include_str!("../migrations/0004_draft_generations.sql"),
            include_str!("../migrations/0005_generation_command_hardening.sql"),
            include_str!("../migrations/0006_bounded_branch_index.sql"),
            include_str!("../migrations/0007_research_admission.sql"),
            include_str!("../migrations/0008_verified_inference_batches.sql"),
            include_str!("../migrations/0009_research_execution_ledger.sql"),
        ] {
            connection
                .execute_batch(migration)
                .expect("apply through v9");
        }
        connection
            .pragma_update(None, "user_version", 9_i64)
            .expect("mark version nine");

        migrate(&mut connection).expect("migrate token-piece evidence");

        let (strict, columns): (i64, i64) = connection
            .query_row(
                "SELECT strict, (SELECT COUNT(*) FROM pragma_table_info('research_token_piece_evidence'))
                 FROM pragma_table_list
                 WHERE schema = 'main' AND name = 'research_token_piece_evidence'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read v10 table metadata");
        assert_eq!(strict, 1);
        assert_eq!(columns, 7);
        for trigger in [
            "research_token_piece_evidence_validate_insert",
            "research_token_piece_evidence_immutable_update",
            "research_token_piece_evidence_immutable_delete",
        ] {
            let count: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'trigger' AND name = ?1",
                    [trigger],
                    |row| row.get(0),
                )
                .expect("read v10 trigger");
            assert_eq!(count, 1, "{trigger} must exist");
        }
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
    fn version_eight_seals_exact_prompt_and_every_ordered_verified_case() {
        let mut connection = Connection::open_in_memory().expect("in-memory SQLite");
        configure(&connection).expect("configure SQLite");
        migrate(&mut connection).expect("migrate current schema");

        for table in [
            "research_model_bindings",
            "research_model_binding_sources",
            "research_verified_inference_batches",
            "research_verified_prompt_sources",
            "research_cancelled_call_diagnostics",
            "research_completed_call_evidence",
            "research_verified_inference_batch_calls",
            "research_verified_inference_batch_seals",
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

        let hash = |byte: char| byte.to_string().repeat(64);
        let batch = hash('0');
        let binding = hash('1');
        let binding_canonical = hash('2');
        let binding_source = hash('3');
        let binding_capabilities = hash('4');
        let prompt_specification = hash('5');
        let prompt = hash('6');
        let prompt_tokens = hash('7');
        let runtime_model = hash('8');
        let tokenizer = hash('9');
        let prompt_token_fingerprint = hash('a');
        let compiled_prompt = hash('b');
        let call_record = hash('c');
        let output = hash('d');
        let output_tokens = hash('e');
        let event = hash('f');
        let receipt = hash('0');
        let case_verification = hash('1');
        for (blob_id, byte_len) in [
            (&binding_canonical, 2_i64),
            (&binding_source, 2),
            (&binding_capabilities, 2),
            (&prompt_specification, 2),
            (&prompt, 5_i64),
            (&prompt_tokens, 4),
            (&call_record, 2),
            (&output, 5),
            (&output_tokens, 4),
            (&event, 2),
            (&receipt, 2),
        ] {
            connection
                .execute(
                    "INSERT INTO blobs(blob_id, byte_len, media_type, created_at_ms)
                     VALUES (?1, ?2, 'application/octet-stream', 1)",
                    rusqlite::params![blob_id, byte_len],
                )
                .expect("seed evidence blob");
        }

        let project = "PPPPPPPPPPPPPPPPPPPPPPPPPP";
        let campaign = "CCCCCCCCCCCCCCCCCCCCCCCCCC";
        let stage = "SSSSSSSSSSSSSSSSSSSSSSSSSS";
        let attempt = "AAAAAAAAAAAAAAAAAAAAAAAAAA";
        let trial_case = "TTTTTTTTTTTTTTTTTTTTTTTTTT";
        let revision = "RRRRRRRRRRRRRRRRRRRRRRRRRR";
        connection
            .execute(
                "INSERT INTO documents(document_id, relative_path, document_kind, created_at_ms)
                 VALUES ('document-eight', 'manuscript/eight.md', 'prose', 1)",
                [],
            )
            .expect("seed source document");
        connection
            .execute(
                "INSERT INTO artifacts(
                    artifact_id, blob_id, artifact_kind, media_type, metadata_json, created_at_ms
                 ) VALUES ('artifact-eight', ?1, 'document_revision', 'text/plain', '{}', 1)",
                [&prompt],
            )
            .expect("seed source artifact");
        connection
            .execute(
                "INSERT INTO revisions(
                    revision_id, document_id, parent_revision_id, artifact_id, reason, created_at_ms
                 ) VALUES (?1, 'document-eight', NULL, 'artifact-eight', 'test', 1)",
                [revision],
            )
            .expect("seed source revision");
        connection
            .execute(
                "INSERT INTO research_model_bindings(
                    binding_fingerprint, manifest_canonical_blob_id,
                    manifest_canonical_byte_len, manifest_artifact_hash,
                    binding_id, declared_role, model_sha256, model_byte_len,
                    tokenizer_sha256, projector_sha256, architecture, context_tokens,
                    capabilities_blob_id, capabilities_byte_len, capability_count, created_at_ms
                 ) VALUES (?1, ?2, 2, ?2, 'writer', 'base_writer', ?3, 1,
                           ?4, NULL, 'gemma', 128, ?5, 2, 1, 1)",
                rusqlite::params![
                    binding,
                    binding_canonical,
                    hash('2'),
                    tokenizer,
                    binding_capabilities,
                ],
            )
            .expect("seed compiled model binding");
        connection
            .execute(
                "INSERT INTO research_model_binding_sources(
                    binding_fingerprint, manifest_source_hash, manifest_source_blob_id,
                    manifest_source_byte_len, created_at_ms
                 ) VALUES (?1, ?2, ?2, 2, 1)",
                rusqlite::params![binding, binding_source],
            )
            .expect("seed exact binding source occurrence");
        connection
            .execute(
                "INSERT INTO research_verified_inference_batches(
                    batch_verification_fingerprint, project_id,
                    model_binding_fingerprint, model_binding_source_hash,
                    runtime_model_fingerprint,
                    prompt_specification_blob_id, prompt_specification_byte_len,
                    source_prompt_fingerprint, prompt_content_fingerprint,
                    treatment_recipe_fingerprint,
                    prompt_source_count, prompt_freeze_fingerprint, prompt_frozen_at_ms,
                    prompt_campaign_id, prompt_stage_id,
                    prompt_stage_attempt_id, prompt_trial_case_id,
                    tail_prompt_start_byte, tail_prompt_end_byte,
                    source_tail_revision_id, source_tail_blob_id,
                    source_tail_start_byte, source_tail_end_byte,
                    source_tail_origin, source_tail_assembly_id,
                    native_request_id,
                    exact_prompt_blob_id, exact_prompt_byte_len,
                    prompt_form, prompt_token_policy,
                    prompt_token_ids_blob_id, prompt_token_count,
                    prompt_token_ids_fingerprint, compiled_prompt_fingerprint,
                    expected_case_count, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 2, ?7, ?8, ?9, 1, ?10, 1,
                           ?11, ?12, ?13, ?14, 0, 5, ?15, ?16, 0, 5,
                           'live_manuscript', NULL, 'request',
                           ?16, 5, 'completion', 'no_bos_parse_special',
                           ?17, 1, ?18, ?19, 1, 1)",
                rusqlite::params![
                    batch,
                    project,
                    binding,
                    binding_source,
                    runtime_model,
                    prompt_specification,
                    hash('2'),
                    hash('3'),
                    hash('4'),
                    hash('5'),
                    campaign,
                    stage,
                    attempt,
                    trial_case,
                    revision,
                    prompt,
                    prompt_tokens,
                    prompt_token_fingerprint,
                    compiled_prompt,
                ],
            )
            .expect("seed verified batch header");

        assert!(
            connection
                .execute(
                    "INSERT INTO research_verified_inference_batch_seals(
                        batch_verification_fingerprint, completed_call_count,
                        cancelled_call_count, sealed_at_ms
                     ) VALUES (?1, 1, 0, 1)",
                    [&batch],
                )
                .is_err(),
            "a header without its declared cases must not seal"
        );

        connection
            .execute(
                "INSERT INTO research_verified_prompt_sources(
                    batch_verification_fingerprint, position, block_index, source_index,
                    source_kind, source_revision_id, source_blob_id,
                    source_start_byte, source_end_byte, assembly_id
                 ) VALUES (?1, 0, -1, 0, 'tail_live', ?2, ?3, 0, 5, NULL)",
                rusqlite::params![batch, revision, prompt],
            )
            .expect("bind exact current prompt tail");

        let insert_call = |call_id: &str, seed: &str, model_fingerprint: &str| {
            connection.execute(
                "INSERT INTO research_model_calls(
                    call_id, campaign_id, stage_id, stage_attempt_id, trial_case_id,
                    seed_decimal, model_fingerprint, tokenizer_fingerprint, prompt_fingerprint,
                    sampler_fingerprint, control_program_fingerprint, evidence_class,
                    verification_audit_fingerprint, call_record_blob_id, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                           'live_base_writer_claim', ?12, ?13, 1)",
                rusqlite::params![
                    call_id,
                    campaign,
                    stage,
                    attempt,
                    trial_case,
                    seed,
                    model_fingerprint,
                    tokenizer,
                    compiled_prompt,
                    hash('4'),
                    hash('5'),
                    case_verification,
                    call_record,
                ],
            )
        };
        assert!(
            insert_call("call-leading-zero", "07", &runtime_model).is_err(),
            "non-canonical seed text must fail closed"
        );
        assert!(
            insert_call("call-overflow", "18446744073709551616", &runtime_model).is_err(),
            "a seed beyond u64 must fail closed"
        );
        assert!(
            insert_call("call-uppercase", "7", &"A".repeat(64)).is_err(),
            "uppercase digest text must fail closed"
        );

        connection
            .execute(
                "INSERT INTO research_model_calls(
                    call_id, campaign_id, stage_id, stage_attempt_id, trial_case_id,
                    seed_decimal, model_fingerprint, tokenizer_fingerprint, prompt_fingerprint,
                    sampler_fingerprint, control_program_fingerprint, evidence_class,
                    verification_audit_fingerprint, call_record_blob_id, created_at_ms
                 ) VALUES ('call-eight', ?1, ?2, ?3, ?4, '7',
                           ?5, ?6, ?7, ?8, ?9,
                           'live_base_writer_claim', ?10, ?11, 1)",
                rusqlite::params![
                    campaign,
                    stage,
                    attempt,
                    trial_case,
                    runtime_model,
                    tokenizer,
                    compiled_prompt,
                    hash('4'),
                    hash('5'),
                    case_verification,
                    call_record,
                ],
            )
            .expect("seed completed call");
        connection
            .execute(
                "INSERT INTO research_call_terminals(
                    call_id, status, raw_output_blob_id, raw_output_byte_len,
                    token_ids_blob_id, token_count, token_ids_fingerprint,
                    raw_event_stream_blob_id, backend_receipt_blob_id,
                    terminal_message, created_at_ms
                 ) VALUES ('call-eight', 'completed', ?1, 5, ?2, 1, ?3, ?4, ?5, NULL, 1)",
                rusqlite::params![output, output_tokens, binding, event, receipt],
            )
            .expect("seed completed terminal");
        assert!(
            connection
                .execute(
                    "INSERT INTO research_verified_inference_batch_calls(
                        batch_verification_fingerprint, position, call_id,
                        campaign_id, stage_id, stage_attempt_id, trial_case_id,
                        seed_decimal, outcome, case_verification_fingerprint
                     ) VALUES (?1, 0, 'call-eight', ?2, ?3, ?4, ?5,
                               '7', 'completed', ?6)",
                    rusqlite::params![
                        batch,
                        campaign,
                        stage,
                        attempt,
                        trial_case,
                        case_verification,
                    ],
                )
                .is_err(),
            "a completed case without its one-to-one projection evidence must fail"
        );
        connection
            .execute(
                "INSERT INTO research_completed_call_evidence(
                    call_id, raw_output_byte_len, has_output_projection,
                    displayed_output_blob_id, displayed_output_byte_len,
                    displayed_start_byte, displayed_end_byte,
                    endpoint_tail_start_byte, endpoint_tail_end_byte,
                    stop_suffix_start_byte, stop_suffix_end_byte,
                    terminal_sampled_token_id, verification_fingerprint, created_at_ms
                 ) VALUES ('call-eight', 5, 0, NULL, NULL, NULL, NULL,
                           NULL, NULL, NULL, NULL, NULL, ?1, 1)",
                [&case_verification],
            )
            .expect("bind explicit absent output projection");
        assert!(
            connection
                .execute(
                    "INSERT INTO research_verified_inference_batch_calls(
                        batch_verification_fingerprint, position, call_id,
                        campaign_id, stage_id, stage_attempt_id, trial_case_id,
                        seed_decimal, outcome, case_verification_fingerprint
                     ) VALUES (?1, 0, 'call-eight', 'XXXXXXXXXXXXXXXXXXXXXXXXXX',
                               ?2, ?3, ?4, '7', 'completed', ?5)",
                    rusqlite::params![batch, stage, attempt, trial_case, case_verification],
                )
                .is_err(),
            "batch scope substitution must fail closed"
        );
        connection
            .execute(
                "INSERT INTO research_verified_inference_batch_calls(
                    batch_verification_fingerprint, position, call_id,
                    campaign_id, stage_id, stage_attempt_id, trial_case_id,
                    seed_decimal, outcome, case_verification_fingerprint
                 ) VALUES (?1, 0, 'call-eight', ?2, ?3, ?4, ?5,
                           '7', 'completed', ?6)",
                rusqlite::params![
                    batch,
                    campaign,
                    stage,
                    attempt,
                    trial_case,
                    case_verification,
                ],
            )
            .expect("bind exact completed case");
        connection
            .execute(
                "INSERT INTO research_verified_inference_batch_seals(
                    batch_verification_fingerprint, completed_call_count,
                    cancelled_call_count, sealed_at_ms
                 ) VALUES (?1, 1, 0, 1)",
                [&batch],
            )
            .expect("seal complete batch");

        assert!(
            connection
                .execute(
                    "UPDATE research_verified_inference_batches
                     SET native_request_id = 'changed'
                     WHERE batch_verification_fingerprint = ?1",
                    [&batch],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "DELETE FROM research_verified_inference_batch_seals
                     WHERE batch_verification_fingerprint = ?1",
                    [&batch],
                )
                .is_err()
        );
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

    #[test]
    fn version_nine_adds_strict_immutable_research_execution_groups() {
        let mut connection = Connection::open_in_memory().expect("in-memory SQLite");
        configure(&connection).expect("configure SQLite");
        migrate(&mut connection).expect("migrate current schema");

        let tables = [
            "research_execution_records",
            "research_campaigns",
            "research_trial_specs",
            "research_campaign_stage_specs",
            "research_campaign_stage_dependencies",
            "research_campaign_trial_dependencies",
            "research_campaign_stage_attempts",
            "research_campaign_trial_attempts",
            "research_campaign_events",
            "research_trial_events",
            "research_campaign_budget_reservations",
            "research_campaign_budget_charges",
            "research_campaign_trial_budget_reservations",
            "research_campaign_trial_budget_charges",
            "research_campaign_search_decisions",
            "research_story_graphs",
            "research_story_states",
            "research_prompt_masks",
            "research_backtranslation_proposals",
            "research_backtranslation_auditions",
            "research_backtranslation_audition_batches",
            "research_backtranslation_audition_evaluator_receipts",
            "research_backtranslation_acceptances",
            "research_evaluation_tasks",
            "research_evaluation_receipts",
            "research_evidence_spans",
            "research_pairwise_assignments",
            "research_score_vectors",
            "research_candidate_descriptors",
            "research_preference_labels",
            "research_archive_snapshots",
            "research_benchmark_suites",
            "research_benchmark_seals",
            "research_benchmark_contenders",
            "research_benchmark_runs",
            "research_benchmark_journals",
            "research_human_label_packets",
            "research_benchmark_results",
        ];
        for table in tables {
            let strict: i64 = connection
                .query_row(
                    "SELECT strict FROM pragma_table_list
                     WHERE schema = 'main' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap_or_else(|error| panic!("read STRICT status for {table}: {error}"));
            assert_eq!(strict, 1, "{table} must be STRICT");

            for operation in ["before update", "before delete"] {
                let trigger_count: i64 = connection
                    .query_row(
                        "SELECT COUNT(*) FROM sqlite_schema
                         WHERE type = 'trigger' AND tbl_name = ?1
                           AND instr(lower(sql), ?2) > 0",
                        rusqlite::params![table, operation],
                        |row| row.get(0),
                    )
                    .expect("inspect immutability trigger");
                assert_eq!(
                    trigger_count, 1,
                    "{table} must have exactly one {operation} trigger"
                );
            }
        }
    }

    #[test]
    fn version_nine_rejects_unsealed_records_broken_event_chains_and_budget_overruns() {
        let mut connection = Connection::open_in_memory().expect("in-memory SQLite");
        configure(&connection).expect("configure SQLite");
        migrate(&mut connection).expect("migrate current schema");
        let campaign_id = seed_version_nine_campaign(&connection);
        verify_version_nine_campaign_event_chain(&connection, campaign_id);
        verify_version_nine_budget_reconciliation(&connection, campaign_id);
        verify_version_nine_trial_budget_and_attempt_retries(&connection, campaign_id);
    }

    #[test]
    fn version_nine_prompt_masks_distinguish_surface_bytes_from_fim_capability() {
        let mut connection = Connection::open_in_memory().expect("in-memory SQLite");
        configure(&connection).expect("configure SQLite");
        migrate(&mut connection).expect("migrate current schema");
        let campaign_id = seed_version_nine_campaign(&connection);
        verify_version_nine_budget_reconciliation(&connection, campaign_id);
        let stage_attempt_id = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
        let source_blob_id = seed_test_blob(&connection, b"exact mask source");
        let rendered_blob_id = seed_test_blob(&connection, b"masked source");

        let fim_record = seed_execution_record(&connection, b"fim mask", "prompt_mask");
        assert!(
            connection
                .execute(
                    "INSERT INTO research_prompt_masks(
                        mask_fingerprint, campaign_id, stage_attempt_id, mask_kind,
                        source_blob_id, rendered_blob_id, backend_capability_fingerprint,
                        record_fingerprint, created_at_ms
                     ) VALUES (?1, ?2, ?3, 'model_specific_fim', ?4, ?5, ?6, ?7, 8)",
                    rusqlite::params![
                        loom_types::BlobId::digest(b"invalid rendered FIM").to_string(),
                        campaign_id,
                        stage_attempt_id,
                        source_blob_id,
                        rendered_blob_id,
                        loom_types::BlobId::digest(b"fim capability").to_string(),
                        fim_record,
                    ],
                )
                .is_err(),
            "FIM evidence must not fabricate rendered control bytes"
        );

        let surface_record =
            seed_execution_record(&connection, b"surface mask missing output", "prompt_mask");
        assert!(
            connection
                .execute(
                    "INSERT INTO research_prompt_masks(
                        mask_fingerprint, campaign_id, stage_attempt_id, mask_kind,
                        source_blob_id, rendered_blob_id, backend_capability_fingerprint,
                        record_fingerprint, created_at_ms
                     ) VALUES (?1, ?2, ?3, 'entity', ?4, NULL, NULL, ?5, 8)",
                    rusqlite::params![
                        loom_types::BlobId::digest(b"missing surface output").to_string(),
                        campaign_id,
                        stage_attempt_id,
                        source_blob_id,
                        surface_record,
                    ],
                )
                .is_err(),
            "surface masks require their exact rendered bytes"
        );

        let valid_fim_record = seed_execution_record(&connection, b"valid FIM mask", "prompt_mask");
        connection
            .execute(
                "INSERT INTO research_prompt_masks(
                    mask_fingerprint, campaign_id, stage_attempt_id, mask_kind,
                    source_blob_id, rendered_blob_id, backend_capability_fingerprint,
                    record_fingerprint, created_at_ms
                 ) VALUES (?1, ?2, ?3, 'model_specific_fim', ?4, NULL, ?5, ?6, 8)",
                rusqlite::params![
                    loom_types::BlobId::digest(b"valid FIM binding").to_string(),
                    campaign_id,
                    stage_attempt_id,
                    source_blob_id,
                    loom_types::BlobId::digest(b"fim capability").to_string(),
                    valid_fim_record,
                ],
            )
            .expect("FIM persists without invented rendered bytes");

        let valid_surface_record =
            seed_execution_record(&connection, b"valid surface mask", "prompt_mask");
        connection
            .execute(
                "INSERT INTO research_prompt_masks(
                    mask_fingerprint, campaign_id, stage_attempt_id, mask_kind,
                    source_blob_id, rendered_blob_id, backend_capability_fingerprint,
                    record_fingerprint, created_at_ms
                 ) VALUES (?1, ?2, ?3, 'entity', ?4, ?5, NULL, ?6, 8)",
                rusqlite::params![
                    loom_types::BlobId::digest(b"valid surface binding").to_string(),
                    campaign_id,
                    stage_attempt_id,
                    source_blob_id,
                    rendered_blob_id,
                    valid_surface_record,
                ],
            )
            .expect("surface mask persists with exact rendered bytes");
    }

    fn seed_execution_record(connection: &Connection, payload: &[u8], kind: &str) -> String {
        let fingerprint = loom_types::BlobId::digest(payload).to_string();
        connection
            .execute(
                "INSERT INTO blobs(blob_id, byte_len, media_type, created_at_ms)
                 VALUES (?1, ?2, 'application/json', 1)",
                rusqlite::params![
                    fingerprint,
                    i64::try_from(payload.len()).expect("test record length fits i64")
                ],
            )
            .expect("seed record blob");
        connection
            .execute(
                "INSERT INTO research_execution_records(
                    record_fingerprint, record_kind, record_blob_id, created_at_ms
                 ) VALUES (?1, ?2, ?1, 1)",
                rusqlite::params![fingerprint, kind],
            )
            .expect("seed execution record");
        fingerprint
    }

    fn seed_campaign_trial_run(
        connection: &Connection,
        run_id: &str,
        trial_fingerprint: &str,
        campaign_id: &str,
        payload: &[u8],
    ) {
        let record = seed_execution_record(connection, payload, "trial_run");
        connection
            .execute(
                "INSERT INTO research_trial_runs(
                    trial_run_id, trial_fingerprint, origin_kind, origin_campaign_id,
                    record_fingerprint, created_at_ms
                 ) VALUES (?1, ?2, 'campaign', ?3, ?4, 12)",
                rusqlite::params![run_id, trial_fingerprint, campaign_id, record],
            )
            .expect("seed campaign trial run");
    }

    fn seed_test_blob(connection: &Connection, payload: &[u8]) -> String {
        let fingerprint = loom_types::BlobId::digest(payload).to_string();
        connection
            .execute(
                "INSERT INTO blobs(blob_id, byte_len, media_type, created_at_ms)
                 VALUES (?1, ?2, 'application/octet-stream', 1)",
                rusqlite::params![
                    fingerprint,
                    i64::try_from(payload.len()).expect("test blob length fits i64")
                ],
            )
            .expect("seed test blob");
        fingerprint
    }

    fn seed_version_nine_campaign(connection: &Connection) -> &'static str {
        let campaign_record = seed_execution_record(connection, b"campaign", "campaign");
        let manifest_blob = loom_types::BlobId::digest(b"manifest").to_string();
        connection
            .execute(
                "INSERT INTO blobs(blob_id, byte_len, media_type, created_at_ms)
                 VALUES (?1, 8, 'application/toml', 1)",
                [&manifest_blob],
            )
            .expect("seed manifest blob");
        let campaign_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        connection
            .execute(
                "INSERT INTO research_campaigns(
                    campaign_id, campaign_fingerprint, project_id, manifest_source_blob_id,
                    manifest_fingerprint, project_input_fingerprint, seed_decimal,
                    maximum_writer_tokens, maximum_controller_tokens,
                    maximum_evaluations, maximum_wall_time_ms,
                    record_fingerprint, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, '0', 100, 100, 2, 1000, ?7, 1)",
                rusqlite::params![
                    campaign_id,
                    loom_types::BlobId::digest(b"frozen-campaign").to_string(),
                    "01ARZ3NDEKTSV4RRFFQ69G5FAW",
                    manifest_blob,
                    "1".repeat(64),
                    "2".repeat(64),
                    campaign_record,
                ],
            )
            .expect("insert campaign");
        campaign_id
    }

    fn verify_version_nine_campaign_event_chain(connection: &Connection, campaign_id: &str) {
        let started_zero = seed_execution_record(connection, b"started-zero", "campaign_event");
        assert!(
            connection
                .execute(
                    "INSERT INTO research_campaign_events(
                        campaign_id, event_index, previous_event_fingerprint,
                        event_fingerprint, event_kind, trial_attempt_id,
                        attempt_outcome, record_fingerprint, occurred_at_ms
                     ) VALUES (?1, 0, NULL, ?2, 'started', NULL, NULL, ?3, 2)",
                    rusqlite::params![
                        campaign_id,
                        loom_types::BlobId::digest(b"started-zero-event").to_string(),
                        started_zero,
                    ],
                )
                .is_err(),
            "a campaign must begin prepared"
        );

        let prepared = seed_execution_record(connection, b"prepared", "campaign_event");
        let prepared_event = loom_types::BlobId::digest(b"prepared-event").to_string();
        connection
            .execute(
                "INSERT INTO research_campaign_events(
                    campaign_id, event_index, previous_event_fingerprint,
                    event_fingerprint, event_kind, trial_attempt_id,
                    attempt_outcome, record_fingerprint, occurred_at_ms
                 ) VALUES (?1, 0, NULL, ?2, 'prepared', NULL, NULL, ?3, 2)",
                rusqlite::params![campaign_id, prepared_event, prepared],
            )
            .expect("prepare campaign");
        let broken_chain = seed_execution_record(connection, b"broken-chain", "campaign_event");
        assert!(
            connection
                .execute(
                    "INSERT INTO research_campaign_events(
                        campaign_id, event_index, previous_event_fingerprint,
                        event_fingerprint, event_kind, trial_attempt_id,
                        attempt_outcome, record_fingerprint, occurred_at_ms
                     ) VALUES (?1, 1, ?2, ?3, 'started', NULL, NULL, ?4, 3)",
                    rusqlite::params![
                        campaign_id,
                        loom_types::BlobId::digest(b"not-the-prior-event").to_string(),
                        loom_types::BlobId::digest(b"broken-chain-event").to_string(),
                        broken_chain,
                    ],
                )
                .is_err(),
            "a campaign event must name the exact prior event"
        );
        let started = seed_execution_record(connection, b"started", "campaign_event");
        connection
            .execute(
                "INSERT INTO research_campaign_events(
                    campaign_id, event_index, previous_event_fingerprint,
                    event_fingerprint, event_kind, trial_attempt_id,
                    attempt_outcome, record_fingerprint, occurred_at_ms
                 ) VALUES (?1, 1, ?2, ?3, 'started', NULL, NULL, ?4, 3)",
                rusqlite::params![
                    campaign_id,
                    prepared_event,
                    loom_types::BlobId::digest(b"started-event").to_string(),
                    started,
                ],
            )
            .expect("start campaign");
        assert!(
            connection
                .execute(
                    "UPDATE research_campaign_events SET occurred_at_ms = 4
                     WHERE campaign_id = ?1 AND event_index = 1",
                    [campaign_id],
                )
                .is_err(),
            "campaign events are immutable"
        );
    }

    #[allow(clippy::too_many_lines)]
    fn verify_version_nine_budget_reconciliation(connection: &Connection, campaign_id: &str) {
        let trial_record = seed_execution_record(connection, b"trial", "trial_spec");
        let trial_fingerprint = loom_types::BlobId::digest(b"frozen-trial").to_string();
        connection
            .execute(
                "INSERT INTO research_trial_specs(
                    trial_fingerprint, campaign_id, trial_case_id,
                    treatment_fingerprint, prompt_content_fingerprint,
                    model_binding_fingerprint, expected_writer_call_count,
                    declared_writer_token_maximum,
                    maximum_writer_tokens, maximum_controller_tokens,
                    maximum_evaluations, maximum_wall_time_ms,
                    record_fingerprint, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 100, 100, 100, 2, 1000, ?7, 4)",
                rusqlite::params![
                    trial_fingerprint,
                    campaign_id,
                    "01ARZ3NDEKTSV4RRFFQ69G5FB0",
                    "3".repeat(64),
                    "4".repeat(64),
                    "5".repeat(64),
                    trial_record,
                ],
            )
            .expect("insert frozen trial");

        let stage_record = seed_execution_record(connection, b"stage", "stage_spec");
        let stage_id = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
        connection
            .execute(
                "INSERT INTO research_campaign_stage_specs(
                    stage_id, trial_fingerprint, stage_ordinal, stage_kind,
                    stage_spec_fingerprint,
                    maximum_writer_tokens, maximum_controller_tokens,
                    maximum_evaluations, maximum_wall_time_ms,
                    record_fingerprint, created_at_ms
                 ) VALUES (?1, ?2, 0, 'generate', ?3, 100, 20, 2, 1000, ?4, 4)",
                rusqlite::params![
                    stage_id,
                    trial_fingerprint,
                    loom_types::BlobId::digest(b"generate stage spec").to_string(),
                    stage_record,
                ],
            )
            .expect("insert stage");
        let run_record = seed_execution_record(connection, b"trial-run", "trial_run");
        let trial_run_id = "01ARZ3NDEKTSV4RRFFQ69G5FB1";
        connection
            .execute(
                "INSERT INTO research_trial_runs(
                    trial_run_id, trial_fingerprint, origin_kind,
                    record_fingerprint, created_at_ms
                 ) VALUES (?1, ?2, 'standalone', ?3, 5)",
                rusqlite::params![trial_run_id, trial_fingerprint, run_record],
            )
            .expect("insert trial run");
        let attempt_record = seed_execution_record(connection, b"attempt", "stage_attempt");
        let attempt_id = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
        connection
            .execute(
                "INSERT INTO research_campaign_stage_attempts(
                    stage_attempt_id, trial_run_id, stage_id, attempt_ordinal,
                    record_fingerprint, created_at_ms
                 ) VALUES (?1, ?2, ?3, 1, ?4, 5)",
                rusqlite::params![attempt_id, trial_run_id, stage_id, attempt_record],
            )
            .expect("insert attempt");

        let reservation_record =
            seed_execution_record(connection, b"reservation", "budget_reservation");
        let reservation_id = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";
        connection
            .execute(
                "INSERT INTO research_campaign_budget_reservations(
                    reservation_id, campaign_id, stage_attempt_id,
                    writer_tokens, controller_tokens, evaluations, wall_time_ms,
                    record_fingerprint, reserved_at_ms
                 ) VALUES (?1, ?2, ?3, 80, 10, 1, 900, ?4, 6)",
                rusqlite::params![reservation_id, campaign_id, attempt_id, reservation_record],
            )
            .expect("reserve budget");
        let excessive_charge = seed_execution_record(connection, b"excess-charge", "budget_charge");
        assert!(
            connection
                .execute(
                    "INSERT INTO research_campaign_budget_charges(
                        reservation_id, writer_tokens, controller_tokens,
                        evaluations, wall_time_ms, record_fingerprint, charged_at_ms
                     ) VALUES (?1, 81, 10, 1, 900, ?2, 7)",
                    rusqlite::params![reservation_id, excessive_charge],
                )
                .is_err(),
            "actual charges cannot exceed the reservation"
        );
        let charge = seed_execution_record(connection, b"charge", "budget_charge");
        connection
            .execute(
                "INSERT INTO research_campaign_budget_charges(
                    reservation_id, writer_tokens, controller_tokens,
                    evaluations, wall_time_ms, record_fingerprint, charged_at_ms
                 ) VALUES (?1, 79, 9, 1, 850, ?2, 7)",
                rusqlite::params![reservation_id, charge],
            )
            .expect("reconcile charge");

        let zero_attempt =
            seed_execution_record(connection, b"zero-stage-attempt", "stage_attempt");
        assert!(
            connection
                .execute(
                    "INSERT INTO research_campaign_stage_attempts(
                        stage_attempt_id, trial_run_id, stage_id, attempt_ordinal,
                        record_fingerprint, created_at_ms
                     ) VALUES (?1, ?2, ?3, 0, ?4, 8)",
                    rusqlite::params![
                        "01ARZ3NDEKTSV4RRFFQ69G5FB1",
                        trial_run_id,
                        stage_id,
                        zero_attempt,
                    ],
                )
                .is_err(),
            "attempt ordinals are one-based"
        );
        let gap_attempt = seed_execution_record(connection, b"gap-stage-attempt", "stage_attempt");
        assert!(
            connection
                .execute(
                    "INSERT INTO research_campaign_stage_attempts(
                        stage_attempt_id, trial_run_id, stage_id, attempt_ordinal,
                        record_fingerprint, created_at_ms
                     ) VALUES (?1, ?2, ?3, 3, ?4, 8)",
                    rusqlite::params![
                        "01ARZ3NDEKTSV4RRFFQ69G5FB2",
                        trial_run_id,
                        stage_id,
                        gap_attempt,
                    ],
                )
                .is_err(),
            "a stage retry cannot skip an ordinal"
        );

        let prepared_record = seed_execution_record(connection, b"trial-prepared", "trial_event");
        let prepared_fingerprint = loom_types::BlobId::digest(b"trial prepared event").to_string();
        connection
            .execute(
                "INSERT INTO research_trial_events(
                    trial_run_id, trial_fingerprint, event_index, previous_event_fingerprint,
                    event_fingerprint, event_kind, stage_attempt_id,
                    attempt_outcome, record_fingerprint, occurred_at_ms
                 ) VALUES (?1, ?2, 0, NULL, ?3, 'prepared', NULL, NULL, ?4, 8)",
                rusqlite::params![
                    trial_run_id,
                    trial_fingerprint,
                    prepared_fingerprint,
                    prepared_record,
                ],
            )
            .expect("prepare trial journal");
        let reserved_record = seed_execution_record(connection, b"attempt-reserved", "trial_event");
        let reserved_fingerprint =
            loom_types::BlobId::digest(b"attempt reserved event").to_string();
        connection
            .execute(
                "INSERT INTO research_trial_events(
                    trial_run_id, trial_fingerprint, event_index, previous_event_fingerprint,
                    event_fingerprint, event_kind, stage_attempt_id,
                    attempt_outcome, record_fingerprint, occurred_at_ms
                 ) VALUES (?1, ?2, 1, ?3, ?4, 'attempt_reserved', ?5, NULL, ?6, 9)",
                rusqlite::params![
                    trial_run_id,
                    trial_fingerprint,
                    prepared_fingerprint,
                    reserved_fingerprint,
                    attempt_id,
                    reserved_record,
                ],
            )
            .expect("reserve first stage attempt");
        let finished_record = seed_execution_record(connection, b"attempt-finished", "trial_event");
        connection
            .execute(
                "INSERT INTO research_trial_events(
                    trial_run_id, trial_fingerprint, event_index, previous_event_fingerprint,
                    event_fingerprint, event_kind, stage_attempt_id,
                    attempt_outcome, record_fingerprint, occurred_at_ms
                 ) VALUES (?1, ?2, 2, ?3, ?4, 'attempt_finished', ?5, 'failed', ?6, 10)",
                rusqlite::params![
                    trial_run_id,
                    trial_fingerprint,
                    reserved_fingerprint,
                    loom_types::BlobId::digest(b"attempt failed event").to_string(),
                    attempt_id,
                    finished_record,
                ],
            )
            .expect("finish first stage attempt after exact charge");
        let retry_record =
            seed_execution_record(connection, b"retry-stage-attempt", "stage_attempt");
        connection
            .execute(
                "INSERT INTO research_campaign_stage_attempts(
                    stage_attempt_id, trial_run_id, stage_id, attempt_ordinal,
                    record_fingerprint, created_at_ms
                 ) VALUES (?1, ?2, ?3, 2, ?4, 11)",
                rusqlite::params![
                    "01ARZ3NDEKTSV4RRFFQ69G5FB3",
                    trial_run_id,
                    stage_id,
                    retry_record,
                ],
            )
            .expect("one-based contiguous retry follows failed terminal");
    }

    #[allow(clippy::too_many_lines)]
    fn verify_version_nine_trial_budget_and_attempt_retries(
        connection: &Connection,
        campaign_id: &str,
    ) {
        let trial_fingerprint = loom_types::BlobId::digest(b"frozen-trial").to_string();
        let trial_attempt_id = "01ARZ3NDEKTSV4RRFFQ69G5FB4";
        seed_campaign_trial_run(
            connection,
            trial_attempt_id,
            &trial_fingerprint,
            campaign_id,
            b"campaign-trial-run-one",
        );
        let first_attempt_record = seed_execution_record(
            connection,
            b"campaign-trial-attempt-one",
            "campaign_trial_attempt",
        );
        connection
            .execute(
                "INSERT INTO research_campaign_trial_attempts(
                    trial_attempt_id, trial_fingerprint, attempt_ordinal,
                    record_fingerprint, created_at_ms
                 ) VALUES (?1, ?2, 1, ?3, 12)",
                rusqlite::params![trial_attempt_id, trial_fingerprint, first_attempt_record],
            )
            .expect("first campaign trial attempt is ordinal one");
        let zero_attempt_record = seed_execution_record(
            connection,
            b"campaign-trial-attempt-zero",
            "campaign_trial_attempt",
        );
        seed_campaign_trial_run(
            connection,
            "01ARZ3NDEKTSV4RRFFQ69G5FB5",
            &trial_fingerprint,
            campaign_id,
            b"campaign-trial-run-zero",
        );
        assert!(
            connection
                .execute(
                    "INSERT INTO research_campaign_trial_attempts(
                        trial_attempt_id, trial_fingerprint, attempt_ordinal,
                        record_fingerprint, created_at_ms
                     ) VALUES (?1, ?2, 0, ?3, 12)",
                    rusqlite::params![
                        "01ARZ3NDEKTSV4RRFFQ69G5FB5",
                        trial_fingerprint,
                        zero_attempt_record,
                    ],
                )
                .is_err(),
            "campaign trial attempts are one-based"
        );

        let trial_reserved_record =
            seed_execution_record(connection, b"campaign-trial-reserved", "campaign_event");
        let trial_reserved_fingerprint =
            loom_types::BlobId::digest(b"campaign trial reserved event").to_string();
        assert!(
            connection
                .execute(
                    "INSERT INTO research_campaign_events(
                        campaign_id, event_index, previous_event_fingerprint,
                        event_fingerprint, event_kind, trial_attempt_id,
                        attempt_outcome, record_fingerprint, occurred_at_ms
                     ) VALUES (?1, 2, ?2, ?3, 'trial_reserved', ?4, NULL, ?5, 13)",
                    rusqlite::params![
                        campaign_id,
                        loom_types::BlobId::digest(b"started-event").to_string(),
                        trial_reserved_fingerprint,
                        trial_attempt_id,
                        trial_reserved_record,
                    ],
                )
                .is_err(),
            "campaign trial reservation event requires exact frozen budget first"
        );

        let reservation_record = seed_execution_record(
            connection,
            b"campaign-trial-reservation",
            "budget_reservation",
        );
        let reservation_id = "01ARZ3NDEKTSV4RRFFQ69G5FB6";
        assert!(
            connection
                .execute(
                    "INSERT INTO research_campaign_trial_budget_reservations(
                        reservation_id, trial_attempt_id, writer_tokens,
                        controller_tokens, evaluations, wall_time_ms,
                        record_fingerprint, reserved_at_ms
                     ) VALUES (?1, ?2, 99, 100, 2, 1000, ?3, 13)",
                    rusqlite::params![reservation_id, trial_attempt_id, reservation_record],
                )
                .is_err(),
            "campaign trial reservation must equal, not merely fit, its frozen maximum"
        );
        connection
            .execute(
                "INSERT INTO research_campaign_trial_budget_reservations(
                    reservation_id, trial_attempt_id, writer_tokens,
                    controller_tokens, evaluations, wall_time_ms,
                    record_fingerprint, reserved_at_ms
                 ) VALUES (?1, ?2, 100, 100, 2, 1000, ?3, 13)",
                rusqlite::params![reservation_id, trial_attempt_id, reservation_record],
            )
            .expect("reserve exact frozen trial maximum");
        connection
            .execute(
                "INSERT INTO research_campaign_events(
                    campaign_id, event_index, previous_event_fingerprint,
                    event_fingerprint, event_kind, trial_attempt_id,
                    attempt_outcome, record_fingerprint, occurred_at_ms
                 ) VALUES (?1, 2, ?2, ?3, 'trial_reserved', ?4, NULL, ?5, 13)",
                rusqlite::params![
                    campaign_id,
                    loom_types::BlobId::digest(b"started-event").to_string(),
                    trial_reserved_fingerprint,
                    trial_attempt_id,
                    trial_reserved_record,
                ],
            )
            .expect("record trial reservation after budget row");
        let dispatched_record =
            seed_execution_record(connection, b"campaign-trial-dispatched", "campaign_event");
        let dispatched_fingerprint =
            loom_types::BlobId::digest(b"campaign trial dispatched event").to_string();
        connection
            .execute(
                "INSERT INTO research_campaign_events(
                    campaign_id, event_index, previous_event_fingerprint,
                    event_fingerprint, event_kind, trial_attempt_id,
                    attempt_outcome, record_fingerprint, occurred_at_ms
                 ) VALUES (?1, 3, ?2, ?3, 'trial_dispatched', ?4, NULL, ?5, 14)",
                rusqlite::params![
                    campaign_id,
                    trial_reserved_fingerprint,
                    dispatched_fingerprint,
                    trial_attempt_id,
                    dispatched_record,
                ],
            )
            .expect("dispatch reserved campaign trial");

        let finished_record =
            seed_execution_record(connection, b"campaign-trial-finished", "campaign_event");
        let finished_fingerprint =
            loom_types::BlobId::digest(b"campaign trial finished event").to_string();
        assert!(
            connection
                .execute(
                    "INSERT INTO research_campaign_events(
                        campaign_id, event_index, previous_event_fingerprint,
                        event_fingerprint, event_kind, trial_attempt_id,
                        attempt_outcome, record_fingerprint, occurred_at_ms
                     ) VALUES (?1, 4, ?2, ?3, 'trial_finished', ?4, 'failed', ?5, 15)",
                    rusqlite::params![
                        campaign_id,
                        dispatched_fingerprint,
                        finished_fingerprint,
                        trial_attempt_id,
                        finished_record,
                    ],
                )
                .is_err(),
            "trial terminal event requires reconciled charge"
        );
        let excessive_charge =
            seed_execution_record(connection, b"campaign-trial-excess-charge", "budget_charge");
        assert!(
            connection
                .execute(
                    "INSERT INTO research_campaign_trial_budget_charges(
                        reservation_id, writer_tokens, controller_tokens,
                        evaluations, wall_time_ms, record_fingerprint, charged_at_ms
                     ) VALUES (?1, 101, 100, 2, 1000, ?2, 15)",
                    rusqlite::params![reservation_id, excessive_charge],
                )
                .is_err(),
            "campaign trial charge cannot exceed its reservation"
        );
        let charge = seed_execution_record(connection, b"campaign-trial-charge", "budget_charge");
        connection
            .execute(
                "INSERT INTO research_campaign_trial_budget_charges(
                    reservation_id, writer_tokens, controller_tokens,
                    evaluations, wall_time_ms, record_fingerprint, charged_at_ms
                 ) VALUES (?1, 90, 80, 2, 900, ?2, 15)",
                rusqlite::params![reservation_id, charge],
            )
            .expect("reconcile campaign trial charge");
        connection
            .execute(
                "INSERT INTO research_campaign_events(
                    campaign_id, event_index, previous_event_fingerprint,
                    event_fingerprint, event_kind, trial_attempt_id,
                    attempt_outcome, record_fingerprint, occurred_at_ms
                 ) VALUES (?1, 4, ?2, ?3, 'trial_finished', ?4, 'failed', ?5, 15)",
                rusqlite::params![
                    campaign_id,
                    dispatched_fingerprint,
                    finished_fingerprint,
                    trial_attempt_id,
                    finished_record,
                ],
            )
            .expect("finish campaign trial after charge");

        let gap_record =
            seed_execution_record(connection, b"campaign-trial-gap", "campaign_trial_attempt");
        seed_campaign_trial_run(
            connection,
            "01ARZ3NDEKTSV4RRFFQ69G5FB7",
            &trial_fingerprint,
            campaign_id,
            b"campaign-trial-run-gap",
        );
        assert!(
            connection
                .execute(
                    "INSERT INTO research_campaign_trial_attempts(
                        trial_attempt_id, trial_fingerprint, attempt_ordinal,
                        record_fingerprint, created_at_ms
                     ) VALUES (?1, ?2, 3, ?3, 16)",
                    rusqlite::params!["01ARZ3NDEKTSV4RRFFQ69G5FB7", trial_fingerprint, gap_record,],
                )
                .is_err(),
            "campaign trial retry cannot skip an ordinal"
        );
        let retry_record = seed_execution_record(
            connection,
            b"campaign-trial-retry",
            "campaign_trial_attempt",
        );
        seed_campaign_trial_run(
            connection,
            "01ARZ3NDEKTSV4RRFFQ69G5FB8",
            &trial_fingerprint,
            campaign_id,
            b"campaign-trial-run-retry",
        );
        connection
            .execute(
                "INSERT INTO research_campaign_trial_attempts(
                    trial_attempt_id, trial_fingerprint, attempt_ordinal,
                    record_fingerprint, created_at_ms
                 ) VALUES (?1, ?2, 2, ?3, 16)",
                rusqlite::params![
                    "01ARZ3NDEKTSV4RRFFQ69G5FB8",
                    trial_fingerprint,
                    retry_record,
                ],
            )
            .expect("contiguous retry follows failed charged terminal");
    }
}
