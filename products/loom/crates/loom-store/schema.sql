-- Current unreleased Loom schema. Changes replace this schema; there are no upgrade paths.

CREATE TABLE blobs (
    blob_id TEXT PRIMARY KEY,
    byte_len INTEGER NOT NULL CHECK (byte_len >= 0),
    media_type TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE artifacts (
    artifact_id TEXT PRIMARY KEY,
    blob_id TEXT NOT NULL REFERENCES blobs(blob_id),
    artifact_kind TEXT NOT NULL,
    media_type TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE operations (
    operation_id TEXT PRIMARY KEY,
    operation_kind TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE operation_inputs (
    operation_id TEXT NOT NULL REFERENCES operations(operation_id),
    position INTEGER NOT NULL CHECK (position >= 0),
    artifact_id TEXT NOT NULL REFERENCES artifacts(artifact_id),
    PRIMARY KEY (operation_id, position)
) STRICT, WITHOUT ROWID;

CREATE TABLE operation_outputs (
    operation_id TEXT NOT NULL REFERENCES operations(operation_id),
    position INTEGER NOT NULL CHECK (position >= 0),
    artifact_id TEXT NOT NULL UNIQUE REFERENCES artifacts(artifact_id),
    PRIMARY KEY (operation_id, position)
) STRICT, WITHOUT ROWID;

CREATE TABLE documents (
    document_id TEXT PRIMARY KEY,
    relative_path TEXT NOT NULL UNIQUE,
    document_kind TEXT NOT NULL CHECK (document_kind IN ('prose', 'verse', 'hybrid')),
    created_at_ms INTEGER NOT NULL
, display_title TEXT
    CHECK (
        display_title IS NULL OR (
            length(CAST(display_title AS BLOB)) BETWEEN 1 AND 256
            -- Rust's `str::trim` follows Unicode White_Space, while SQLite's
            -- one-argument trim only recognizes U+0020. Keep the database
            -- canonical representation identical to the Rust command path.
            AND unicode(substr(display_title, 1, 1)) NOT IN (
                9, 10, 11, 12, 13, 32, 133, 160, 5760,
                8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200, 8201, 8202,
                8232, 8233, 8239, 8287, 12288
            )
            AND unicode(substr(display_title, -1, 1)) NOT IN (
                9, 10, 11, 12, 13, 32, 133, 160, 5760,
                8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200, 8201, 8202,
                8232, 8233, 8239, 8287, 12288
            )
            -- `char::is_control` is Unicode General_Category=Cc: U+0000-001F
            -- and U+007F-009F. NUL needs its own predicate because it cannot
            -- participate in a SQLite GLOB pattern.
            AND instr(display_title, char(0)) = 0
            AND display_title NOT GLOB ('*[' || char(1) || '-' || char(31) || ']*')
            AND display_title NOT GLOB ('*[' || char(127) || '-' || char(159) || ']*')
        )
    )) STRICT, WITHOUT ROWID;

CREATE TABLE revisions (
    revision_id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES documents(document_id),
    parent_revision_id TEXT REFERENCES revisions(revision_id),
    artifact_id TEXT NOT NULL UNIQUE REFERENCES artifacts(artifact_id),
    reason TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE revision_segments (
    revision_id TEXT NOT NULL REFERENCES revisions(revision_id),
    position INTEGER NOT NULL CHECK (position >= 0),
    artifact_id TEXT NOT NULL REFERENCES artifacts(artifact_id),
    start_byte INTEGER NOT NULL CHECK (start_byte >= 0),
    end_byte INTEGER NOT NULL CHECK (end_byte >= start_byte),
    contribution_kind TEXT NOT NULL CHECK (contribution_kind IN ('human', 'generated', 'mixed', 'source')),
    PRIMARY KEY (revision_id, position)
) STRICT, WITHOUT ROWID;

CREATE TABLE visible_file_outbox (
    outbox_id INTEGER PRIMARY KEY,
    revision_id TEXT NOT NULL UNIQUE REFERENCES revisions(revision_id),
    relative_path TEXT NOT NULL,
    target_blob_id TEXT NOT NULL REFERENCES blobs(blob_id),
    expected_visible_blob_id TEXT,
    state TEXT NOT NULL CHECK (state IN ('pending', 'completed')),
    created_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER
) STRICT;

CREATE TABLE command_receipts (
    command_id TEXT PRIMARY KEY,
    command_kind TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    completed_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE INDEX revisions_by_document
ON revisions(document_id, created_at_ms DESC, revision_id DESC);

CREATE INDEX pending_visible_file_outbox
ON visible_file_outbox(state, outbox_id);

CREATE TRIGGER blobs_are_immutable_update
BEFORE UPDATE ON blobs BEGIN
    SELECT RAISE(ABORT, 'blobs are immutable');
END;

CREATE TRIGGER blobs_are_immutable_delete
BEFORE DELETE ON blobs BEGIN
    SELECT RAISE(ABORT, 'blobs are immutable');
END;

CREATE TRIGGER artifacts_are_immutable_update
BEFORE UPDATE ON artifacts BEGIN
    SELECT RAISE(ABORT, 'artifacts are immutable');
END;

CREATE TRIGGER artifacts_are_immutable_delete
BEFORE DELETE ON artifacts BEGIN
    SELECT RAISE(ABORT, 'artifacts are immutable');
END;

CREATE TRIGGER operations_are_immutable_update
BEFORE UPDATE ON operations BEGIN
    SELECT RAISE(ABORT, 'operations are immutable');
END;

CREATE TRIGGER operations_are_immutable_delete
BEFORE DELETE ON operations BEGIN
    SELECT RAISE(ABORT, 'operations are immutable');
END;

CREATE TRIGGER operation_inputs_are_immutable_update
BEFORE UPDATE ON operation_inputs BEGIN
    SELECT RAISE(ABORT, 'operation inputs are immutable');
END;

CREATE TRIGGER operation_inputs_are_immutable_delete
BEFORE DELETE ON operation_inputs BEGIN
    SELECT RAISE(ABORT, 'operation inputs are immutable');
END;

CREATE TRIGGER operation_outputs_are_immutable_update
BEFORE UPDATE ON operation_outputs BEGIN
    SELECT RAISE(ABORT, 'operation outputs are immutable');
END;

CREATE TRIGGER operation_outputs_are_immutable_delete
BEFORE DELETE ON operation_outputs BEGIN
    SELECT RAISE(ABORT, 'operation outputs are immutable');
END;

CREATE TRIGGER revisions_are_immutable_update
BEFORE UPDATE ON revisions BEGIN
    SELECT RAISE(ABORT, 'revisions are immutable');
END;

CREATE TRIGGER revisions_are_immutable_delete
BEFORE DELETE ON revisions BEGIN
    SELECT RAISE(ABORT, 'revisions are immutable');
END;

CREATE TRIGGER revision_segments_are_immutable_update
BEFORE UPDATE ON revision_segments BEGIN
    SELECT RAISE(ABORT, 'revision segments are immutable');
END;

CREATE TRIGGER revision_segments_are_immutable_delete
BEFORE DELETE ON revision_segments BEGIN
    SELECT RAISE(ABORT, 'revision segments are immutable');
END;

CREATE TRIGGER command_receipts_are_immutable_update
BEFORE UPDATE ON command_receipts BEGIN
    SELECT RAISE(ABORT, 'command receipts are immutable');
END;

CREATE TRIGGER command_receipts_are_immutable_delete
BEFORE DELETE ON command_receipts BEGIN
    SELECT RAISE(ABORT, 'command receipts are immutable');
END;

CREATE TABLE model_environments (
    artifact_id TEXT PRIMARY KEY REFERENCES artifacts(artifact_id),
    environment_id TEXT NOT NULL UNIQUE,
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE prompt_recipes (
    artifact_id TEXT PRIMARY KEY REFERENCES artifacts(artifact_id),
    exact_prompt_blob_id TEXT NOT NULL REFERENCES blobs(blob_id),
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE prompt_recipe_inputs (
    recipe_artifact_id TEXT NOT NULL REFERENCES prompt_recipes(artifact_id),
    position INTEGER NOT NULL CHECK (position >= 0),
    input_artifact_id TEXT NOT NULL REFERENCES artifacts(artifact_id),
    PRIMARY KEY (recipe_artifact_id, position)
) STRICT, WITHOUT ROWID;

CREATE TABLE context_recipes (
    artifact_id TEXT PRIMARY KEY REFERENCES artifacts(artifact_id),
    source_revision_id TEXT NOT NULL REFERENCES revisions(revision_id),
    retrieval_evidence_blob_id TEXT REFERENCES blobs(blob_id),
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE context_recipe_sources (
    recipe_artifact_id TEXT NOT NULL REFERENCES context_recipes(artifact_id),
    position INTEGER NOT NULL CHECK (position >= 0),
    source_artifact_id TEXT NOT NULL REFERENCES artifacts(artifact_id),
    PRIMARY KEY (recipe_artifact_id, position)
) STRICT, WITHOUT ROWID;

CREATE TABLE authority_policies (
    artifact_id TEXT PRIMARY KEY REFERENCES artifacts(artifact_id),
    policy_version INTEGER NOT NULL CHECK (policy_version > 0),
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE authority_policy_members (
    policy_artifact_id TEXT NOT NULL REFERENCES authority_policies(artifact_id),
    environment_artifact_id TEXT NOT NULL REFERENCES model_environments(artifact_id),
    role TEXT NOT NULL CHECK (role IN ('writer', 'critic')),
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (policy_artifact_id, role, position),
    UNIQUE (policy_artifact_id, environment_artifact_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE branches (
    branch_id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES documents(document_id),
    source_revision_id TEXT NOT NULL REFERENCES revisions(revision_id),
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE generation_runs (
    run_id TEXT PRIMARY KEY,
    branch_id TEXT NOT NULL UNIQUE REFERENCES branches(branch_id),
    run_artifact_id TEXT NOT NULL UNIQUE REFERENCES artifacts(artifact_id),
    document_id TEXT NOT NULL REFERENCES documents(document_id),
    source_revision_id TEXT NOT NULL REFERENCES revisions(revision_id),
    source_blob_id TEXT NOT NULL REFERENCES blobs(blob_id),
    target_start_byte INTEGER NOT NULL CHECK (target_start_byte >= 0),
    target_end_byte INTEGER NOT NULL CHECK (target_end_byte >= target_start_byte),
    model_environment_artifact_id TEXT NOT NULL REFERENCES model_environments(artifact_id),
    prompt_recipe_artifact_id TEXT NOT NULL REFERENCES prompt_recipes(artifact_id),
    context_recipe_artifact_id TEXT NOT NULL REFERENCES context_recipes(artifact_id),
    authority_policy_artifact_id TEXT NOT NULL REFERENCES authority_policies(artifact_id),
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE generation_events (
    event_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES generation_runs(run_id),
    sequence INTEGER NOT NULL CHECK (sequence >= 0),
    event_kind TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    is_terminal INTEGER NOT NULL CHECK (is_terminal IN (0, 1)),
    created_at_ms INTEGER NOT NULL,
    UNIQUE (run_id, sequence)
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX one_generation_terminal_event
ON generation_events(run_id) WHERE is_terminal = 1;

CREATE TABLE generation_candidates (
    candidate_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL UNIQUE REFERENCES generation_runs(run_id),
    generated_span_artifact_id TEXT NOT NULL UNIQUE REFERENCES artifacts(artifact_id),
    token_trace_artifact_id TEXT NOT NULL UNIQUE REFERENCES artifacts(artifact_id),
    output_blob_id TEXT NOT NULL REFERENCES blobs(blob_id),
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE generation_terminals (
    run_id TEXT PRIMARY KEY REFERENCES generation_runs(run_id),
    event_id TEXT NOT NULL UNIQUE REFERENCES generation_events(event_id),
    status TEXT NOT NULL CHECK (status IN ('completed', 'cancelled', 'failed', 'pruned', 'rejected')),
    candidate_id TEXT REFERENCES generation_candidates(candidate_id),
    error TEXT,
    created_at_ms INTEGER NOT NULL,
    CHECK (
        (status = 'completed' AND candidate_id IS NOT NULL AND error IS NULL)
        OR (status IN ('cancelled', 'pruned', 'rejected') AND candidate_id IS NULL)
        OR (status = 'failed' AND candidate_id IS NULL AND error IS NOT NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE TABLE selection_events (
    selection_artifact_id TEXT PRIMARY KEY REFERENCES artifacts(artifact_id),
    selection_id TEXT NOT NULL UNIQUE,
    candidate_id TEXT NOT NULL REFERENCES generation_candidates(candidate_id),
    decision TEXT NOT NULL CHECK (decision IN ('promote', 'keep_alternative', 'reject')),
    source_revision_id TEXT NOT NULL REFERENCES revisions(revision_id),
    resulting_revision_id TEXT REFERENCES revisions(revision_id),
    command_id TEXT NOT NULL UNIQUE REFERENCES command_receipts(command_id),
    created_at_ms INTEGER NOT NULL,
    CHECK (
        (decision = 'promote' AND resulting_revision_id IS NOT NULL)
        OR (decision != 'promote' AND resulting_revision_id IS NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE TABLE authorship_attestations (
    attestation_artifact_id TEXT PRIMARY KEY REFERENCES artifacts(artifact_id),
    candidate_id TEXT NOT NULL REFERENCES generation_candidates(candidate_id),
    generated_span_artifact_id TEXT NOT NULL REFERENCES artifacts(artifact_id),
    promoted_revision_id TEXT NOT NULL UNIQUE REFERENCES revisions(revision_id),
    promotion_command_id TEXT NOT NULL UNIQUE REFERENCES command_receipts(command_id),
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE command_requests (
    command_id TEXT PRIMARY KEY REFERENCES command_receipts(command_id),
    request_fingerprint TEXT NOT NULL,
    command_kind TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TRIGGER generation_events_stop_after_terminal
BEFORE INSERT ON generation_events
WHEN EXISTS (
    SELECT 1 FROM generation_events
    WHERE run_id = NEW.run_id AND is_terminal = 1
)
BEGIN
    SELECT RAISE(ABORT, 'generation already has a terminal event');
END;

CREATE TRIGGER model_environments_are_immutable_update
BEFORE UPDATE ON model_environments BEGIN SELECT RAISE(ABORT, 'model environments are immutable'); END;

CREATE TRIGGER model_environments_are_immutable_delete
BEFORE DELETE ON model_environments BEGIN SELECT RAISE(ABORT, 'model environments are immutable'); END;

CREATE TRIGGER prompt_recipes_are_immutable_update
BEFORE UPDATE ON prompt_recipes BEGIN SELECT RAISE(ABORT, 'prompt recipes are immutable'); END;

CREATE TRIGGER prompt_recipes_are_immutable_delete
BEFORE DELETE ON prompt_recipes BEGIN SELECT RAISE(ABORT, 'prompt recipes are immutable'); END;

CREATE TRIGGER prompt_recipe_inputs_are_immutable_update
BEFORE UPDATE ON prompt_recipe_inputs BEGIN SELECT RAISE(ABORT, 'prompt recipe inputs are immutable'); END;

CREATE TRIGGER prompt_recipe_inputs_are_immutable_delete
BEFORE DELETE ON prompt_recipe_inputs BEGIN SELECT RAISE(ABORT, 'prompt recipe inputs are immutable'); END;

CREATE TRIGGER context_recipes_are_immutable_update
BEFORE UPDATE ON context_recipes BEGIN SELECT RAISE(ABORT, 'context recipes are immutable'); END;

CREATE TRIGGER context_recipes_are_immutable_delete
BEFORE DELETE ON context_recipes BEGIN SELECT RAISE(ABORT, 'context recipes are immutable'); END;

CREATE TRIGGER context_recipe_sources_are_immutable_update
BEFORE UPDATE ON context_recipe_sources BEGIN SELECT RAISE(ABORT, 'context recipe sources are immutable'); END;

CREATE TRIGGER context_recipe_sources_are_immutable_delete
BEFORE DELETE ON context_recipe_sources BEGIN SELECT RAISE(ABORT, 'context recipe sources are immutable'); END;

CREATE TRIGGER authority_policies_are_immutable_update
BEFORE UPDATE ON authority_policies BEGIN SELECT RAISE(ABORT, 'authority policies are immutable'); END;

CREATE TRIGGER authority_policies_are_immutable_delete
BEFORE DELETE ON authority_policies BEGIN SELECT RAISE(ABORT, 'authority policies are immutable'); END;

CREATE TRIGGER authority_policy_members_are_immutable_update
BEFORE UPDATE ON authority_policy_members BEGIN SELECT RAISE(ABORT, 'authority policy members are immutable'); END;

CREATE TRIGGER authority_policy_members_are_immutable_delete
BEFORE DELETE ON authority_policy_members BEGIN SELECT RAISE(ABORT, 'authority policy members are immutable'); END;

CREATE TRIGGER branches_are_immutable_update
BEFORE UPDATE ON branches BEGIN SELECT RAISE(ABORT, 'branches are immutable'); END;

CREATE TRIGGER branches_are_immutable_delete
BEFORE DELETE ON branches BEGIN SELECT RAISE(ABORT, 'branches are immutable'); END;

CREATE TRIGGER generation_runs_are_immutable_update
BEFORE UPDATE ON generation_runs BEGIN SELECT RAISE(ABORT, 'generation runs are immutable'); END;

CREATE TRIGGER generation_runs_are_immutable_delete
BEFORE DELETE ON generation_runs BEGIN SELECT RAISE(ABORT, 'generation runs are immutable'); END;

CREATE TRIGGER generation_events_are_immutable_update
BEFORE UPDATE ON generation_events BEGIN SELECT RAISE(ABORT, 'generation events are immutable'); END;

CREATE TRIGGER generation_events_are_immutable_delete
BEFORE DELETE ON generation_events BEGIN SELECT RAISE(ABORT, 'generation events are immutable'); END;

CREATE TRIGGER generation_candidates_are_immutable_update
BEFORE UPDATE ON generation_candidates BEGIN SELECT RAISE(ABORT, 'generation candidates are immutable'); END;

CREATE TRIGGER generation_candidates_are_immutable_delete
BEFORE DELETE ON generation_candidates BEGIN SELECT RAISE(ABORT, 'generation candidates are immutable'); END;

CREATE TRIGGER generation_terminals_are_immutable_update
BEFORE UPDATE ON generation_terminals BEGIN SELECT RAISE(ABORT, 'generation terminals are immutable'); END;

CREATE TRIGGER generation_terminals_are_immutable_delete
BEFORE DELETE ON generation_terminals BEGIN SELECT RAISE(ABORT, 'generation terminals are immutable'); END;

CREATE TRIGGER selection_events_are_immutable_update
BEFORE UPDATE ON selection_events BEGIN SELECT RAISE(ABORT, 'selection events are immutable'); END;

CREATE TRIGGER selection_events_are_immutable_delete
BEFORE DELETE ON selection_events BEGIN SELECT RAISE(ABORT, 'selection events are immutable'); END;

CREATE TRIGGER authorship_attestations_are_immutable_update
BEFORE UPDATE ON authorship_attestations BEGIN SELECT RAISE(ABORT, 'authorship attestations are immutable'); END;

CREATE TRIGGER authorship_attestations_are_immutable_delete
BEFORE DELETE ON authorship_attestations BEGIN SELECT RAISE(ABORT, 'authorship attestations are immutable'); END;

CREATE TRIGGER command_requests_are_immutable_update
BEFORE UPDATE ON command_requests BEGIN SELECT RAISE(ABORT, 'command requests are immutable'); END;

CREATE TRIGGER command_requests_are_immutable_delete
BEFORE DELETE ON command_requests BEGIN SELECT RAISE(ABORT, 'command requests are immutable'); END;

CREATE TABLE transient_drafts (
    document_id TEXT PRIMARY KEY NOT NULL
        REFERENCES documents(document_id) ON DELETE RESTRICT,
    source_revision_id TEXT NOT NULL
        REFERENCES revisions(revision_id) ON DELETE RESTRICT,
    draft_blob_id TEXT NOT NULL CHECK (length(draft_blob_id) = 64),
    storage_slot INTEGER NOT NULL CHECK (storage_slot IN (0, 1)),
    draft_version INTEGER NOT NULL CHECK (draft_version > 0),
    updated_at_ms INTEGER NOT NULL
, base_version INTEGER NOT NULL DEFAULT 0
        CHECK (base_version >= 0 AND base_version < draft_version)) STRICT, WITHOUT ROWID;

CREATE INDEX transient_drafts_source_revision
    ON transient_drafts(source_revision_id);

CREATE TRIGGER transient_drafts_source_document_insert
BEFORE INSERT ON transient_drafts
WHEN NOT EXISTS (
    SELECT 1 FROM revisions
    WHERE revision_id = NEW.source_revision_id
      AND document_id = NEW.document_id
)
BEGIN
    SELECT RAISE(ABORT, 'transient draft source revision belongs to another document');
END;

CREATE TRIGGER transient_drafts_source_document_update
BEFORE UPDATE ON transient_drafts
WHEN NOT EXISTS (
    SELECT 1 FROM revisions
    WHERE revision_id = NEW.source_revision_id
      AND document_id = NEW.document_id
)
BEGIN
    SELECT RAISE(ABORT, 'transient draft source revision belongs to another document');
END;

CREATE TABLE transient_draft_sequences (
    document_id TEXT PRIMARY KEY NOT NULL
        REFERENCES documents(document_id) ON DELETE RESTRICT,
    last_version INTEGER NOT NULL CHECK (last_version > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER transient_draft_sequences_only_advance
BEFORE UPDATE ON transient_draft_sequences
WHEN NEW.last_version <= OLD.last_version
BEGIN
    SELECT RAISE(ABORT, 'transient draft sequence must advance monotonically');
END;

CREATE TRIGGER transient_draft_sequences_cannot_delete
BEFORE DELETE ON transient_draft_sequences
BEGIN
    SELECT RAISE(ABORT, 'transient draft sequence cannot be deleted');
END;

CREATE TABLE generation_terminal_evidence (
    run_id TEXT PRIMARY KEY
        REFERENCES generation_terminals(run_id) ON DELETE RESTRICT,
    operation_id TEXT NOT NULL UNIQUE
        REFERENCES operations(operation_id) ON DELETE RESTRICT,
    output_artifact_id TEXT NOT NULL UNIQUE
        REFERENCES artifacts(artifact_id) ON DELETE RESTRICT,
    output_blob_id TEXT NOT NULL
        REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    token_trace_artifact_id TEXT NOT NULL UNIQUE
        REFERENCES artifacts(artifact_id) ON DELETE RESTRICT,
    candidate_id TEXT UNIQUE
        REFERENCES generation_candidates(candidate_id) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE generation_command_events (
    command_id TEXT PRIMARY KEY
        REFERENCES command_requests(command_id) ON DELETE RESTRICT,
    event_id TEXT NOT NULL UNIQUE
        REFERENCES generation_events(event_id) ON DELETE RESTRICT
) STRICT, WITHOUT ROWID;

CREATE TRIGGER generation_terminal_evidence_validate_insert
BEFORE INSERT ON generation_terminal_evidence
WHEN NOT EXISTS (
        SELECT 1
        FROM generation_terminals gt
        WHERE gt.run_id = NEW.run_id
          AND (
              (gt.status = 'completed' AND gt.candidate_id = NEW.candidate_id)
              OR (gt.status != 'completed' AND gt.candidate_id IS NULL AND NEW.candidate_id IS NULL)
          )
    )
    OR NOT EXISTS (
        SELECT 1
        FROM operation_outputs oo
        WHERE oo.operation_id = NEW.operation_id
          AND oo.artifact_id = NEW.output_artifact_id
    )
    OR NOT EXISTS (
        SELECT 1
        FROM operation_outputs oo
        WHERE oo.operation_id = NEW.operation_id
          AND oo.artifact_id = NEW.token_trace_artifact_id
    )
    OR NOT EXISTS (
        SELECT 1
        FROM artifacts a
        WHERE a.artifact_id = NEW.output_artifact_id
          AND a.blob_id = NEW.output_blob_id
    )
BEGIN
    SELECT RAISE(ABORT, 'terminal evidence does not match its terminal or producing operation');
END;

CREATE TRIGGER generation_command_events_validate_insert
BEFORE INSERT ON generation_command_events
WHEN NOT EXISTS (
        SELECT 1
        FROM command_requests cr
        WHERE cr.command_id = NEW.command_id
          AND cr.command_kind = 'cancel_generation'
    )
    OR NOT EXISTS (
        SELECT 1
        FROM generation_events ge
        WHERE ge.event_id = NEW.event_id
          AND ge.event_kind = 'cancellation_requested'
          AND ge.is_terminal = 0
    )
BEGIN
    SELECT RAISE(ABORT, 'generation command event does not match a cancellation request');
END;

CREATE TRIGGER generation_terminal_evidence_are_immutable_update
BEFORE UPDATE ON generation_terminal_evidence
BEGIN
    SELECT RAISE(ABORT, 'generation terminal evidence is immutable');
END;

CREATE TRIGGER generation_terminal_evidence_are_immutable_delete
BEFORE DELETE ON generation_terminal_evidence
BEGIN
    SELECT RAISE(ABORT, 'generation terminal evidence is immutable');
END;

CREATE TRIGGER generation_command_events_are_immutable_update
BEFORE UPDATE ON generation_command_events
BEGIN
    SELECT RAISE(ABORT, 'generation command events are immutable');
END;

CREATE TRIGGER generation_command_events_are_immutable_delete
BEFORE DELETE ON generation_command_events
BEGIN
    SELECT RAISE(ABORT, 'generation command events are immutable');
END;

CREATE TABLE generation_run_index (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL UNIQUE REFERENCES generation_runs(run_id),
    seed_decimal TEXT CHECK (
        seed_decimal IS NULL
        OR (
            length(seed_decimal) BETWEEN 1 AND 20
            AND seed_decimal NOT GLOB '*[^0-9]*'
        )
    ),
    model_identifier TEXT CHECK (
        model_identifier IS NULL
        OR length(CAST(model_identifier AS BLOB)) <= 4096
    )
) STRICT;

CREATE INDEX generation_run_index_run_sequence
ON generation_run_index(run_id, sequence);

CREATE TRIGGER generation_run_index_are_immutable_update
BEFORE UPDATE ON generation_run_index
BEGIN
    SELECT RAISE(ABORT, 'generation run index entries are immutable');
END;

CREATE TRIGGER generation_run_index_are_immutable_delete
BEFORE DELETE ON generation_run_index
BEGIN
    SELECT RAISE(ABORT, 'generation run index entries are immutable');
END;

CREATE TABLE generation_weave_commands (
    run_id TEXT PRIMARY KEY
        REFERENCES generation_runs(run_id) ON DELETE RESTRICT,
    command_id TEXT NOT NULL
        REFERENCES command_receipts(command_id) ON DELETE RESTRICT,
    run_artifact_id TEXT NOT NULL UNIQUE
        REFERENCES artifacts(artifact_id) ON DELETE RESTRICT
) STRICT, WITHOUT ROWID;

CREATE INDEX generation_weave_commands_command_id
ON generation_weave_commands(command_id);

CREATE TRIGGER generation_weave_commands_validate_insert
BEFORE INSERT ON generation_weave_commands
WHEN NOT EXISTS (
    SELECT 1
    FROM generation_runs run
    JOIN artifacts run_artifact ON run_artifact.artifact_id = NEW.run_artifact_id
    JOIN command_requests request ON request.command_id = NEW.command_id
    JOIN command_receipts receipt ON receipt.command_id = request.command_id
    CROSS JOIN json_each(
        CASE
            WHEN json_valid(receipt.receipt_json) THEN receipt.receipt_json
            ELSE '{}'
        END,
        '$.resulting_artifact_ids'
    ) artifact_member
    JOIN json_each(
        CASE
            WHEN json_valid(receipt.receipt_json) THEN receipt.receipt_json
            ELSE '{}'
        END,
        '$.resulting_operation_ids'
    ) operation_member ON operation_member.key = artifact_member.key
    JOIN operations generation_operation ON generation_operation.operation_id = operation_member.value
    JOIN operation_outputs operation_output
      ON operation_output.operation_id = operation_member.value
     AND operation_output.position = 0
     AND operation_output.artifact_id = artifact_member.value
    WHERE run.run_id = NEW.run_id
      AND run.run_artifact_id = NEW.run_artifact_id
      AND artifact_member.value = NEW.run_artifact_id
      AND artifact_member.type = 'text'
      AND operation_member.type = 'text'
      AND run_artifact.artifact_kind = 'generation_run'
      AND json_valid(run_artifact.metadata_json)
      AND json_type(run_artifact.metadata_json, '$.run_id') = 'text'
      AND json_extract(run_artifact.metadata_json, '$.run_id') = run.run_id
      AND generation_operation.operation_kind = 'generate'
      AND json_valid(generation_operation.metadata_json)
      AND json_type(generation_operation.metadata_json, '$.run_id') = 'text'
      AND json_extract(generation_operation.metadata_json, '$.run_id') = run.run_id
      AND NOT EXISTS (
          SELECT 1
          FROM operation_outputs other_output
          WHERE other_output.operation_id = operation_member.value
            AND (
                other_output.position <> 0
                OR other_output.artifact_id <> artifact_member.value
            )
      )
      AND request.command_kind = 'weave'
      AND receipt.command_kind = 'weave'
      AND json_valid(receipt.receipt_json)
      AND json_type(receipt.receipt_json, '$.command_id') = 'text'
      AND json_extract(receipt.receipt_json, '$.command_id') = NEW.command_id
      AND json_type(receipt.receipt_json, '$.command') = 'text'
      AND json_extract(receipt.receipt_json, '$.command') = 'weave'
      AND json_type(receipt.receipt_json, '$.source_revision_id') = 'text'
      AND json_extract(receipt.receipt_json, '$.source_revision_id') = run.source_revision_id
      AND json_type(receipt.receipt_json, '$.resulting_artifact_ids') = 'array'
      AND json_array_length(receipt.receipt_json, '$.resulting_artifact_ids') BETWEEN 1 AND 64
      AND json_type(receipt.receipt_json, '$.resulting_operation_ids') = 'array'
      AND json_array_length(receipt.receipt_json, '$.resulting_operation_ids')
          = json_array_length(receipt.receipt_json, '$.resulting_artifact_ids')
      AND json_type(receipt.receipt_json, '$.resulting_revision_ids') = 'array'
      AND json_array_length(receipt.receipt_json, '$.resulting_revision_ids') = 0
)
BEGIN
    SELECT RAISE(ABORT, 'generation weave command does not match its run and receipt');
END;

CREATE TRIGGER generation_weave_commands_are_immutable_update
BEFORE UPDATE ON generation_weave_commands
BEGIN
    SELECT RAISE(ABORT, 'generation weave commands are immutable');
END;

CREATE TRIGGER generation_weave_commands_are_immutable_delete
BEFORE DELETE ON generation_weave_commands
BEGIN
    SELECT RAISE(ABORT, 'generation weave commands are immutable');
END;

CREATE TABLE document_deletions (
    command_id TEXT PRIMARY KEY CHECK (
        length(command_id) = 26
        AND command_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(command_id, 1, 1) BETWEEN '0' AND '7'
    ),
    document_id TEXT NOT NULL UNIQUE REFERENCES documents(document_id) ON DELETE RESTRICT,
    revision_id TEXT NOT NULL REFERENCES revisions(revision_id) ON DELETE RESTRICT,
    blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    relative_path TEXT NOT NULL,
    recovery_file_name TEXT NOT NULL UNIQUE CHECK (
        length(CAST(recovery_file_name AS BLOB)) BETWEEN 1 AND 255
        AND recovery_file_name NOT GLOB '*[/\\]*'
    ),
    deleted_at_ms INTEGER NOT NULL CHECK (deleted_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER document_deletions_validate_insert
BEFORE INSERT ON document_deletions
WHEN NOT EXISTS (
    SELECT 1
    FROM documents document
    JOIN revisions revision ON revision.revision_id = NEW.revision_id
    JOIN artifacts artifact ON artifact.artifact_id = revision.artifact_id
    WHERE document.document_id = NEW.document_id
      AND document.relative_path = NEW.relative_path
      AND revision.document_id = document.document_id
      AND artifact.blob_id = NEW.blob_id
      AND revision.revision_id = (
          SELECT active.revision_id
          FROM revisions active
          WHERE active.document_id = document.document_id
          ORDER BY active.created_at_ms DESC, active.revision_id DESC
          LIMIT 1
      )
)
BEGIN
    SELECT RAISE(ABORT, 'document deletion does not match the active catalogue identity');
END;

CREATE TRIGGER document_deletions_are_immutable_update
BEFORE UPDATE ON document_deletions
BEGIN
    SELECT RAISE(ABORT, 'document deletions are immutable');
END;

CREATE TRIGGER document_deletions_are_immutable_delete
BEFORE DELETE ON document_deletions
BEGIN
    SELECT RAISE(ABORT, 'document deletions are immutable');
END;

CREATE TABLE document_delete_operations (
    command_id TEXT PRIMARY KEY CHECK (
        length(command_id) = 26
        AND command_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(command_id, 1, 1) BETWEEN '0' AND '7'
    ),
    document_id TEXT NOT NULL REFERENCES documents(document_id) ON DELETE RESTRICT,
    revision_id TEXT NOT NULL REFERENCES revisions(revision_id) ON DELETE RESTRICT,
    blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    relative_path TEXT NOT NULL,
    recovery_file_name TEXT NOT NULL UNIQUE CHECK (
        length(CAST(recovery_file_name AS BLOB)) BETWEEN 1 AND 255
        AND recovery_file_name NOT GLOB '*[/\\]*'
    ),
    state TEXT NOT NULL CHECK (state IN ('prepared', 'captured', 'committed', 'aborted')),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    captured_at_ms INTEGER,
    committed_at_ms INTEGER,
    finished_at_ms INTEGER,
    CHECK (
        (state = 'prepared' AND captured_at_ms IS NULL AND committed_at_ms IS NULL AND finished_at_ms IS NULL)
        OR (state = 'captured' AND captured_at_ms IS NOT NULL AND committed_at_ms IS NULL AND finished_at_ms IS NULL)
        OR (state = 'committed' AND captured_at_ms IS NOT NULL AND committed_at_ms IS NOT NULL AND finished_at_ms IS NOT NULL)
        OR (state = 'aborted' AND committed_at_ms IS NULL AND finished_at_ms IS NOT NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX document_delete_one_live_operation
ON document_delete_operations(document_id)
WHERE state IN ('prepared', 'captured');

CREATE TRIGGER document_delete_operations_validate_insert
BEFORE INSERT ON document_delete_operations
WHEN NEW.state <> 'prepared' OR NOT EXISTS (
    SELECT 1
    FROM documents document
    JOIN revisions revision ON revision.revision_id = NEW.revision_id
    JOIN artifacts artifact ON artifact.artifact_id = revision.artifact_id
    WHERE document.document_id = NEW.document_id
      AND document.relative_path = NEW.relative_path
      AND revision.document_id = document.document_id
      AND artifact.blob_id = NEW.blob_id
      AND NOT EXISTS (
          SELECT 1 FROM document_deletions deletion
          WHERE deletion.document_id = document.document_id
      )
)
BEGIN
    SELECT RAISE(ABORT, 'document delete preparation lacks active catalogue authority');
END;

CREATE TRIGGER document_delete_operations_state_machine
BEFORE UPDATE ON document_delete_operations
WHEN NEW.command_id <> OLD.command_id
  OR NEW.document_id <> OLD.document_id
  OR NEW.revision_id <> OLD.revision_id
  OR NEW.blob_id <> OLD.blob_id
  OR NEW.relative_path <> OLD.relative_path
  OR NEW.recovery_file_name <> OLD.recovery_file_name
  OR NEW.created_at_ms <> OLD.created_at_ms
  OR NOT (
      (OLD.state = 'prepared' AND NEW.state = 'captured'
       AND NEW.captured_at_ms IS NOT NULL
       AND NEW.committed_at_ms IS NULL AND NEW.finished_at_ms IS NULL)
      OR (OLD.state = 'prepared' AND NEW.state = 'aborted'
          AND NEW.captured_at_ms IS NULL
          AND NEW.committed_at_ms IS NULL AND NEW.finished_at_ms IS NOT NULL)
      OR (OLD.state = 'captured' AND NEW.state = 'committed'
          AND NEW.captured_at_ms = OLD.captured_at_ms
          AND NEW.committed_at_ms IS NOT NULL AND NEW.finished_at_ms IS NOT NULL)
      OR (OLD.state = 'prepared' AND NEW.state = 'committed'
          AND NEW.captured_at_ms IS NOT NULL
          AND NEW.committed_at_ms IS NOT NULL AND NEW.finished_at_ms IS NOT NULL)
      OR (OLD.state = 'captured' AND NEW.state = 'aborted'
          AND NEW.captured_at_ms = OLD.captured_at_ms
          AND NEW.committed_at_ms IS NULL AND NEW.finished_at_ms IS NOT NULL)
      OR (OLD.state = 'aborted' AND NEW.state = 'prepared'
          AND NEW.captured_at_ms IS NULL
          AND NEW.committed_at_ms IS NULL AND NEW.finished_at_ms IS NULL)
  )
BEGIN
    SELECT RAISE(ABORT, 'invalid document delete lifecycle transition');
END;

CREATE TRIGGER document_delete_operations_are_immutable_delete
BEFORE DELETE ON document_delete_operations
BEGIN
    SELECT RAISE(ABORT, 'document delete operations are durable');
END;

CREATE TABLE document_rename_operations (
    operation_id TEXT PRIMARY KEY CHECK (
        length(operation_id) = 26
        AND operation_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(operation_id, 1, 1) BETWEEN '0' AND '7'
    ),
    document_id TEXT NOT NULL REFERENCES documents(document_id) ON DELETE RESTRICT,
    revision_id TEXT NOT NULL REFERENCES revisions(revision_id) ON DELETE RESTRICT,
    blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    source_relative_path TEXT NOT NULL,
    target_relative_path TEXT NOT NULL,
    target_display_title TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('prepared', 'captured', 'committed', 'aborted')),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    captured_at_ms INTEGER,
    committed_at_ms INTEGER,
    finished_at_ms INTEGER,
    CHECK (
        (state = 'prepared' AND captured_at_ms IS NULL AND committed_at_ms IS NULL AND finished_at_ms IS NULL)
        OR (state = 'captured' AND captured_at_ms IS NOT NULL AND committed_at_ms IS NULL AND finished_at_ms IS NULL)
        OR (state = 'committed' AND captured_at_ms IS NOT NULL AND committed_at_ms IS NOT NULL AND finished_at_ms IS NOT NULL)
        OR (state = 'aborted' AND committed_at_ms IS NULL AND finished_at_ms IS NOT NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX document_rename_one_live_operation
ON document_rename_operations(document_id)
WHERE state IN ('prepared', 'captured');

CREATE INDEX document_rename_reserved_source_path
ON document_rename_operations(source_relative_path)
WHERE state IN ('prepared', 'captured');

CREATE INDEX document_rename_reserved_target_path
ON document_rename_operations(target_relative_path)
WHERE state IN ('prepared', 'captured');

CREATE TRIGGER document_rename_operations_validate_insert
BEFORE INSERT ON document_rename_operations
WHEN NEW.state <> 'prepared' OR NOT EXISTS (
    SELECT 1
    FROM documents document
    JOIN revisions revision ON revision.revision_id = NEW.revision_id
    JOIN artifacts artifact ON artifact.artifact_id = revision.artifact_id
    WHERE document.document_id = NEW.document_id
      AND document.relative_path = NEW.source_relative_path
      AND revision.document_id = document.document_id
      AND artifact.blob_id = NEW.blob_id
      AND NOT EXISTS (
          SELECT 1 FROM document_deletions deletion
          WHERE deletion.document_id = document.document_id
      )
)
BEGIN
    SELECT RAISE(ABORT, 'document rename preparation lacks active catalogue authority');
END;

CREATE TRIGGER document_rename_operations_state_machine
BEFORE UPDATE ON document_rename_operations
WHEN NEW.operation_id <> OLD.operation_id
  OR NEW.document_id <> OLD.document_id
  OR NEW.revision_id <> OLD.revision_id
  OR NEW.blob_id <> OLD.blob_id
  OR NEW.source_relative_path <> OLD.source_relative_path
  OR NEW.target_relative_path <> OLD.target_relative_path
  OR NEW.target_display_title <> OLD.target_display_title
  OR NEW.created_at_ms <> OLD.created_at_ms
  OR NOT (
      (OLD.state = 'prepared' AND NEW.state = 'captured'
       AND NEW.captured_at_ms IS NOT NULL
       AND NEW.committed_at_ms IS NULL AND NEW.finished_at_ms IS NULL)
      OR (OLD.state = 'prepared' AND NEW.state = 'aborted'
          AND NEW.captured_at_ms IS NULL
          AND NEW.committed_at_ms IS NULL AND NEW.finished_at_ms IS NOT NULL)
      OR (OLD.state = 'captured' AND NEW.state = 'committed'
          AND NEW.captured_at_ms = OLD.captured_at_ms
          AND NEW.committed_at_ms IS NOT NULL AND NEW.finished_at_ms IS NOT NULL)
      OR (OLD.state = 'captured' AND NEW.state = 'aborted'
          AND NEW.captured_at_ms = OLD.captured_at_ms
          AND NEW.committed_at_ms IS NULL AND NEW.finished_at_ms IS NOT NULL)
  )
BEGIN
    SELECT RAISE(ABORT, 'invalid document rename lifecycle transition');
END;

CREATE TRIGGER document_rename_operations_are_immutable_delete
BEFORE DELETE ON document_rename_operations
BEGIN
    SELECT RAISE(ABORT, 'document rename operations are durable');
END;
