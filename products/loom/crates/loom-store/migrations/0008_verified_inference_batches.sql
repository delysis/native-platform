-- Batch-level admission binds the exact prompt and every terminal branch to
-- one consumed runtime authority. These rows remain audit evidence only;
-- only session-bound leases returned by ProjectStore authorize assembly.

CREATE TABLE research_model_bindings (
    binding_fingerprint TEXT PRIMARY KEY CHECK (
        length(binding_fingerprint) = 64
        AND binding_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    manifest_canonical_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    manifest_canonical_byte_len INTEGER NOT NULL CHECK (
        manifest_canonical_byte_len BETWEEN 1 AND 1048576
    ),
    manifest_artifact_hash TEXT NOT NULL CHECK (
        length(manifest_artifact_hash) = 64
        AND manifest_artifact_hash NOT GLOB '*[^0-9a-f]*'
        AND manifest_artifact_hash = manifest_canonical_blob_id
    ),
    binding_id TEXT NOT NULL CHECK (
        length(CAST(binding_id AS BLOB)) BETWEEN 1 AND 64
    ),
    declared_role TEXT NOT NULL CHECK (declared_role = 'base_writer'),
    model_sha256 TEXT NOT NULL CHECK (
        length(model_sha256) = 64
        AND model_sha256 NOT GLOB '*[^0-9a-f]*'
    ),
    model_byte_len INTEGER NOT NULL CHECK (model_byte_len > 0),
    tokenizer_sha256 TEXT NOT NULL CHECK (
        length(tokenizer_sha256) = 64
        AND tokenizer_sha256 NOT GLOB '*[^0-9a-f]*'
    ),
    projector_sha256 TEXT CHECK (
        projector_sha256 IS NULL
        OR (
            length(projector_sha256) = 64
            AND projector_sha256 NOT GLOB '*[^0-9a-f]*'
        )
    ),
    architecture TEXT NOT NULL CHECK (
        length(CAST(architecture AS BLOB)) BETWEEN 1 AND 64
    ),
    context_tokens INTEGER NOT NULL CHECK (context_tokens > 0),
    capabilities_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    capabilities_byte_len INTEGER NOT NULL CHECK (
        capabilities_byte_len BETWEEN 1 AND 65536
    ),
    capability_count INTEGER NOT NULL CHECK (capability_count BETWEEN 1 AND 64),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    UNIQUE(manifest_artifact_hash, binding_id)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_model_bindings_validate_insert
BEFORE INSERT ON research_model_bindings
WHEN NOT EXISTS (
        SELECT 1 FROM blobs artifact
        WHERE artifact.blob_id = NEW.manifest_canonical_blob_id
          AND artifact.byte_len = NEW.manifest_canonical_byte_len
    )
    OR NOT EXISTS (
        SELECT 1 FROM blobs capabilities
        WHERE capabilities.blob_id = NEW.capabilities_blob_id
          AND capabilities.byte_len = NEW.capabilities_byte_len
    )
BEGIN
    SELECT RAISE(ABORT, 'compiled model binding evidence is absent or inconsistent');
END;

CREATE TABLE research_model_binding_sources (
    binding_fingerprint TEXT NOT NULL REFERENCES research_model_bindings(binding_fingerprint) ON DELETE RESTRICT,
    manifest_source_hash TEXT NOT NULL CHECK (
        length(manifest_source_hash) = 64
        AND manifest_source_hash NOT GLOB '*[^0-9a-f]*'
    ),
    manifest_source_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    manifest_source_byte_len INTEGER NOT NULL CHECK (
        manifest_source_byte_len BETWEEN 1 AND 1048576
    ),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    PRIMARY KEY(binding_fingerprint, manifest_source_hash),
    CHECK (manifest_source_hash = manifest_source_blob_id)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_model_binding_sources_validate_insert
BEFORE INSERT ON research_model_binding_sources
WHEN NOT EXISTS (
    SELECT 1 FROM blobs source
    WHERE source.blob_id = NEW.manifest_source_blob_id
      AND source.byte_len = NEW.manifest_source_byte_len
)
BEGIN
    SELECT RAISE(ABORT, 'model binding source occurrence is absent or inconsistent');
END;

CREATE TABLE research_verified_inference_batches (
    batch_verification_fingerprint TEXT PRIMARY KEY CHECK (
        length(batch_verification_fingerprint) = 64
        AND batch_verification_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    project_id TEXT NOT NULL CHECK (length(project_id) = 26),
    model_binding_fingerprint TEXT NOT NULL REFERENCES research_model_bindings(binding_fingerprint) ON DELETE RESTRICT,
    model_binding_source_hash TEXT NOT NULL CHECK (
        length(model_binding_source_hash) = 64
        AND model_binding_source_hash NOT GLOB '*[^0-9a-f]*'
    ),
    runtime_model_fingerprint TEXT NOT NULL CHECK (
        length(runtime_model_fingerprint) = 64
        AND runtime_model_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    prompt_specification_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    prompt_specification_byte_len INTEGER NOT NULL CHECK (
        prompt_specification_byte_len BETWEEN 1 AND 134217728
    ),
    source_prompt_fingerprint TEXT NOT NULL CHECK (
        length(source_prompt_fingerprint) = 64
        AND source_prompt_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    prompt_content_fingerprint TEXT NOT NULL CHECK (
        length(prompt_content_fingerprint) = 64
        AND prompt_content_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    treatment_recipe_fingerprint TEXT NOT NULL CHECK (
        length(treatment_recipe_fingerprint) = 64
        AND treatment_recipe_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    prompt_source_count INTEGER NOT NULL CHECK (prompt_source_count BETWEEN 1 AND 64),
    prompt_freeze_fingerprint TEXT NOT NULL CHECK (
        length(prompt_freeze_fingerprint) = 64
        AND prompt_freeze_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    prompt_frozen_at_ms INTEGER NOT NULL CHECK (prompt_frozen_at_ms > 0),
    prompt_campaign_id TEXT NOT NULL CHECK (length(prompt_campaign_id) = 26),
    prompt_stage_id TEXT NOT NULL CHECK (length(prompt_stage_id) = 26),
    prompt_stage_attempt_id TEXT NOT NULL CHECK (length(prompt_stage_attempt_id) = 26),
    prompt_trial_case_id TEXT NOT NULL CHECK (length(prompt_trial_case_id) = 26),
    tail_prompt_start_byte INTEGER NOT NULL CHECK (tail_prompt_start_byte >= 0),
    tail_prompt_end_byte INTEGER NOT NULL CHECK (tail_prompt_end_byte > tail_prompt_start_byte),
    source_tail_revision_id TEXT CHECK (
        source_tail_revision_id IS NULL OR length(source_tail_revision_id) = 26
    ),
    source_tail_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    source_tail_start_byte INTEGER NOT NULL CHECK (source_tail_start_byte >= 0),
    source_tail_end_byte INTEGER NOT NULL CHECK (source_tail_end_byte > source_tail_start_byte),
    source_tail_origin TEXT NOT NULL CHECK (
        source_tail_origin IN ('live_manuscript', 'admitted_assembly')
    ),
    source_tail_assembly_id TEXT CHECK (
        source_tail_assembly_id IS NULL OR length(source_tail_assembly_id) = 26
    ),
    native_request_id TEXT NOT NULL CHECK (
        length(CAST(native_request_id AS BLOB)) BETWEEN 1 AND 256
    ),
    exact_prompt_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    exact_prompt_byte_len INTEGER NOT NULL CHECK (
        exact_prompt_byte_len BETWEEN 1 AND 16777216
    ),
    prompt_form TEXT NOT NULL CHECK (prompt_form = 'completion'),
    prompt_token_policy TEXT NOT NULL CHECK (
        prompt_token_policy = 'no_bos_parse_special'
    ),
    prompt_token_ids_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    prompt_token_count INTEGER NOT NULL CHECK (
        prompt_token_count BETWEEN 1 AND 1048576
    ),
    prompt_token_ids_fingerprint TEXT NOT NULL CHECK (
        length(prompt_token_ids_fingerprint) = 64
        AND prompt_token_ids_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    compiled_prompt_fingerprint TEXT NOT NULL CHECK (
        length(compiled_prompt_fingerprint) = 64
        AND compiled_prompt_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    expected_case_count INTEGER NOT NULL CHECK (
        expected_case_count BETWEEN 1 AND 64
    ),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    UNIQUE(project_id, native_request_id),
    CHECK (tail_prompt_end_byte = exact_prompt_byte_len),
    CHECK (
        (source_tail_origin = 'live_manuscript'
            AND source_tail_revision_id IS NOT NULL
            AND source_tail_assembly_id IS NULL)
        OR
        (source_tail_origin = 'admitted_assembly'
            AND source_tail_revision_id IS NULL
            AND source_tail_assembly_id IS NOT NULL)
    ),
    FOREIGN KEY(model_binding_fingerprint, model_binding_source_hash)
        REFERENCES research_model_binding_sources(binding_fingerprint, manifest_source_hash)
        ON DELETE RESTRICT
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_verified_batches_validate_insert
BEFORE INSERT ON research_verified_inference_batches
WHEN NOT EXISTS (
        SELECT 1 FROM blobs prompt
        WHERE prompt.blob_id = NEW.exact_prompt_blob_id
          AND prompt.byte_len = NEW.exact_prompt_byte_len
    )
    OR NOT EXISTS (
        SELECT 1 FROM blobs tokens
        WHERE tokens.blob_id = NEW.prompt_token_ids_blob_id
          AND tokens.byte_len = NEW.prompt_token_count * 4
    )
    OR NOT EXISTS (
        SELECT 1 FROM blobs specification
        WHERE specification.blob_id = NEW.prompt_specification_blob_id
          AND specification.byte_len = NEW.prompt_specification_byte_len
    )
    OR NOT EXISTS (
        SELECT 1 FROM blobs source_tail
        WHERE source_tail.blob_id = NEW.source_tail_blob_id
          AND source_tail.byte_len = NEW.source_tail_end_byte
    )
    OR NEW.tail_prompt_end_byte - NEW.tail_prompt_start_byte
        != NEW.source_tail_end_byte - NEW.source_tail_start_byte
    OR (
        NEW.source_tail_origin = 'live_manuscript'
        AND NOT EXISTS (
            SELECT 1
            FROM revisions revision
            JOIN artifacts artifact USING (artifact_id)
            WHERE revision.revision_id = NEW.source_tail_revision_id
              AND artifact.blob_id = NEW.source_tail_blob_id
        )
    )
    OR (
        NEW.source_tail_origin = 'admitted_assembly'
        AND NOT EXISTS (
            SELECT 1
            FROM research_candidate_assemblies assembly
            JOIN research_admission_records admission
              ON admission.subject_kind = 'candidate_assembly'
             AND admission.subject_id = assembly.assembly_id
            WHERE assembly.assembly_id = NEW.source_tail_assembly_id
              AND assembly.assembled_blob_id = NEW.source_tail_blob_id
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'verified inference batch prompt evidence is absent or inconsistent');
END;

CREATE TABLE research_verified_prompt_sources (
    batch_verification_fingerprint TEXT NOT NULL REFERENCES research_verified_inference_batches(batch_verification_fingerprint) ON DELETE RESTRICT,
    position INTEGER NOT NULL CHECK (position BETWEEN 0 AND 63),
    block_index INTEGER NOT NULL CHECK (block_index BETWEEN -1 AND 62),
    source_index INTEGER NOT NULL CHECK (source_index BETWEEN 0 AND 15),
    source_kind TEXT NOT NULL CHECK (
        source_kind IN ('preceding_exact', 'tail_live', 'tail_admitted_assembly')
    ),
    source_revision_id TEXT CHECK (
        source_revision_id IS NULL OR length(source_revision_id) = 26
    ),
    source_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    source_start_byte INTEGER NOT NULL CHECK (source_start_byte >= 0),
    source_end_byte INTEGER NOT NULL CHECK (source_end_byte > source_start_byte),
    assembly_id TEXT CHECK (assembly_id IS NULL OR length(assembly_id) = 26),
    PRIMARY KEY(batch_verification_fingerprint, position),
    UNIQUE(batch_verification_fingerprint, block_index, source_index),
    CHECK (
        (source_kind IN ('preceding_exact', 'tail_live')
            AND source_revision_id IS NOT NULL
            AND assembly_id IS NULL)
        OR (source_kind = 'tail_admitted_assembly'
            AND source_revision_id IS NULL
            AND assembly_id IS NOT NULL)
    ),
    CHECK (
        (source_kind = 'preceding_exact' AND block_index >= 0 AND source_index = 0)
        OR (source_kind LIKE 'tail_%' AND block_index = -1 AND source_index = 0)
    )
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_verified_prompt_sources_validate_insert
BEFORE INSERT ON research_verified_prompt_sources
WHEN EXISTS (
        SELECT 1 FROM research_verified_inference_batch_seals seal
        WHERE seal.batch_verification_fingerprint = NEW.batch_verification_fingerprint
    )
    OR NOT EXISTS (
        SELECT 1 FROM research_verified_inference_batches batch
        WHERE batch.batch_verification_fingerprint = NEW.batch_verification_fingerprint
          AND NEW.position < batch.prompt_source_count
    )
    OR (
        NEW.source_kind LIKE 'tail_%'
        AND NOT EXISTS (
            SELECT 1 FROM research_verified_inference_batches batch
            WHERE batch.batch_verification_fingerprint = NEW.batch_verification_fingerprint
              AND batch.source_tail_blob_id = NEW.source_blob_id
              AND batch.source_tail_start_byte = NEW.source_start_byte
              AND batch.source_tail_end_byte = NEW.source_end_byte
              AND (
                  (NEW.source_kind = 'tail_live'
                    AND batch.source_tail_origin = 'live_manuscript'
                    AND batch.source_tail_revision_id = NEW.source_revision_id
                    AND NEW.assembly_id IS NULL)
                  OR
                  (NEW.source_kind = 'tail_admitted_assembly'
                    AND batch.source_tail_origin = 'admitted_assembly'
                    AND batch.source_tail_revision_id IS NULL
                    AND NEW.source_revision_id IS NULL
                    AND batch.source_tail_assembly_id = NEW.assembly_id)
              )
        )
    )
    OR NOT EXISTS (
        SELECT 1 FROM blobs source
        WHERE source.blob_id = NEW.source_blob_id
          AND source.byte_len >= NEW.source_end_byte
    )
    OR (
        NEW.source_kind LIKE 'tail_%'
        AND NOT EXISTS (
            SELECT 1 FROM blobs source
            WHERE source.blob_id = NEW.source_blob_id
              AND source.byte_len = NEW.source_end_byte
        )
    )
    OR (
        NEW.source_kind IN ('preceding_exact', 'tail_live')
        AND NOT EXISTS (
            SELECT 1
            FROM revisions revision
            JOIN artifacts artifact USING (artifact_id)
            WHERE revision.revision_id = NEW.source_revision_id
              AND artifact.blob_id = NEW.source_blob_id
        )
    )
    OR (
        NEW.source_kind = 'tail_admitted_assembly'
        AND NOT EXISTS (
            SELECT 1
            FROM research_candidate_assemblies assembly
            JOIN research_admission_records admission
              ON admission.subject_kind = 'candidate_assembly'
             AND admission.subject_id = assembly.assembly_id
            WHERE assembly.assembly_id = NEW.assembly_id
              AND assembly.assembled_blob_id = NEW.source_blob_id
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'prompt source range is not exact immutable project evidence');
END;

CREATE TABLE research_cancelled_call_diagnostics (
    call_id TEXT PRIMARY KEY REFERENCES research_call_terminals(call_id) ON DELETE RESTRICT,
    partial_raw_output_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    partial_raw_output_byte_len INTEGER NOT NULL CHECK (
        partial_raw_output_byte_len BETWEEN 0 AND 16777216
    ),
    token_ids_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    token_count INTEGER NOT NULL CHECK (token_count BETWEEN 0 AND 1048576),
    token_ids_fingerprint TEXT NOT NULL CHECK (
        length(token_ids_fingerprint) = 64
        AND token_ids_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    raw_event_stream_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    backend_receipt_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    verification_fingerprint TEXT NOT NULL CHECK (
        length(verification_fingerprint) = 64
        AND verification_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_cancelled_diagnostics_validate_insert
BEFORE INSERT ON research_cancelled_call_diagnostics
WHEN NOT EXISTS (
        SELECT 1
        FROM research_call_terminals terminal
        JOIN research_model_calls call USING (call_id)
        WHERE terminal.call_id = NEW.call_id
          AND terminal.status = 'cancelled'
          AND call.evidence_class = 'live_base_writer_claim'
    )
    OR NOT EXISTS (
        SELECT 1 FROM blobs output
        WHERE output.blob_id = NEW.partial_raw_output_blob_id
          AND output.byte_len = NEW.partial_raw_output_byte_len
    )
    OR NOT EXISTS (
        SELECT 1 FROM blobs tokens
        WHERE tokens.blob_id = NEW.token_ids_blob_id
          AND tokens.byte_len = NEW.token_count * 4
    )
    OR NOT EXISTS (
        SELECT 1 FROM blobs events, blobs receipt
        WHERE events.blob_id = NEW.raw_event_stream_blob_id
          AND events.byte_len > 0
          AND receipt.blob_id = NEW.backend_receipt_blob_id
          AND receipt.byte_len > 0
    )
BEGIN
    SELECT RAISE(ABORT, 'cancelled call diagnostic is not bound to verified terminal evidence');
END;

-- Every completed call gets exactly one row, including a call whose valid
-- projection is absent. Persisting explicit absence prevents replay from
-- silently treating the raw completion as displayed prose.
CREATE TABLE research_completed_call_evidence (
    call_id TEXT PRIMARY KEY REFERENCES research_call_terminals(call_id) ON DELETE RESTRICT,
    raw_output_byte_len INTEGER NOT NULL CHECK (
        raw_output_byte_len BETWEEN 0 AND 16777216
    ),
    has_output_projection INTEGER NOT NULL CHECK (has_output_projection IN (0, 1)),
    displayed_output_blob_id TEXT REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    displayed_output_byte_len INTEGER CHECK (
        displayed_output_byte_len IS NULL OR displayed_output_byte_len > 0
    ),
    displayed_start_byte INTEGER,
    displayed_end_byte INTEGER,
    endpoint_tail_start_byte INTEGER,
    endpoint_tail_end_byte INTEGER,
    stop_suffix_start_byte INTEGER,
    stop_suffix_end_byte INTEGER,
    terminal_sampled_token_id INTEGER CHECK (
        terminal_sampled_token_id IS NULL OR terminal_sampled_token_id >= 0
    ),
    verification_fingerprint TEXT NOT NULL CHECK (
        length(verification_fingerprint) = 64
        AND verification_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    CHECK (
        (has_output_projection = 0
            AND displayed_output_blob_id IS NULL
            AND displayed_output_byte_len IS NULL
            AND displayed_start_byte IS NULL
            AND displayed_end_byte IS NULL
            AND endpoint_tail_start_byte IS NULL
            AND endpoint_tail_end_byte IS NULL
            AND stop_suffix_start_byte IS NULL
            AND stop_suffix_end_byte IS NULL)
        OR
        (has_output_projection = 1
            AND displayed_output_blob_id IS NOT NULL
            AND displayed_output_byte_len IS NOT NULL
            AND displayed_start_byte = 0
            AND displayed_end_byte = displayed_output_byte_len
            AND displayed_end_byte > displayed_start_byte
            AND displayed_end_byte = endpoint_tail_start_byte
            AND endpoint_tail_start_byte <= endpoint_tail_end_byte
            AND endpoint_tail_end_byte = stop_suffix_start_byte
            AND stop_suffix_start_byte <= stop_suffix_end_byte
            AND stop_suffix_end_byte = raw_output_byte_len)
    )
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_completed_call_evidence_validate_insert
BEFORE INSERT ON research_completed_call_evidence
WHEN NOT EXISTS (
        SELECT 1
        FROM research_call_terminals terminal
        JOIN research_model_calls call USING (call_id)
        WHERE terminal.call_id = NEW.call_id
          AND terminal.status = 'completed'
          AND terminal.raw_output_byte_len = NEW.raw_output_byte_len
          AND call.evidence_class = 'live_base_writer_claim'
          AND call.verification_audit_fingerprint = NEW.verification_fingerprint
    )
    OR (
        NEW.has_output_projection = 1
        AND NOT EXISTS (
            SELECT 1 FROM blobs displayed
            WHERE displayed.blob_id = NEW.displayed_output_blob_id
              AND displayed.byte_len = NEW.displayed_output_byte_len
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'completed call projection is absent or inconsistent');
END;

CREATE TABLE research_verified_inference_batch_calls (
    batch_verification_fingerprint TEXT NOT NULL REFERENCES research_verified_inference_batches(batch_verification_fingerprint) ON DELETE RESTRICT,
    position INTEGER NOT NULL CHECK (position >= 0),
    call_id TEXT NOT NULL UNIQUE REFERENCES research_model_calls(call_id) ON DELETE RESTRICT,
    campaign_id TEXT NOT NULL CHECK (length(campaign_id) = 26),
    stage_id TEXT NOT NULL CHECK (length(stage_id) = 26),
    stage_attempt_id TEXT NOT NULL CHECK (length(stage_attempt_id) = 26),
    trial_case_id TEXT NOT NULL CHECK (length(trial_case_id) = 26),
    seed_decimal TEXT NOT NULL CHECK (
        length(seed_decimal) BETWEEN 1 AND 20
        AND seed_decimal NOT GLOB '*[^0-9]*'
        AND (seed_decimal = '0' OR substr(seed_decimal, 1, 1) != '0')
        AND (length(seed_decimal) < 20 OR seed_decimal <= '18446744073709551615')
    ),
    outcome TEXT NOT NULL CHECK (outcome IN ('completed', 'cancelled')),
    case_verification_fingerprint TEXT NOT NULL CHECK (
        length(case_verification_fingerprint) = 64
        AND case_verification_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    PRIMARY KEY(batch_verification_fingerprint, position),
    UNIQUE(batch_verification_fingerprint, call_id)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_verified_batch_calls_validate_insert
BEFORE INSERT ON research_verified_inference_batch_calls
WHEN EXISTS (
        SELECT 1 FROM research_verified_inference_batch_seals seal
        WHERE seal.batch_verification_fingerprint = NEW.batch_verification_fingerprint
    )
    OR NOT EXISTS (
        SELECT 1
        FROM research_verified_inference_batches batch
        JOIN research_model_bindings binding
          ON binding.binding_fingerprint = batch.model_binding_fingerprint
        JOIN research_model_calls call ON call.call_id = NEW.call_id
        WHERE batch.batch_verification_fingerprint = NEW.batch_verification_fingerprint
          AND NEW.position < batch.expected_case_count
          AND call.campaign_id = NEW.campaign_id
          AND call.campaign_id = batch.prompt_campaign_id
          AND call.stage_id = NEW.stage_id
          AND call.stage_id = batch.prompt_stage_id
          AND call.stage_attempt_id = NEW.stage_attempt_id
          AND call.stage_attempt_id = batch.prompt_stage_attempt_id
          AND call.trial_case_id = NEW.trial_case_id
          AND call.trial_case_id = batch.prompt_trial_case_id
          AND call.seed_decimal = NEW.seed_decimal
          AND call.model_fingerprint = batch.runtime_model_fingerprint
          AND call.tokenizer_fingerprint = binding.tokenizer_sha256
          AND call.prompt_fingerprint = batch.compiled_prompt_fingerprint
          AND call.evidence_class = 'live_base_writer_claim'
    )
    OR (
        NEW.outcome = 'completed'
        AND NOT EXISTS (
            SELECT 1
            FROM research_model_calls call
            JOIN research_call_terminals terminal USING (call_id)
            JOIN research_completed_call_evidence completed USING (call_id)
            WHERE call.call_id = NEW.call_id
              AND call.verification_audit_fingerprint = NEW.case_verification_fingerprint
              AND terminal.status = 'completed'
              AND completed.verification_fingerprint = NEW.case_verification_fingerprint
        )
    )
    OR (
        NEW.outcome = 'cancelled'
        AND NOT EXISTS (
            SELECT 1
            FROM research_model_calls call
            JOIN research_call_terminals terminal USING (call_id)
            JOIN research_cancelled_call_diagnostics diagnostic USING (call_id)
            WHERE call.call_id = NEW.call_id
              AND call.verification_audit_fingerprint IS NULL
              AND terminal.status = 'cancelled'
              AND diagnostic.verification_fingerprint = NEW.case_verification_fingerprint
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'verified inference batch case is absent, mismatched, or already sealed');
END;

CREATE TABLE research_verified_inference_batch_seals (
    batch_verification_fingerprint TEXT PRIMARY KEY REFERENCES research_verified_inference_batches(batch_verification_fingerprint) ON DELETE RESTRICT,
    completed_call_count INTEGER NOT NULL CHECK (completed_call_count >= 0),
    cancelled_call_count INTEGER NOT NULL CHECK (cancelled_call_count >= 0),
    sealed_at_ms INTEGER NOT NULL CHECK (sealed_at_ms > 0),
    CHECK (completed_call_count + cancelled_call_count >= 1)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_verified_batch_seals_validate_insert
BEFORE INSERT ON research_verified_inference_batch_seals
WHEN NOT EXISTS (
        SELECT 1
        FROM research_verified_inference_batches batch
        WHERE batch.batch_verification_fingerprint = NEW.batch_verification_fingerprint
          AND batch.expected_case_count = NEW.completed_call_count + NEW.cancelled_call_count
    )
    OR (
        SELECT COUNT(*) FROM research_verified_inference_batch_calls item
        WHERE item.batch_verification_fingerprint = NEW.batch_verification_fingerprint
    ) != NEW.completed_call_count + NEW.cancelled_call_count
    OR (
        SELECT COUNT(*) FROM research_verified_prompt_sources source
        WHERE source.batch_verification_fingerprint = NEW.batch_verification_fingerprint
    ) != (
        SELECT prompt_source_count FROM research_verified_inference_batches batch
        WHERE batch.batch_verification_fingerprint = NEW.batch_verification_fingerprint
    )
    OR (
        SELECT COUNT(*) FROM research_verified_prompt_sources source
        WHERE source.batch_verification_fingerprint = NEW.batch_verification_fingerprint
          AND source.source_kind LIKE 'tail_%'
    ) != 1
    OR (
        SELECT COUNT(*) FROM research_verified_inference_batch_calls item
        WHERE item.batch_verification_fingerprint = NEW.batch_verification_fingerprint
          AND item.outcome = 'completed'
    ) != NEW.completed_call_count
    OR (
        SELECT COUNT(*) FROM research_verified_inference_batch_calls item
        WHERE item.batch_verification_fingerprint = NEW.batch_verification_fingerprint
          AND item.outcome = 'cancelled'
    ) != NEW.cancelled_call_count
    OR (
        SELECT COUNT(*)
        FROM research_verified_inference_batch_calls item
        JOIN research_completed_call_evidence completed USING (call_id)
        WHERE item.batch_verification_fingerprint = NEW.batch_verification_fingerprint
          AND item.outcome = 'completed'
    ) != NEW.completed_call_count
    OR (
        SELECT COUNT(*)
        FROM research_verified_inference_batch_calls item
        JOIN research_cancelled_call_diagnostics diagnostic USING (call_id)
        WHERE item.batch_verification_fingerprint = NEW.batch_verification_fingerprint
          AND item.outcome = 'cancelled'
    ) != NEW.cancelled_call_count
    OR (
        SELECT COALESCE(MIN(position), -1) FROM research_verified_inference_batch_calls item
        WHERE item.batch_verification_fingerprint = NEW.batch_verification_fingerprint
    ) != 0
    OR (
        SELECT COALESCE(MAX(position), -1) FROM research_verified_inference_batch_calls item
        WHERE item.batch_verification_fingerprint = NEW.batch_verification_fingerprint
    ) != NEW.completed_call_count + NEW.cancelled_call_count - 1
BEGIN
    SELECT RAISE(ABORT, 'verified inference batch seal does not cover every ordered case');
END;

-- Migration-eight hardening for historical migration-seven tables. Existing
-- rows remain readable; new evidence must use canonical seed and digest text.
CREATE TRIGGER research_v8_model_calls_harden_insert
BEFORE INSERT ON research_model_calls
WHEN (NEW.seed_decimal != '0' AND substr(NEW.seed_decimal, 1, 1) = '0')
    OR (length(NEW.seed_decimal) = 20 AND NEW.seed_decimal > '18446744073709551615')
    OR NEW.model_fingerprint GLOB '*[^0-9a-f]*'
    OR NEW.tokenizer_fingerprint GLOB '*[^0-9a-f]*'
    OR NEW.prompt_fingerprint GLOB '*[^0-9a-f]*'
    OR NEW.sampler_fingerprint GLOB '*[^0-9a-f]*'
    OR NEW.control_program_fingerprint GLOB '*[^0-9a-f]*'
    OR (
        NEW.verification_audit_fingerprint IS NOT NULL
        AND NEW.verification_audit_fingerprint GLOB '*[^0-9a-f]*'
    )
BEGIN
    SELECT RAISE(ABORT, 'research model call has non-canonical seed or fingerprint evidence');
END;

CREATE TRIGGER research_v8_completed_terminal_evidence_insert
BEFORE INSERT ON research_call_terminals
WHEN NEW.status = 'completed'
    AND (
        NOT EXISTS (
            SELECT 1 FROM blobs events
            WHERE events.blob_id = NEW.raw_event_stream_blob_id
              AND events.byte_len > 0
        )
        OR (
            NEW.backend_receipt_blob_id IS NOT NULL
            AND NOT EXISTS (
                SELECT 1 FROM blobs receipt
                WHERE receipt.blob_id = NEW.backend_receipt_blob_id
                  AND receipt.byte_len > 0
            )
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'completed call event or receipt evidence is empty');
END;

CREATE TRIGGER research_model_bindings_immutable_update BEFORE UPDATE ON research_model_bindings BEGIN SELECT RAISE(ABORT, 'compiled model bindings are immutable'); END;
CREATE TRIGGER research_model_bindings_immutable_delete BEFORE DELETE ON research_model_bindings BEGIN SELECT RAISE(ABORT, 'compiled model bindings are immutable'); END;
CREATE TRIGGER research_model_binding_sources_immutable_update BEFORE UPDATE ON research_model_binding_sources BEGIN SELECT RAISE(ABORT, 'model binding source occurrences are immutable'); END;
CREATE TRIGGER research_model_binding_sources_immutable_delete BEFORE DELETE ON research_model_binding_sources BEGIN SELECT RAISE(ABORT, 'model binding source occurrences are immutable'); END;
CREATE TRIGGER research_verified_batches_immutable_update BEFORE UPDATE ON research_verified_inference_batches BEGIN SELECT RAISE(ABORT, 'verified inference batches are immutable'); END;
CREATE TRIGGER research_verified_batches_immutable_delete BEFORE DELETE ON research_verified_inference_batches BEGIN SELECT RAISE(ABORT, 'verified inference batches are immutable'); END;
CREATE TRIGGER research_verified_prompt_sources_immutable_update BEFORE UPDATE ON research_verified_prompt_sources BEGIN SELECT RAISE(ABORT, 'verified prompt sources are immutable'); END;
CREATE TRIGGER research_verified_prompt_sources_immutable_delete BEFORE DELETE ON research_verified_prompt_sources BEGIN SELECT RAISE(ABORT, 'verified prompt sources are immutable'); END;
CREATE TRIGGER research_cancelled_diagnostics_immutable_update BEFORE UPDATE ON research_cancelled_call_diagnostics BEGIN SELECT RAISE(ABORT, 'cancelled call diagnostics are immutable'); END;
CREATE TRIGGER research_cancelled_diagnostics_immutable_delete BEFORE DELETE ON research_cancelled_call_diagnostics BEGIN SELECT RAISE(ABORT, 'cancelled call diagnostics are immutable'); END;
CREATE TRIGGER research_completed_call_evidence_immutable_update BEFORE UPDATE ON research_completed_call_evidence BEGIN SELECT RAISE(ABORT, 'completed call evidence is immutable'); END;
CREATE TRIGGER research_completed_call_evidence_immutable_delete BEFORE DELETE ON research_completed_call_evidence BEGIN SELECT RAISE(ABORT, 'completed call evidence is immutable'); END;
CREATE TRIGGER research_verified_batch_calls_immutable_update BEFORE UPDATE ON research_verified_inference_batch_calls BEGIN SELECT RAISE(ABORT, 'verified inference batch cases are immutable'); END;
CREATE TRIGGER research_verified_batch_calls_immutable_delete BEFORE DELETE ON research_verified_inference_batch_calls BEGIN SELECT RAISE(ABORT, 'verified inference batch cases are immutable'); END;
CREATE TRIGGER research_verified_batch_seals_immutable_update BEFORE UPDATE ON research_verified_inference_batch_seals BEGIN SELECT RAISE(ABORT, 'verified inference batch seals are immutable'); END;
CREATE TRIGGER research_verified_batch_seals_immutable_delete BEFORE DELETE ON research_verified_inference_batch_seals BEGIN SELECT RAISE(ABORT, 'verified inference batch seals are immutable'); END;
