CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS blobs (
    blob_id TEXT PRIMARY KEY,
    byte_len INTEGER NOT NULL CHECK (byte_len >= 0),
    media_type TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS artifacts (
    artifact_id TEXT PRIMARY KEY,
    blob_id TEXT NOT NULL REFERENCES blobs(blob_id),
    artifact_kind TEXT NOT NULL,
    media_type TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS operations (
    operation_id TEXT PRIMARY KEY,
    operation_kind TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS operation_inputs (
    operation_id TEXT NOT NULL REFERENCES operations(operation_id),
    position INTEGER NOT NULL CHECK (position >= 0),
    artifact_id TEXT NOT NULL REFERENCES artifacts(artifact_id),
    PRIMARY KEY (operation_id, position)
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS operation_outputs (
    operation_id TEXT NOT NULL REFERENCES operations(operation_id),
    position INTEGER NOT NULL CHECK (position >= 0),
    artifact_id TEXT NOT NULL UNIQUE REFERENCES artifacts(artifact_id),
    PRIMARY KEY (operation_id, position)
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS documents (
    document_id TEXT PRIMARY KEY,
    relative_path TEXT NOT NULL UNIQUE,
    document_kind TEXT NOT NULL CHECK (document_kind IN ('prose', 'verse', 'hybrid')),
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS revisions (
    revision_id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES documents(document_id),
    parent_revision_id TEXT REFERENCES revisions(revision_id),
    artifact_id TEXT NOT NULL UNIQUE REFERENCES artifacts(artifact_id),
    reason TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS revision_segments (
    revision_id TEXT NOT NULL REFERENCES revisions(revision_id),
    position INTEGER NOT NULL CHECK (position >= 0),
    artifact_id TEXT NOT NULL REFERENCES artifacts(artifact_id),
    start_byte INTEGER NOT NULL CHECK (start_byte >= 0),
    end_byte INTEGER NOT NULL CHECK (end_byte >= start_byte),
    contribution_kind TEXT NOT NULL CHECK (contribution_kind IN ('human', 'generated', 'mixed', 'source')),
    PRIMARY KEY (revision_id, position)
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS visible_file_outbox (
    outbox_id INTEGER PRIMARY KEY,
    revision_id TEXT NOT NULL UNIQUE REFERENCES revisions(revision_id),
    relative_path TEXT NOT NULL,
    target_blob_id TEXT NOT NULL REFERENCES blobs(blob_id),
    expected_visible_blob_id TEXT,
    state TEXT NOT NULL CHECK (state IN ('pending', 'completed')),
    created_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER
) STRICT;

CREATE TABLE IF NOT EXISTS command_receipts (
    command_id TEXT PRIMARY KEY,
    command_kind TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    completed_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS revisions_by_document
ON revisions(document_id, created_at_ms DESC, revision_id DESC);

CREATE INDEX IF NOT EXISTS pending_visible_file_outbox
ON visible_file_outbox(state, outbox_id);

CREATE TRIGGER IF NOT EXISTS blobs_are_immutable_update
BEFORE UPDATE ON blobs BEGIN
    SELECT RAISE(ABORT, 'blobs are immutable');
END;
CREATE TRIGGER IF NOT EXISTS blobs_are_immutable_delete
BEFORE DELETE ON blobs BEGIN
    SELECT RAISE(ABORT, 'blobs are immutable');
END;

CREATE TRIGGER IF NOT EXISTS artifacts_are_immutable_update
BEFORE UPDATE ON artifacts BEGIN
    SELECT RAISE(ABORT, 'artifacts are immutable');
END;
CREATE TRIGGER IF NOT EXISTS artifacts_are_immutable_delete
BEFORE DELETE ON artifacts BEGIN
    SELECT RAISE(ABORT, 'artifacts are immutable');
END;

CREATE TRIGGER IF NOT EXISTS operations_are_immutable_update
BEFORE UPDATE ON operations BEGIN
    SELECT RAISE(ABORT, 'operations are immutable');
END;
CREATE TRIGGER IF NOT EXISTS operations_are_immutable_delete
BEFORE DELETE ON operations BEGIN
    SELECT RAISE(ABORT, 'operations are immutable');
END;

CREATE TRIGGER IF NOT EXISTS operation_inputs_are_immutable_update
BEFORE UPDATE ON operation_inputs BEGIN
    SELECT RAISE(ABORT, 'operation inputs are immutable');
END;
CREATE TRIGGER IF NOT EXISTS operation_inputs_are_immutable_delete
BEFORE DELETE ON operation_inputs BEGIN
    SELECT RAISE(ABORT, 'operation inputs are immutable');
END;

CREATE TRIGGER IF NOT EXISTS operation_outputs_are_immutable_update
BEFORE UPDATE ON operation_outputs BEGIN
    SELECT RAISE(ABORT, 'operation outputs are immutable');
END;
CREATE TRIGGER IF NOT EXISTS operation_outputs_are_immutable_delete
BEFORE DELETE ON operation_outputs BEGIN
    SELECT RAISE(ABORT, 'operation outputs are immutable');
END;

CREATE TRIGGER IF NOT EXISTS revisions_are_immutable_update
BEFORE UPDATE ON revisions BEGIN
    SELECT RAISE(ABORT, 'revisions are immutable');
END;
CREATE TRIGGER IF NOT EXISTS revisions_are_immutable_delete
BEFORE DELETE ON revisions BEGIN
    SELECT RAISE(ABORT, 'revisions are immutable');
END;

CREATE TRIGGER IF NOT EXISTS revision_segments_are_immutable_update
BEFORE UPDATE ON revision_segments BEGIN
    SELECT RAISE(ABORT, 'revision segments are immutable');
END;
CREATE TRIGGER IF NOT EXISTS revision_segments_are_immutable_delete
BEFORE DELETE ON revision_segments BEGIN
    SELECT RAISE(ABORT, 'revision segments are immutable');
END;

CREATE TRIGGER IF NOT EXISTS command_receipts_are_immutable_update
BEFORE UPDATE ON command_receipts BEGIN
    SELECT RAISE(ABORT, 'command receipts are immutable');
END;
CREATE TRIGGER IF NOT EXISTS command_receipts_are_immutable_delete
BEFORE DELETE ON command_receipts BEGIN
    SELECT RAISE(ABORT, 'command receipts are immutable');
END;
