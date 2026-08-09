-- Research evidence is append-only and deliberately separate from the legacy
-- one-run/one-candidate shelf.  A row in a component table is only diagnostic
-- evidence until a matching row exists in research_admissions.

CREATE TABLE research_model_calls (
    call_id TEXT PRIMARY KEY,
    campaign_id TEXT NOT NULL,
    stage_id TEXT NOT NULL,
    stage_attempt_id TEXT NOT NULL,
    trial_case_id TEXT NOT NULL,
    seed_decimal TEXT NOT NULL CHECK (
        length(seed_decimal) BETWEEN 1 AND 20
        AND seed_decimal NOT GLOB '*[^0-9]*'
    ),
    model_fingerprint TEXT NOT NULL CHECK (length(model_fingerprint) = 64),
    tokenizer_fingerprint TEXT NOT NULL CHECK (length(tokenizer_fingerprint) = 64),
    prompt_fingerprint TEXT NOT NULL CHECK (length(prompt_fingerprint) = 64),
    sampler_fingerprint TEXT NOT NULL CHECK (length(sampler_fingerprint) = 64),
    control_program_fingerprint TEXT NOT NULL CHECK (length(control_program_fingerprint) = 64),
    evidence_class TEXT NOT NULL CHECK (evidence_class IN (
        'live_base_writer_claim',
        'live_instruct_editor_claim',
        'live_local_critic_claim',
        'live_codex_critic_claim',
        'fixture',
        'mock',
        'historical_receipt'
    )),
    verification_replay_fingerprint TEXT CHECK (
        verification_replay_fingerprint IS NULL
        OR length(verification_replay_fingerprint) = 64
    ),
    call_record_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    CHECK (
        verification_replay_fingerprint IS NULL
        OR evidence_class IN (
            'live_base_writer_claim',
            'live_instruct_editor_claim',
            'live_local_critic_claim',
            'live_codex_critic_claim'
        )
    )
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_model_calls_validate_insert
BEFORE INSERT ON research_model_calls
WHEN NOT EXISTS (
    SELECT 1 FROM blobs record
    WHERE record.blob_id = NEW.call_record_blob_id
      AND record.byte_len > 0
)
BEGIN
    SELECT RAISE(ABORT, 'research model call record blob is absent or empty');
END;

CREATE TABLE research_call_terminals (
    call_id TEXT PRIMARY KEY REFERENCES research_model_calls(call_id) ON DELETE RESTRICT,
    status TEXT NOT NULL CHECK (status IN ('completed', 'failed', 'cancelled', 'rejected')),
    raw_output_blob_id TEXT REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    raw_output_byte_len INTEGER CHECK (raw_output_byte_len IS NULL OR raw_output_byte_len >= 0),
    token_ids_blob_id TEXT REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    token_count INTEGER CHECK (token_count IS NULL OR token_count BETWEEN 0 AND 1048576),
    token_ids_fingerprint TEXT CHECK (
        token_ids_fingerprint IS NULL OR length(token_ids_fingerprint) = 64
    ),
    raw_event_stream_blob_id TEXT REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    backend_receipt_blob_id TEXT REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    terminal_message TEXT CHECK (
        terminal_message IS NULL OR length(CAST(terminal_message AS BLOB)) BETWEEN 1 AND 1024
    ),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    CHECK (
        (status = 'completed'
            AND raw_output_blob_id IS NOT NULL
            AND raw_output_byte_len IS NOT NULL
            AND token_ids_blob_id IS NOT NULL
            AND token_count IS NOT NULL
            AND token_ids_fingerprint IS NOT NULL
            AND raw_event_stream_blob_id IS NOT NULL
            AND terminal_message IS NULL)
        OR
        (status != 'completed'
            AND raw_output_blob_id IS NULL
            AND raw_output_byte_len IS NULL
            AND token_ids_blob_id IS NULL
            AND token_count IS NULL
            AND token_ids_fingerprint IS NULL
            AND raw_event_stream_blob_id IS NULL
            AND backend_receipt_blob_id IS NULL
            AND terminal_message IS NOT NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_call_terminals_validate_insert
BEFORE INSERT ON research_call_terminals
WHEN NOT EXISTS (
        SELECT 1 FROM research_model_calls call
        WHERE call.call_id = NEW.call_id
    )
    OR (
        NEW.status = 'completed'
        AND EXISTS (
            SELECT 1 FROM research_model_calls call
            WHERE call.call_id = NEW.call_id
              AND call.evidence_class LIKE 'live_%_claim'
              AND NEW.backend_receipt_blob_id IS NULL
        )
    )
    OR (
        NEW.status != 'completed'
        AND EXISTS (
            SELECT 1 FROM research_model_calls call
            WHERE call.call_id = NEW.call_id
              AND call.verification_replay_fingerprint IS NOT NULL
        )
    )
    OR (
        NEW.status = 'completed'
        AND NOT EXISTS (
            SELECT 1
            FROM blobs output, blobs tokens
            WHERE output.blob_id = NEW.raw_output_blob_id
              AND output.byte_len = NEW.raw_output_byte_len
              AND tokens.blob_id = NEW.token_ids_blob_id
              AND tokens.byte_len = NEW.token_count * 4
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'research call terminal does not match its call evidence');
END;

CREATE TABLE research_output_projections (
    occurrence_id TEXT PRIMARY KEY,
    call_id TEXT NOT NULL REFERENCES research_model_calls(call_id) ON DELETE RESTRICT,
    raw_output_byte_len INTEGER NOT NULL CHECK (raw_output_byte_len > 0),
    displayed_start_byte INTEGER NOT NULL CHECK (displayed_start_byte = 0),
    displayed_end_byte INTEGER NOT NULL CHECK (displayed_end_byte > displayed_start_byte),
    endpoint_tail_start_byte INTEGER NOT NULL,
    endpoint_tail_end_byte INTEGER NOT NULL,
    stop_suffix_start_byte INTEGER NOT NULL,
    stop_suffix_end_byte INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    CHECK (displayed_end_byte = endpoint_tail_start_byte),
    CHECK (endpoint_tail_start_byte <= endpoint_tail_end_byte),
    CHECK (endpoint_tail_end_byte = stop_suffix_start_byte),
    CHECK (stop_suffix_start_byte <= stop_suffix_end_byte),
    CHECK (stop_suffix_end_byte = raw_output_byte_len)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_output_projections_validate_insert
BEFORE INSERT ON research_output_projections
WHEN NOT EXISTS (
    SELECT 1
    FROM research_call_terminals terminal
    WHERE terminal.call_id = NEW.call_id
      AND terminal.status = 'completed'
      AND terminal.raw_output_byte_len = NEW.raw_output_byte_len
)
BEGIN
    SELECT RAISE(ABORT, 'output projection is not bound to a completed call');
END;

CREATE TABLE research_generated_span_occurrences (
    occurrence_id TEXT PRIMARY KEY REFERENCES research_output_projections(occurrence_id) ON DELETE RESTRICT,
    call_id TEXT NOT NULL REFERENCES research_model_calls(call_id) ON DELETE RESTRICT,
    raw_output_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    output_start_byte INTEGER NOT NULL CHECK (output_start_byte = 0),
    output_end_byte INTEGER NOT NULL CHECK (output_end_byte > output_start_byte),
    token_start INTEGER CHECK (token_start IS NULL OR token_start >= 0),
    token_end INTEGER CHECK (token_end IS NULL OR token_end > token_start),
    evidence_class TEXT NOT NULL CHECK (evidence_class IN (
        'live_base_writer_claim',
        'live_instruct_editor_claim',
        'live_local_critic_claim',
        'live_codex_critic_claim',
        'fixture',
        'mock',
        'historical_receipt'
    )),
    extraction_receipt_fingerprint TEXT NOT NULL CHECK (length(extraction_receipt_fingerprint) = 64),
    verification_replay_fingerprint TEXT CHECK (
        verification_replay_fingerprint IS NULL OR length(verification_replay_fingerprint) = 64
    ),
    span_record_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    CHECK ((token_start IS NULL) = (token_end IS NULL))
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_generated_spans_validate_insert
BEFORE INSERT ON research_generated_span_occurrences
WHEN NOT EXISTS (
    SELECT 1
    FROM research_output_projections projection
    JOIN research_call_terminals terminal ON terminal.call_id = projection.call_id
    JOIN research_model_calls call ON call.call_id = projection.call_id
    WHERE projection.occurrence_id = NEW.occurrence_id
      AND projection.call_id = NEW.call_id
      AND projection.displayed_start_byte = NEW.output_start_byte
      AND projection.displayed_end_byte = NEW.output_end_byte
      AND terminal.status = 'completed'
      AND terminal.raw_output_blob_id = NEW.raw_output_blob_id
      AND call.evidence_class = NEW.evidence_class
      AND (
          (NEW.verification_replay_fingerprint IS NULL)
          OR (
              call.verification_replay_fingerprint = NEW.verification_replay_fingerprint
              AND NEW.evidence_class = 'live_base_writer_claim'
          )
      )
      AND (NEW.token_end IS NULL OR NEW.token_end <= terminal.token_count)
      AND EXISTS (
          SELECT 1 FROM blobs record
          WHERE record.blob_id = NEW.span_record_blob_id
            AND record.byte_len > 0
      )
)
BEGIN
    SELECT RAISE(ABORT, 'generated span is not exact call evidence');
END;

CREATE TABLE research_operation_graphs (
    graph_fingerprint TEXT PRIMARY KEY CHECK (length(graph_fingerprint) = 64),
    graph_record_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    output_operation_id TEXT NOT NULL,
    node_count INTEGER NOT NULL CHECK (node_count BETWEEN 1 AND 4096),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_operation_graphs_validate_insert
BEFORE INSERT ON research_operation_graphs
WHEN NOT EXISTS (
    SELECT 1 FROM blobs record
    WHERE record.blob_id = NEW.graph_record_blob_id
      AND record.byte_len > 0
)
BEGIN
    SELECT RAISE(ABORT, 'operation graph record blob is absent or empty');
END;

CREATE TABLE research_pipeline_operations (
    graph_fingerprint TEXT NOT NULL REFERENCES research_operation_graphs(graph_fingerprint) ON DELETE RESTRICT,
    position INTEGER NOT NULL CHECK (position >= 0),
    operation_id TEXT NOT NULL,
    operation_kind TEXT NOT NULL CHECK (operation_kind IN (
        'model_call', 'extract_span', 'assemble', 'project',
        'human_transformation', 'instruct_editor_transformation',
        'critic_text', 'codex_text', 'fixture_text',
        'historical_text', 'literal_text'
    )),
    reference_id TEXT NOT NULL,
    producer_call_id TEXT,
    evidence_class TEXT CHECK (evidence_class IS NULL OR evidence_class IN (
        'live_base_writer_claim', 'live_instruct_editor_claim',
        'live_local_critic_claim', 'live_codex_critic_claim',
        'fixture', 'mock', 'historical_receipt'
    )),
    PRIMARY KEY (graph_fingerprint, position),
    UNIQUE (graph_fingerprint, operation_id),
    CHECK (
        (operation_kind = 'model_call' AND evidence_class IS NOT NULL AND producer_call_id IS NULL)
        OR (operation_kind IN ('instruct_editor_transformation', 'critic_text')
            AND evidence_class IS NULL AND producer_call_id IS NOT NULL)
        OR (operation_kind NOT IN ('model_call', 'instruct_editor_transformation', 'critic_text')
            AND evidence_class IS NULL AND producer_call_id IS NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE TABLE research_pipeline_operation_inputs (
    graph_fingerprint TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    position INTEGER NOT NULL CHECK (position BETWEEN 0 AND 511),
    input_operation_id TEXT NOT NULL,
    PRIMARY KEY (graph_fingerprint, operation_id, position),
    FOREIGN KEY (graph_fingerprint, operation_id)
        REFERENCES research_pipeline_operations(graph_fingerprint, operation_id) ON DELETE RESTRICT,
    FOREIGN KEY (graph_fingerprint, input_operation_id)
        REFERENCES research_pipeline_operations(graph_fingerprint, operation_id) ON DELETE RESTRICT
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_pipeline_operations_validate_insert
BEFORE INSERT ON research_pipeline_operations
WHEN NEW.position >= (
        SELECT node_count FROM research_operation_graphs
        WHERE graph_fingerprint = NEW.graph_fingerprint
    )
    OR (
        NEW.operation_kind = 'model_call'
        AND NEW.evidence_class NOT IN (
            'live_base_writer_claim', 'live_instruct_editor_claim',
            'live_local_critic_claim', 'live_codex_critic_claim',
            'fixture', 'mock', 'historical_receipt'
        )
    )
    OR (
        NEW.operation_kind = 'model_call'
        AND NOT EXISTS (
            SELECT 1 FROM research_model_calls call
            WHERE call.call_id = NEW.reference_id
              AND call.evidence_class = NEW.evidence_class
        )
    )
    OR (
        NEW.operation_kind = 'extract_span'
        AND NOT EXISTS (
            SELECT 1 FROM research_generated_span_occurrences span
            WHERE span.occurrence_id = NEW.reference_id
        )
    )
    OR (
        NEW.operation_kind IN ('instruct_editor_transformation', 'critic_text')
        AND NOT EXISTS (
            SELECT 1 FROM research_model_calls call
            WHERE call.call_id = NEW.producer_call_id
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'pipeline operation violates its graph contract');
END;

CREATE TRIGGER research_pipeline_inputs_validate_insert
BEFORE INSERT ON research_pipeline_operation_inputs
WHEN NOT EXISTS (
    SELECT 1
    FROM research_pipeline_operations output
    JOIN research_pipeline_operations input
      ON input.graph_fingerprint = output.graph_fingerprint
     AND input.operation_id = NEW.input_operation_id
    WHERE output.graph_fingerprint = NEW.graph_fingerprint
      AND output.operation_id = NEW.operation_id
      AND input.position < output.position
)
    OR EXISTS (
        SELECT 1
        FROM research_pipeline_operations output
        JOIN research_pipeline_operations input
          ON input.graph_fingerprint = output.graph_fingerprint
         AND input.operation_id = NEW.input_operation_id
        WHERE output.graph_fingerprint = NEW.graph_fingerprint
          AND output.operation_id = NEW.operation_id
          AND (
              (output.operation_kind = 'extract_span' AND input.operation_kind != 'model_call')
              OR (output.operation_kind = 'assemble' AND input.operation_kind != 'extract_span')
              OR (output.operation_kind = 'project' AND input.operation_kind != 'assemble')
              OR output.operation_kind IN (
                  'model_call', 'critic_text', 'codex_text', 'fixture_text',
                  'historical_text', 'literal_text'
              )
          )
    )
BEGIN
    SELECT RAISE(ABORT, 'pipeline input must name an earlier node in the same graph');
END;

CREATE TABLE research_candidate_assemblies (
    assembly_id TEXT PRIMARY KEY,
    graph_fingerprint TEXT NOT NULL UNIQUE REFERENCES research_operation_graphs(graph_fingerprint) ON DELETE RESTRICT,
    part_count INTEGER NOT NULL CHECK (part_count BETWEEN 1 AND 256),
    part_order_fingerprint TEXT NOT NULL CHECK (length(part_order_fingerprint) = 64),
    assembled_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    assembled_byte_len INTEGER NOT NULL CHECK (assembled_byte_len > 0 AND assembled_byte_len <= 67108864),
    assembly_record_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_candidate_assemblies_validate_insert
BEFORE INSERT ON research_candidate_assemblies
WHEN NOT EXISTS (
    SELECT 1
    FROM blobs assembled, blobs record, research_operation_graphs graph
    WHERE assembled.blob_id = NEW.assembled_blob_id
      AND assembled.byte_len = NEW.assembled_byte_len
      AND record.blob_id = NEW.assembly_record_blob_id
      AND record.byte_len > 0
      AND graph.graph_fingerprint = NEW.graph_fingerprint
)
BEGIN
    SELECT RAISE(ABORT, 'candidate assembly bytes or graph are not registered');
END;

CREATE TABLE research_candidate_assembly_parts (
    assembly_id TEXT NOT NULL REFERENCES research_candidate_assemblies(assembly_id) ON DELETE RESTRICT,
    position INTEGER NOT NULL CHECK (position >= 0),
    join_before TEXT NOT NULL CHECK (join_before IN ('none', 'space', 'line_break', 'paragraph_break')),
    occurrence_id TEXT NOT NULL REFERENCES research_generated_span_occurrences(occurrence_id) ON DELETE RESTRICT,
    PRIMARY KEY (assembly_id, position),
    UNIQUE (assembly_id, occurrence_id)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_assembly_parts_validate_insert
BEFORE INSERT ON research_candidate_assembly_parts
WHEN NEW.position >= (
        SELECT part_count FROM research_candidate_assemblies
        WHERE assembly_id = NEW.assembly_id
    )
    OR (NEW.position = 0 AND NEW.join_before != 'none')
BEGIN
    SELECT RAISE(ABORT, 'assembly part violates its bounded order contract');
END;

CREATE TABLE research_candidate_projections (
    projection_id TEXT PRIMARY KEY,
    assembly_id TEXT NOT NULL REFERENCES research_candidate_assemblies(assembly_id) ON DELETE RESTRICT,
    source_revision_id TEXT NOT NULL REFERENCES revisions(revision_id) ON DELETE RESTRICT,
    source_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    target_start_byte INTEGER NOT NULL CHECK (target_start_byte >= 0),
    target_end_byte INTEGER NOT NULL CHECK (target_end_byte >= target_start_byte),
    graph_fingerprint TEXT NOT NULL UNIQUE REFERENCES research_operation_graphs(graph_fingerprint) ON DELETE RESTRICT,
    assembly_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    resulting_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    resulting_byte_len INTEGER NOT NULL CHECK (resulting_byte_len > 0),
    projection_record_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_candidate_projections_validate_insert
BEFORE INSERT ON research_candidate_projections
WHEN NOT EXISTS (
        SELECT 1
        FROM research_candidate_assemblies assembly
        JOIN research_operation_graphs graph
          ON graph.graph_fingerprint = NEW.graph_fingerprint
        JOIN blobs projection_record ON projection_record.blob_id = NEW.projection_record_blob_id
        JOIN revisions revision ON revision.revision_id = NEW.source_revision_id
        JOIN artifacts artifact ON artifact.artifact_id = revision.artifact_id
        JOIN blobs source_blob ON source_blob.blob_id = NEW.source_blob_id
        JOIN blobs resulting_blob ON resulting_blob.blob_id = NEW.resulting_blob_id
        WHERE assembly.assembly_id = NEW.assembly_id
          AND assembly.assembled_blob_id = NEW.assembly_blob_id
          AND artifact.blob_id = NEW.source_blob_id
          AND NEW.target_end_byte <= source_blob.byte_len
          AND projection_record.byte_len > 0
          AND resulting_blob.byte_len = NEW.resulting_byte_len
          AND NEW.resulting_byte_len = source_blob.byte_len
              - (NEW.target_end_byte - NEW.target_start_byte)
              + assembly.assembled_byte_len
          AND NOT EXISTS (
              SELECT 1 FROM research_pipeline_operations operation
              WHERE operation.graph_fingerprint = graph.graph_fingerprint
                AND (
                    operation.operation_kind NOT IN ('model_call', 'extract_span', 'assemble', 'project')
                    OR (operation.operation_kind = 'model_call'
                        AND operation.evidence_class != 'live_base_writer_claim')
                )
          )
    )
BEGIN
    SELECT RAISE(ABORT, 'candidate projection is not pinned to its assembly and source revision');
END;

CREATE TABLE research_mixed_authorship_assemblies (
    mixed_assembly_id TEXT PRIMARY KEY,
    output_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    output_byte_len INTEGER NOT NULL CHECK (output_byte_len > 0 AND output_byte_len <= 67108864),
    graph_fingerprint TEXT NOT NULL UNIQUE REFERENCES research_operation_graphs(graph_fingerprint) ON DELETE RESTRICT,
    mixed_record_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_mixed_assemblies_validate_insert
BEFORE INSERT ON research_mixed_authorship_assemblies
WHEN NOT EXISTS (
    SELECT 1
    FROM research_operation_graphs graph
    JOIN blobs output ON output.blob_id = NEW.output_blob_id
    JOIN blobs record ON record.blob_id = NEW.mixed_record_blob_id
    WHERE graph.graph_fingerprint = NEW.graph_fingerprint
      AND output.byte_len = NEW.output_byte_len
      AND record.byte_len > 0
      AND EXISTS (
          SELECT 1 FROM research_pipeline_operations operation
          WHERE operation.graph_fingerprint = graph.graph_fingerprint
            AND (
                operation.operation_kind NOT IN ('model_call', 'extract_span', 'assemble', 'project')
                OR (operation.operation_kind = 'model_call'
                    AND operation.evidence_class != 'live_base_writer_claim')
            )
      )
)
BEGIN
    SELECT RAISE(ABORT, 'mixed-authorship assembly must retain ineligible operations');
END;

CREATE TABLE research_admissions (
    admission_id TEXT PRIMARY KEY,
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('candidate_assembly', 'candidate_projection', 'mixed_authorship')),
    subject_id TEXT NOT NULL,
    admitted_at_ms INTEGER NOT NULL CHECK (admitted_at_ms > 0),
    UNIQUE (subject_kind, subject_id)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_admissions_validate_insert
BEFORE INSERT ON research_admissions
WHEN (
        NEW.subject_kind = 'candidate_assembly'
        AND NOT EXISTS (
            SELECT 1
            FROM research_candidate_assemblies assembly
            JOIN research_operation_graphs graph
              ON graph.graph_fingerprint = assembly.graph_fingerprint
            WHERE assembly.assembly_id = NEW.subject_id
              AND (SELECT COUNT(*) FROM research_candidate_assembly_parts part
                   WHERE part.assembly_id = assembly.assembly_id) = assembly.part_count
              AND (SELECT COUNT(*)
                   FROM research_candidate_assembly_parts part
                   JOIN research_generated_span_occurrences span
                     ON span.occurrence_id = part.occurrence_id
                   WHERE part.assembly_id = assembly.assembly_id
                     AND span.evidence_class = 'live_base_writer_claim'
                     AND span.verification_replay_fingerprint IS NOT NULL) = assembly.part_count
              AND (SELECT COUNT(*) FROM research_pipeline_operations operation
                   WHERE operation.graph_fingerprint = graph.graph_fingerprint) = graph.node_count
              AND graph.node_count = assembly.part_count * 2 + 1
              AND NOT EXISTS (
                  SELECT 1 FROM research_pipeline_operations operation
                  WHERE operation.graph_fingerprint = graph.graph_fingerprint
                    AND (
                        operation.operation_kind NOT IN ('model_call', 'extract_span', 'assemble')
                        OR (operation.operation_kind = 'model_call'
                            AND operation.evidence_class != 'live_base_writer_claim')
                    )
              )
              AND EXISTS (
                  SELECT 1 FROM research_pipeline_operations operation
                  WHERE operation.graph_fingerprint = graph.graph_fingerprint
                    AND operation.operation_id = graph.output_operation_id
                    AND operation.operation_kind = 'assemble'
                    AND operation.reference_id = assembly.assembly_id
              )
              AND (SELECT COUNT(*)
                   FROM research_pipeline_operations operation
                   WHERE operation.graph_fingerprint = graph.graph_fingerprint
                     AND operation.operation_kind = 'model_call') = assembly.part_count
              AND (SELECT COUNT(*)
                   FROM research_pipeline_operations operation
                   WHERE operation.graph_fingerprint = graph.graph_fingerprint
                     AND operation.operation_kind = 'extract_span') = assembly.part_count
              AND (SELECT COUNT(*)
                   FROM research_pipeline_operation_inputs input
                   WHERE input.graph_fingerprint = graph.graph_fingerprint) = assembly.part_count * 2
              AND NOT EXISTS (
                  SELECT 1
                  FROM research_candidate_assembly_parts part
                  JOIN research_generated_span_occurrences span
                    ON span.occurrence_id = part.occurrence_id
                  WHERE part.assembly_id = assembly.assembly_id
                    AND NOT EXISTS (
                        SELECT 1
                        FROM research_pipeline_operations call_operation
                        JOIN research_pipeline_operations extract_operation
                          ON extract_operation.graph_fingerprint = call_operation.graph_fingerprint
                        JOIN research_pipeline_operation_inputs extract_input
                          ON extract_input.graph_fingerprint = extract_operation.graph_fingerprint
                         AND extract_input.operation_id = extract_operation.operation_id
                         AND extract_input.position = 0
                         AND extract_input.input_operation_id = call_operation.operation_id
                        JOIN research_pipeline_operation_inputs assembly_input
                          ON assembly_input.graph_fingerprint = extract_operation.graph_fingerprint
                         AND assembly_input.operation_id = graph.output_operation_id
                         AND assembly_input.position = part.position
                         AND assembly_input.input_operation_id = extract_operation.operation_id
                        WHERE call_operation.graph_fingerprint = graph.graph_fingerprint
                          AND call_operation.operation_kind = 'model_call'
                          AND call_operation.reference_id = span.call_id
                          AND call_operation.evidence_class = 'live_base_writer_claim'
                          AND extract_operation.operation_kind = 'extract_span'
                          AND extract_operation.reference_id = span.occurrence_id
                    )
              )
        )
    )
    OR (
        NEW.subject_kind = 'candidate_projection'
        AND NOT EXISTS (
            SELECT 1
            FROM research_candidate_projections projection
            JOIN research_operation_graphs graph
              ON graph.graph_fingerprint = projection.graph_fingerprint
            JOIN research_candidate_assemblies assembly
              ON assembly.assembly_id = projection.assembly_id
            JOIN research_operation_graphs assembly_graph
              ON assembly_graph.graph_fingerprint = assembly.graph_fingerprint
            JOIN research_admissions assembly_admission
             ON assembly_admission.subject_kind = 'candidate_assembly'
             AND assembly_admission.subject_id = projection.assembly_id
            WHERE projection.projection_id = NEW.subject_id
              AND (SELECT COUNT(*) FROM research_pipeline_operations operation
                   WHERE operation.graph_fingerprint = graph.graph_fingerprint) = graph.node_count
              AND graph.node_count = assembly_graph.node_count + 1
              AND NOT EXISTS (
                  SELECT 1 FROM research_pipeline_operations operation
                  WHERE operation.graph_fingerprint = graph.graph_fingerprint
                    AND (
                        operation.operation_kind NOT IN ('model_call', 'extract_span', 'assemble', 'project')
                        OR (operation.operation_kind = 'model_call'
                            AND operation.evidence_class != 'live_base_writer_claim')
                    )
              )
              AND EXISTS (
                  SELECT 1 FROM research_pipeline_operations operation
                  WHERE operation.graph_fingerprint = graph.graph_fingerprint
                    AND operation.operation_id = graph.output_operation_id
                    AND operation.operation_kind = 'project'
                    AND operation.reference_id = projection.projection_id
              )
              AND (SELECT COUNT(*) FROM research_pipeline_operations operation
                   WHERE operation.graph_fingerprint = graph.graph_fingerprint
                     AND operation.operation_kind = 'project') = 1
              AND NOT EXISTS (
                  SELECT 1
                  FROM research_pipeline_operations assembly_operation
                  WHERE assembly_operation.graph_fingerprint = assembly_graph.graph_fingerprint
                    AND NOT EXISTS (
                        SELECT 1
                        FROM research_pipeline_operations projection_operation
                        WHERE projection_operation.graph_fingerprint = graph.graph_fingerprint
                          AND projection_operation.position = assembly_operation.position
                          AND projection_operation.operation_id = assembly_operation.operation_id
                          AND projection_operation.operation_kind = assembly_operation.operation_kind
                          AND projection_operation.reference_id = assembly_operation.reference_id
                          AND projection_operation.producer_call_id IS assembly_operation.producer_call_id
                          AND projection_operation.evidence_class IS assembly_operation.evidence_class
                    )
              )
              AND (SELECT COUNT(*) FROM research_pipeline_operation_inputs input
                   WHERE input.graph_fingerprint = graph.graph_fingerprint)
                  = (SELECT COUNT(*) FROM research_pipeline_operation_inputs input
                     WHERE input.graph_fingerprint = assembly_graph.graph_fingerprint) + 1
              AND NOT EXISTS (
                  SELECT 1
                  FROM research_pipeline_operation_inputs assembly_input
                  WHERE assembly_input.graph_fingerprint = assembly_graph.graph_fingerprint
                    AND NOT EXISTS (
                        SELECT 1
                        FROM research_pipeline_operation_inputs projection_input
                        WHERE projection_input.graph_fingerprint = graph.graph_fingerprint
                          AND projection_input.operation_id = assembly_input.operation_id
                          AND projection_input.position = assembly_input.position
                          AND projection_input.input_operation_id = assembly_input.input_operation_id
                    )
              )
              AND EXISTS (
                  SELECT 1
                  FROM research_pipeline_operation_inputs project_input
                  WHERE project_input.graph_fingerprint = graph.graph_fingerprint
                    AND project_input.operation_id = graph.output_operation_id
                    AND project_input.position = 0
                    AND project_input.input_operation_id = assembly_graph.output_operation_id
              )
        )
    )
    OR (
        NEW.subject_kind = 'mixed_authorship'
        AND NOT EXISTS (
            SELECT 1
            FROM research_mixed_authorship_assemblies mixed
            JOIN research_operation_graphs graph
              ON graph.graph_fingerprint = mixed.graph_fingerprint
            WHERE mixed.mixed_assembly_id = NEW.subject_id
              AND (SELECT COUNT(*) FROM research_pipeline_operations operation
                   WHERE operation.graph_fingerprint = graph.graph_fingerprint) = graph.node_count
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'research admission subject is absent or ineligible');
END;

CREATE TABLE research_user_presence_events (
    event_receipt_blob_id TEXT PRIMARY KEY REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    command_id TEXT NOT NULL UNIQUE REFERENCES command_receipts(command_id) ON DELETE RESTRICT,
    user_presence_kind TEXT NOT NULL CHECK (user_presence_kind IN (
        'editor_gesture', 'cli_interactive_confirmation',
        'native_dialog_confirmation', 'human_review_submission'
    )),
    session_fingerprint TEXT NOT NULL CHECK (length(session_fingerprint) = 64),
    monotonic_event_index INTEGER NOT NULL CHECK (monotonic_event_index > 0),
    occurred_at_ms INTEGER NOT NULL CHECK (occurred_at_ms > 0),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    UNIQUE (session_fingerprint, monotonic_event_index)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_user_presence_events_validate_insert
BEFORE INSERT ON research_user_presence_events
WHEN NOT EXISTS (
    SELECT 1
    FROM command_receipts receipt
    JOIN command_requests request ON request.command_id = receipt.command_id
    WHERE receipt.command_id = NEW.command_id
      AND receipt.command_kind = 'promote_candidate'
      AND request.command_kind = 'promote_candidate'
      AND request.created_at_ms <= NEW.occurred_at_ms
      AND NEW.occurred_at_ms <= receipt.completed_at_ms
)
BEGIN
    SELECT RAISE(ABORT, 'user-presence event is not bound to one promotion command lifetime');
END;

CREATE TABLE research_promotion_authorities (
    command_id TEXT PRIMARY KEY REFERENCES command_receipts(command_id) ON DELETE RESTRICT,
    actor TEXT NOT NULL CHECK (length(CAST(actor AS BLOB)) BETWEEN 1 AND 128),
    source_revision_id TEXT NOT NULL REFERENCES revisions(revision_id) ON DELETE RESTRICT,
    source_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    user_presence_kind TEXT NOT NULL CHECK (user_presence_kind IN (
        'editor_gesture', 'cli_interactive_confirmation',
        'native_dialog_confirmation', 'human_review_submission'
    )),
    session_fingerprint TEXT NOT NULL CHECK (length(session_fingerprint) = 64),
    event_receipt_blob_id TEXT NOT NULL UNIQUE REFERENCES research_user_presence_events(event_receipt_blob_id) ON DELETE RESTRICT,
    monotonic_event_index INTEGER NOT NULL CHECK (monotonic_event_index > 0),
    occurred_at_ms INTEGER NOT NULL CHECK (occurred_at_ms > 0),
    authority_record_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_promotion_authorities_validate_insert
BEFORE INSERT ON research_promotion_authorities
WHEN NOT EXISTS (
    SELECT 1
    FROM revisions revision
    JOIN artifacts artifact ON artifact.artifact_id = revision.artifact_id
    WHERE revision.revision_id = NEW.source_revision_id
      AND artifact.blob_id = NEW.source_blob_id
)
    OR NOT EXISTS (
        SELECT 1 FROM blobs record
        WHERE record.blob_id = NEW.authority_record_blob_id
          AND record.byte_len > 0
    )
    OR NOT EXISTS (
        SELECT 1
        FROM research_user_presence_events event
        JOIN command_receipts receipt ON receipt.command_id = event.command_id
        JOIN command_requests request ON request.command_id = receipt.command_id
        WHERE event.event_receipt_blob_id = NEW.event_receipt_blob_id
          AND event.command_id = NEW.command_id
          AND event.user_presence_kind = NEW.user_presence_kind
          AND event.session_fingerprint = NEW.session_fingerprint
          AND event.monotonic_event_index = NEW.monotonic_event_index
          AND event.occurred_at_ms = NEW.occurred_at_ms
          AND receipt.command_kind = 'promote_candidate'
          AND request.command_kind = 'promote_candidate'
          AND request.created_at_ms <= event.occurred_at_ms
          AND event.occurred_at_ms <= receipt.completed_at_ms
    )
BEGIN
    SELECT RAISE(ABORT, 'promotion authority is not pinned to its source revision');
END;

-- Existing candidates are evidence, not silently grandfathered research
-- claims.  Rust revalidation may append one terminal review event per row.
CREATE TABLE research_legacy_candidates (
    candidate_id TEXT PRIMARY KEY REFERENCES generation_candidates(candidate_id) ON DELETE RESTRICT,
    discovered_at_ms INTEGER NOT NULL CHECK (discovered_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TABLE research_legacy_candidate_review_events (
    candidate_id TEXT NOT NULL REFERENCES research_legacy_candidates(candidate_id) ON DELETE RESTRICT,
    sequence INTEGER NOT NULL CHECK (sequence >= 0),
    disposition TEXT NOT NULL CHECK (disposition IN ('pending', 'migrated_single_span', 'quarantined')),
    assembly_id TEXT REFERENCES research_candidate_assemblies(assembly_id) ON DELETE RESTRICT,
    reason TEXT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    PRIMARY KEY (candidate_id, sequence),
    CHECK (
        (disposition = 'pending' AND sequence = 0 AND assembly_id IS NULL AND reason IS NULL)
        OR (disposition = 'migrated_single_span' AND sequence > 0 AND assembly_id IS NOT NULL AND reason IS NULL)
        OR (disposition = 'quarantined' AND sequence > 0 AND assembly_id IS NULL
            AND length(CAST(reason AS BLOB)) BETWEEN 1 AND 4096)
    )
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_legacy_reviews_validate_insert
BEFORE INSERT ON research_legacy_candidate_review_events
WHEN (NEW.sequence = 0 AND EXISTS (
        SELECT 1 FROM research_legacy_candidate_review_events prior
        WHERE prior.candidate_id = NEW.candidate_id
    ))
    OR (NEW.sequence > 0 AND NOT EXISTS (
        SELECT 1 FROM research_legacy_candidate_review_events prior
        WHERE prior.candidate_id = NEW.candidate_id
          AND prior.sequence = NEW.sequence - 1
    ))
    OR EXISTS (
        SELECT 1 FROM research_legacy_candidate_review_events prior
        WHERE prior.candidate_id = NEW.candidate_id
          AND prior.disposition != 'pending'
    )
BEGIN
    SELECT RAISE(ABORT, 'legacy review events must append once after pending');
END;

INSERT INTO research_legacy_candidates(candidate_id, discovered_at_ms)
SELECT candidate_id, CASE WHEN created_at_ms > 0 THEN created_at_ms ELSE 1 END
FROM generation_candidates;

INSERT INTO research_legacy_candidate_review_events(
    candidate_id, sequence, disposition, assembly_id, reason, created_at_ms
)
SELECT candidate_id, 0, 'pending', NULL, NULL, discovered_at_ms
FROM research_legacy_candidates;

-- Every semantic research table is immutable.  Mutable campaign state will be
-- represented by later append-only event tables, never updates here.
CREATE TRIGGER research_model_calls_immutable_update BEFORE UPDATE ON research_model_calls BEGIN SELECT RAISE(ABORT, 'research model calls are immutable'); END;
CREATE TRIGGER research_model_calls_immutable_delete BEFORE DELETE ON research_model_calls BEGIN SELECT RAISE(ABORT, 'research model calls are immutable'); END;
CREATE TRIGGER research_call_terminals_immutable_update BEFORE UPDATE ON research_call_terminals BEGIN SELECT RAISE(ABORT, 'research call terminals are immutable'); END;
CREATE TRIGGER research_call_terminals_immutable_delete BEFORE DELETE ON research_call_terminals BEGIN SELECT RAISE(ABORT, 'research call terminals are immutable'); END;
CREATE TRIGGER research_output_projections_immutable_update BEFORE UPDATE ON research_output_projections BEGIN SELECT RAISE(ABORT, 'research output projections are immutable'); END;
CREATE TRIGGER research_output_projections_immutable_delete BEFORE DELETE ON research_output_projections BEGIN SELECT RAISE(ABORT, 'research output projections are immutable'); END;
CREATE TRIGGER research_generated_spans_immutable_update BEFORE UPDATE ON research_generated_span_occurrences BEGIN SELECT RAISE(ABORT, 'research generated spans are immutable'); END;
CREATE TRIGGER research_generated_spans_immutable_delete BEFORE DELETE ON research_generated_span_occurrences BEGIN SELECT RAISE(ABORT, 'research generated spans are immutable'); END;
CREATE TRIGGER research_operation_graphs_immutable_update BEFORE UPDATE ON research_operation_graphs BEGIN SELECT RAISE(ABORT, 'research operation graphs are immutable'); END;
CREATE TRIGGER research_operation_graphs_immutable_delete BEFORE DELETE ON research_operation_graphs BEGIN SELECT RAISE(ABORT, 'research operation graphs are immutable'); END;
CREATE TRIGGER research_pipeline_operations_immutable_update BEFORE UPDATE ON research_pipeline_operations BEGIN SELECT RAISE(ABORT, 'research pipeline operations are immutable'); END;
CREATE TRIGGER research_pipeline_operations_immutable_delete BEFORE DELETE ON research_pipeline_operations BEGIN SELECT RAISE(ABORT, 'research pipeline operations are immutable'); END;
CREATE TRIGGER research_pipeline_inputs_immutable_update BEFORE UPDATE ON research_pipeline_operation_inputs BEGIN SELECT RAISE(ABORT, 'research pipeline inputs are immutable'); END;
CREATE TRIGGER research_pipeline_inputs_immutable_delete BEFORE DELETE ON research_pipeline_operation_inputs BEGIN SELECT RAISE(ABORT, 'research pipeline inputs are immutable'); END;
CREATE TRIGGER research_candidate_assemblies_immutable_update BEFORE UPDATE ON research_candidate_assemblies BEGIN SELECT RAISE(ABORT, 'research candidate assemblies are immutable'); END;
CREATE TRIGGER research_candidate_assemblies_immutable_delete BEFORE DELETE ON research_candidate_assemblies BEGIN SELECT RAISE(ABORT, 'research candidate assemblies are immutable'); END;
CREATE TRIGGER research_assembly_parts_immutable_update BEFORE UPDATE ON research_candidate_assembly_parts BEGIN SELECT RAISE(ABORT, 'research assembly parts are immutable'); END;
CREATE TRIGGER research_assembly_parts_immutable_delete BEFORE DELETE ON research_candidate_assembly_parts BEGIN SELECT RAISE(ABORT, 'research assembly parts are immutable'); END;
CREATE TRIGGER research_candidate_projections_immutable_update BEFORE UPDATE ON research_candidate_projections BEGIN SELECT RAISE(ABORT, 'research candidate projections are immutable'); END;
CREATE TRIGGER research_candidate_projections_immutable_delete BEFORE DELETE ON research_candidate_projections BEGIN SELECT RAISE(ABORT, 'research candidate projections are immutable'); END;
CREATE TRIGGER research_mixed_assemblies_immutable_update BEFORE UPDATE ON research_mixed_authorship_assemblies BEGIN SELECT RAISE(ABORT, 'research mixed assemblies are immutable'); END;
CREATE TRIGGER research_mixed_assemblies_immutable_delete BEFORE DELETE ON research_mixed_authorship_assemblies BEGIN SELECT RAISE(ABORT, 'research mixed assemblies are immutable'); END;
CREATE TRIGGER research_admissions_immutable_update BEFORE UPDATE ON research_admissions BEGIN SELECT RAISE(ABORT, 'research admissions are immutable'); END;
CREATE TRIGGER research_admissions_immutable_delete BEFORE DELETE ON research_admissions BEGIN SELECT RAISE(ABORT, 'research admissions are immutable'); END;
CREATE TRIGGER research_user_presence_events_immutable_update BEFORE UPDATE ON research_user_presence_events BEGIN SELECT RAISE(ABORT, 'research user-presence events are immutable'); END;
CREATE TRIGGER research_user_presence_events_immutable_delete BEFORE DELETE ON research_user_presence_events BEGIN SELECT RAISE(ABORT, 'research user-presence events are immutable'); END;
CREATE TRIGGER research_promotion_authorities_immutable_update BEFORE UPDATE ON research_promotion_authorities BEGIN SELECT RAISE(ABORT, 'research promotion authorities are immutable'); END;
CREATE TRIGGER research_promotion_authorities_immutable_delete BEFORE DELETE ON research_promotion_authorities BEGIN SELECT RAISE(ABORT, 'research promotion authorities are immutable'); END;
CREATE TRIGGER research_legacy_candidates_immutable_update BEFORE UPDATE ON research_legacy_candidates BEGIN SELECT RAISE(ABORT, 'research legacy candidates are immutable'); END;
CREATE TRIGGER research_legacy_candidates_immutable_delete BEFORE DELETE ON research_legacy_candidates BEGIN SELECT RAISE(ABORT, 'research legacy candidates are immutable'); END;
CREATE TRIGGER research_legacy_reviews_immutable_update BEFORE UPDATE ON research_legacy_candidate_review_events BEGIN SELECT RAISE(ABORT, 'research legacy review events are immutable'); END;
CREATE TRIGGER research_legacy_reviews_immutable_delete BEFORE DELETE ON research_legacy_candidate_review_events BEGIN SELECT RAISE(ABORT, 'research legacy review events are immutable'); END;
