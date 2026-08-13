CREATE TABLE transient_drafts (
    document_id TEXT PRIMARY KEY NOT NULL
        REFERENCES documents(document_id) ON DELETE RESTRICT,
    source_revision_id TEXT NOT NULL
        REFERENCES revisions(revision_id) ON DELETE RESTRICT,
    draft_blob_id TEXT NOT NULL CHECK (length(draft_blob_id) = 64),
    storage_slot INTEGER NOT NULL CHECK (storage_slot IN (0, 1)),
    draft_version INTEGER NOT NULL CHECK (draft_version > 0),
    updated_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

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
