-- A focused renderer command is evidence that the trusted application host
-- accepted one exact command. It is not evidence of physical user presence.
-- The one-use nonce and move-only authority are deliberately absent here.
CREATE TABLE research_foreground_command_receipts (
    command_id TEXT PRIMARY KEY
        REFERENCES research_promotion_command_requests(command_id) ON DELETE RESTRICT,
    command_request_fingerprint TEXT NOT NULL CHECK (
        length(command_request_fingerprint) = 64
        AND command_request_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    process_session_fingerprint TEXT NOT NULL CHECK (
        length(process_session_fingerprint) = 64
        AND process_session_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    application_session_id TEXT NOT NULL CHECK (
        length(application_session_id) = 26
        AND application_session_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(application_session_id, 1, 1) BETWEEN '0' AND '7'
    ),
    window_id TEXT NOT NULL CHECK (length(CAST(window_id AS BLOB)) BETWEEN 1 AND 128),
    document_id TEXT NOT NULL REFERENCES documents(document_id) ON DELETE RESTRICT,
    promotion_subject_id TEXT NOT NULL CHECK (
        length(promotion_subject_id) = 26
        AND promotion_subject_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(promotion_subject_id, 1, 1) BETWEEN '0' AND '7'
    ),
    candidate_fingerprint TEXT NOT NULL CHECK (
        length(candidate_fingerprint) = 64
        AND candidate_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    focus_epoch INTEGER NOT NULL CHECK (focus_epoch > 0),
    monotonic_event_index INTEGER NOT NULL CHECK (monotonic_event_index > 0),
    issued_at_ms INTEGER NOT NULL CHECK (issued_at_ms > 0),
    expires_at_ms INTEGER NOT NULL CHECK (expires_at_ms >= issued_at_ms),
    occurred_at_ms INTEGER NOT NULL CHECK (
        occurred_at_ms >= issued_at_ms AND occurred_at_ms <= expires_at_ms
    ),
    receipt_blob_id TEXT NOT NULL UNIQUE REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    recorded_at_ms INTEGER NOT NULL CHECK (recorded_at_ms >= occurred_at_ms),
    UNIQUE (process_session_fingerprint, monotonic_event_index)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_foreground_command_receipts_validate_insert
BEFORE INSERT ON research_foreground_command_receipts
WHEN NOT EXISTS (
    SELECT 1
    FROM research_promotion_command_requests request
    JOIN revisions source ON source.revision_id = request.source_revision_id
    JOIN blobs receipt ON receipt.blob_id = NEW.receipt_blob_id
    WHERE request.command_id = NEW.command_id
      AND request.command_request_fingerprint = NEW.command_request_fingerprint
      AND request.subject_id = NEW.promotion_subject_id
      AND request.recorded_at_ms <= NEW.issued_at_ms
      AND source.document_id = NEW.document_id
      AND receipt.byte_len > 0
)
BEGIN
    SELECT RAISE(ABORT, 'foreground command receipt lacks its exact pending promotion');
END;

CREATE TRIGGER research_foreground_command_receipts_immutable_update
BEFORE UPDATE ON research_foreground_command_receipts
BEGIN
    SELECT RAISE(ABORT, 'foreground command receipts are immutable');
END;

CREATE TRIGGER research_foreground_command_receipts_immutable_delete
BEFORE DELETE ON research_foreground_command_receipts
BEGIN
    SELECT RAISE(ABORT, 'foreground command receipts are immutable');
END;
