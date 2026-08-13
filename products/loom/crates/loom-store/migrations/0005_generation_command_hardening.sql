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
