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

-- Older runs did not persist bounded shelf metadata. Their immutable run and
-- environment artifacts remain authoritative and can still be loaded through
-- an explicit single-run lookup. The insertion order below gives every
-- existing run a deterministic cursor position without rewriting evidence.
INSERT INTO generation_run_index(run_id, seed_decimal, model_identifier)
SELECT run_id, NULL, NULL
FROM generation_runs
ORDER BY created_at_ms ASC, run_id ASC;

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
