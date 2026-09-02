-- Document deletion is an explicit catalogue fact, not an inference from a
-- temporarily missing ordinary file. The receipt is immutable so a renderer
-- retry remains exact after restart even when the recovery copy is lost.
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

-- A renderer command becomes native durable intent before any visible name is
-- captured. Reopen can therefore finish an exact private capture without the
-- renderer remembering its command ID, or abort an intent that never captured.
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

-- Rename first installs and durably flushes the new no-clobber path. The
-- catalogue update and `committed` transition are one SQLite transaction.
-- The old name is first captured into private state, then moved no-clobber to
-- the new name. The catalogue update is the final fallible step.
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

-- A prepared operation temporarily reserves both endpoints until recovery.
-- Committed source spellings are ordinary free filesystem names again; stable
-- document/revision IDs, not permanent pathname claims, disambiguate history.
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
