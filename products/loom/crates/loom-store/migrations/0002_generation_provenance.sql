CREATE TABLE IF NOT EXISTS model_environments (
    artifact_id TEXT PRIMARY KEY REFERENCES artifacts(artifact_id),
    environment_id TEXT NOT NULL UNIQUE,
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS prompt_recipes (
    artifact_id TEXT PRIMARY KEY REFERENCES artifacts(artifact_id),
    exact_prompt_blob_id TEXT NOT NULL REFERENCES blobs(blob_id),
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS prompt_recipe_inputs (
    recipe_artifact_id TEXT NOT NULL REFERENCES prompt_recipes(artifact_id),
    position INTEGER NOT NULL CHECK (position >= 0),
    input_artifact_id TEXT NOT NULL REFERENCES artifacts(artifact_id),
    PRIMARY KEY (recipe_artifact_id, position)
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS context_recipes (
    artifact_id TEXT PRIMARY KEY REFERENCES artifacts(artifact_id),
    source_revision_id TEXT NOT NULL REFERENCES revisions(revision_id),
    retrieval_evidence_blob_id TEXT REFERENCES blobs(blob_id),
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS context_recipe_sources (
    recipe_artifact_id TEXT NOT NULL REFERENCES context_recipes(artifact_id),
    position INTEGER NOT NULL CHECK (position >= 0),
    source_artifact_id TEXT NOT NULL REFERENCES artifacts(artifact_id),
    PRIMARY KEY (recipe_artifact_id, position)
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS authority_policies (
    artifact_id TEXT PRIMARY KEY REFERENCES artifacts(artifact_id),
    policy_version INTEGER NOT NULL CHECK (policy_version > 0),
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS authority_policy_members (
    policy_artifact_id TEXT NOT NULL REFERENCES authority_policies(artifact_id),
    environment_artifact_id TEXT NOT NULL REFERENCES model_environments(artifact_id),
    role TEXT NOT NULL CHECK (role IN ('writer', 'critic')),
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (policy_artifact_id, role, position),
    UNIQUE (policy_artifact_id, environment_artifact_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS branches (
    branch_id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES documents(document_id),
    source_revision_id TEXT NOT NULL REFERENCES revisions(revision_id),
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS generation_runs (
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

CREATE TABLE IF NOT EXISTS generation_events (
    event_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES generation_runs(run_id),
    sequence INTEGER NOT NULL CHECK (sequence >= 0),
    event_kind TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    is_terminal INTEGER NOT NULL CHECK (is_terminal IN (0, 1)),
    created_at_ms INTEGER NOT NULL,
    UNIQUE (run_id, sequence)
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX IF NOT EXISTS one_generation_terminal_event
ON generation_events(run_id) WHERE is_terminal = 1;

CREATE TABLE IF NOT EXISTS generation_candidates (
    candidate_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL UNIQUE REFERENCES generation_runs(run_id),
    generated_span_artifact_id TEXT NOT NULL UNIQUE REFERENCES artifacts(artifact_id),
    token_trace_artifact_id TEXT NOT NULL UNIQUE REFERENCES artifacts(artifact_id),
    output_blob_id TEXT NOT NULL REFERENCES blobs(blob_id),
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS generation_terminals (
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

CREATE TABLE IF NOT EXISTS selection_events (
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

CREATE TABLE IF NOT EXISTS authorship_attestations (
    attestation_artifact_id TEXT PRIMARY KEY REFERENCES artifacts(artifact_id),
    candidate_id TEXT NOT NULL REFERENCES generation_candidates(candidate_id),
    generated_span_artifact_id TEXT NOT NULL REFERENCES artifacts(artifact_id),
    promoted_revision_id TEXT NOT NULL UNIQUE REFERENCES revisions(revision_id),
    promotion_command_id TEXT NOT NULL UNIQUE REFERENCES command_receipts(command_id),
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS command_requests (
    command_id TEXT PRIMARY KEY REFERENCES command_receipts(command_id),
    request_fingerprint TEXT NOT NULL,
    command_kind TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TRIGGER IF NOT EXISTS generation_events_stop_after_terminal
BEFORE INSERT ON generation_events
WHEN EXISTS (
    SELECT 1 FROM generation_events
    WHERE run_id = NEW.run_id AND is_terminal = 1
)
BEGIN
    SELECT RAISE(ABORT, 'generation already has a terminal event');
END;

CREATE TRIGGER IF NOT EXISTS model_environments_are_immutable_update
BEFORE UPDATE ON model_environments BEGIN SELECT RAISE(ABORT, 'model environments are immutable'); END;
CREATE TRIGGER IF NOT EXISTS model_environments_are_immutable_delete
BEFORE DELETE ON model_environments BEGIN SELECT RAISE(ABORT, 'model environments are immutable'); END;
CREATE TRIGGER IF NOT EXISTS prompt_recipes_are_immutable_update
BEFORE UPDATE ON prompt_recipes BEGIN SELECT RAISE(ABORT, 'prompt recipes are immutable'); END;
CREATE TRIGGER IF NOT EXISTS prompt_recipes_are_immutable_delete
BEFORE DELETE ON prompt_recipes BEGIN SELECT RAISE(ABORT, 'prompt recipes are immutable'); END;
CREATE TRIGGER IF NOT EXISTS prompt_recipe_inputs_are_immutable_update
BEFORE UPDATE ON prompt_recipe_inputs BEGIN SELECT RAISE(ABORT, 'prompt recipe inputs are immutable'); END;
CREATE TRIGGER IF NOT EXISTS prompt_recipe_inputs_are_immutable_delete
BEFORE DELETE ON prompt_recipe_inputs BEGIN SELECT RAISE(ABORT, 'prompt recipe inputs are immutable'); END;
CREATE TRIGGER IF NOT EXISTS context_recipes_are_immutable_update
BEFORE UPDATE ON context_recipes BEGIN SELECT RAISE(ABORT, 'context recipes are immutable'); END;
CREATE TRIGGER IF NOT EXISTS context_recipes_are_immutable_delete
BEFORE DELETE ON context_recipes BEGIN SELECT RAISE(ABORT, 'context recipes are immutable'); END;
CREATE TRIGGER IF NOT EXISTS context_recipe_sources_are_immutable_update
BEFORE UPDATE ON context_recipe_sources BEGIN SELECT RAISE(ABORT, 'context recipe sources are immutable'); END;
CREATE TRIGGER IF NOT EXISTS context_recipe_sources_are_immutable_delete
BEFORE DELETE ON context_recipe_sources BEGIN SELECT RAISE(ABORT, 'context recipe sources are immutable'); END;
CREATE TRIGGER IF NOT EXISTS authority_policies_are_immutable_update
BEFORE UPDATE ON authority_policies BEGIN SELECT RAISE(ABORT, 'authority policies are immutable'); END;
CREATE TRIGGER IF NOT EXISTS authority_policies_are_immutable_delete
BEFORE DELETE ON authority_policies BEGIN SELECT RAISE(ABORT, 'authority policies are immutable'); END;
CREATE TRIGGER IF NOT EXISTS authority_policy_members_are_immutable_update
BEFORE UPDATE ON authority_policy_members BEGIN SELECT RAISE(ABORT, 'authority policy members are immutable'); END;
CREATE TRIGGER IF NOT EXISTS authority_policy_members_are_immutable_delete
BEFORE DELETE ON authority_policy_members BEGIN SELECT RAISE(ABORT, 'authority policy members are immutable'); END;
CREATE TRIGGER IF NOT EXISTS branches_are_immutable_update
BEFORE UPDATE ON branches BEGIN SELECT RAISE(ABORT, 'branches are immutable'); END;
CREATE TRIGGER IF NOT EXISTS branches_are_immutable_delete
BEFORE DELETE ON branches BEGIN SELECT RAISE(ABORT, 'branches are immutable'); END;
CREATE TRIGGER IF NOT EXISTS generation_runs_are_immutable_update
BEFORE UPDATE ON generation_runs BEGIN SELECT RAISE(ABORT, 'generation runs are immutable'); END;
CREATE TRIGGER IF NOT EXISTS generation_runs_are_immutable_delete
BEFORE DELETE ON generation_runs BEGIN SELECT RAISE(ABORT, 'generation runs are immutable'); END;
CREATE TRIGGER IF NOT EXISTS generation_events_are_immutable_update
BEFORE UPDATE ON generation_events BEGIN SELECT RAISE(ABORT, 'generation events are immutable'); END;
CREATE TRIGGER IF NOT EXISTS generation_events_are_immutable_delete
BEFORE DELETE ON generation_events BEGIN SELECT RAISE(ABORT, 'generation events are immutable'); END;
CREATE TRIGGER IF NOT EXISTS generation_candidates_are_immutable_update
BEFORE UPDATE ON generation_candidates BEGIN SELECT RAISE(ABORT, 'generation candidates are immutable'); END;
CREATE TRIGGER IF NOT EXISTS generation_candidates_are_immutable_delete
BEFORE DELETE ON generation_candidates BEGIN SELECT RAISE(ABORT, 'generation candidates are immutable'); END;
CREATE TRIGGER IF NOT EXISTS generation_terminals_are_immutable_update
BEFORE UPDATE ON generation_terminals BEGIN SELECT RAISE(ABORT, 'generation terminals are immutable'); END;
CREATE TRIGGER IF NOT EXISTS generation_terminals_are_immutable_delete
BEFORE DELETE ON generation_terminals BEGIN SELECT RAISE(ABORT, 'generation terminals are immutable'); END;
CREATE TRIGGER IF NOT EXISTS selection_events_are_immutable_update
BEFORE UPDATE ON selection_events BEGIN SELECT RAISE(ABORT, 'selection events are immutable'); END;
CREATE TRIGGER IF NOT EXISTS selection_events_are_immutable_delete
BEFORE DELETE ON selection_events BEGIN SELECT RAISE(ABORT, 'selection events are immutable'); END;
CREATE TRIGGER IF NOT EXISTS authorship_attestations_are_immutable_update
BEFORE UPDATE ON authorship_attestations BEGIN SELECT RAISE(ABORT, 'authorship attestations are immutable'); END;
CREATE TRIGGER IF NOT EXISTS authorship_attestations_are_immutable_delete
BEFORE DELETE ON authorship_attestations BEGIN SELECT RAISE(ABORT, 'authorship attestations are immutable'); END;
CREATE TRIGGER IF NOT EXISTS command_requests_are_immutable_update
BEFORE UPDATE ON command_requests BEGIN SELECT RAISE(ABORT, 'command requests are immutable'); END;
CREATE TRIGGER IF NOT EXISTS command_requests_are_immutable_delete
BEFORE DELETE ON command_requests BEGIN SELECT RAISE(ABORT, 'command requests are immutable'); END;
