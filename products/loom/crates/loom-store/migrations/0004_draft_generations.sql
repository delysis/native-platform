ALTER TABLE transient_drafts
    ADD COLUMN base_version INTEGER NOT NULL DEFAULT 0
        CHECK (base_version >= 0 AND base_version < draft_version);

UPDATE transient_drafts
SET base_version = draft_version - 1;

CREATE TABLE transient_draft_sequences (
    document_id TEXT PRIMARY KEY NOT NULL
        REFERENCES documents(document_id) ON DELETE RESTRICT,
    last_version INTEGER NOT NULL CHECK (last_version > 0)
) STRICT, WITHOUT ROWID;

INSERT INTO transient_draft_sequences(document_id, last_version)
SELECT document_id, draft_version
FROM transient_drafts;

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
