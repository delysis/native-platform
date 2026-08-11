CREATE TABLE research_token_piece_evidence (
    call_id TEXT PRIMARY KEY REFERENCES research_model_calls(call_id) ON DELETE RESTRICT,
    raw_piece_bytes_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    raw_piece_byte_len INTEGER NOT NULL CHECK (raw_piece_byte_len >= 0),
    boundary_vector_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    boundary_count INTEGER NOT NULL CHECK (boundary_count >= 1),
    token_piece_fingerprint TEXT NOT NULL CHECK (
        length(token_piece_fingerprint) = 64
        AND token_piece_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_token_piece_evidence_validate_insert
BEFORE INSERT ON research_token_piece_evidence
WHEN NOT EXISTS (
        SELECT 1 FROM blobs raw
        WHERE raw.blob_id = NEW.raw_piece_bytes_blob_id
          AND raw.byte_len = NEW.raw_piece_byte_len
    )
    OR NOT EXISTS (
        SELECT 1 FROM blobs boundaries
        WHERE boundaries.blob_id = NEW.boundary_vector_blob_id
          AND boundaries.byte_len = NEW.boundary_count * 8
    )
BEGIN
    SELECT RAISE(ABORT, 'token-piece evidence blobs are absent or inconsistent');
END;

CREATE TRIGGER research_token_piece_evidence_immutable_update
BEFORE UPDATE ON research_token_piece_evidence
BEGIN SELECT RAISE(ABORT, 'token-piece evidence is immutable'); END;

CREATE TRIGGER research_token_piece_evidence_immutable_delete
BEFORE DELETE ON research_token_piece_evidence
BEGIN SELECT RAISE(ABORT, 'token-piece evidence is immutable'); END;
