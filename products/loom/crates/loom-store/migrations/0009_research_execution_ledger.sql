-- Durable research execution state.  These tables are evidence, not runtime
-- authority: reopening a project can replay and verify their records, but no
-- persisted row recreates an unpersisted trial, evaluator, archive, or
-- benchmark capability.

-- All record fingerprints in this migration are lowercase SHA-256 text.  A
-- record blob is the exact canonical serialized witness used by the Rust
-- verifier; empty or missing witnesses are rejected once, in this registry.
CREATE TABLE research_execution_records (
    record_fingerprint TEXT PRIMARY KEY CHECK (
        length(record_fingerprint) = 64
        AND record_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    record_kind TEXT NOT NULL CHECK (record_kind IN (
        'campaign', 'trial_spec', 'trial_run', 'stage_spec', 'campaign_trial_attempt',
        'stage_attempt', 'campaign_event', 'trial_event',
        'budget_reservation', 'budget_charge', 'search_decision',
        'story_graph', 'story_state', 'prompt_mask',
        'backtranslation_proposal', 'backtranslation_audition',
        'backtranslation_acceptance', 'evaluation_task', 'evaluation_receipt',
        'pairwise_assignment', 'score_vector', 'candidate_descriptor',
        'preference_label', 'archive_snapshot', 'benchmark_suite',
        'benchmark_seal', 'benchmark_run', 'benchmark_journal', 'human_label_packet',
        'benchmark_result'
    )),
    record_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_execution_records_validate_insert
BEFORE INSERT ON research_execution_records
WHEN NEW.record_blob_id != NEW.record_fingerprint
    OR NOT EXISTS (
        SELECT 1 FROM blobs blob
        WHERE blob.blob_id = NEW.record_blob_id
          AND blob.byte_len > 0
    )
BEGIN
    SELECT RAISE(ABORT, 'research execution record is absent, empty, or not content-addressed');
END;

-- Campaign and stage execution ------------------------------------------------

CREATE TABLE research_campaigns (
    campaign_id TEXT PRIMARY KEY CHECK (
        length(campaign_id) = 26
        AND campaign_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(campaign_id, 1, 1) BETWEEN '0' AND '7'
    ),
    campaign_fingerprint TEXT NOT NULL UNIQUE CHECK (
        length(campaign_fingerprint) = 64
        AND campaign_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    project_id TEXT NOT NULL CHECK (
        length(project_id) = 26
        AND project_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(project_id, 1, 1) BETWEEN '0' AND '7'
    ),
    manifest_source_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    manifest_fingerprint TEXT NOT NULL CHECK (
        length(manifest_fingerprint) = 64
        AND manifest_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    project_input_fingerprint TEXT NOT NULL CHECK (
        length(project_input_fingerprint) = 64
        AND project_input_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    seed_decimal TEXT NOT NULL CHECK (
        length(seed_decimal) BETWEEN 1 AND 20
        AND seed_decimal NOT GLOB '*[^0-9]*'
        AND (seed_decimal = '0' OR substr(seed_decimal, 1, 1) != '0')
        AND (length(seed_decimal) < 20 OR seed_decimal <= '18446744073709551615')
    ),
    maximum_writer_tokens INTEGER NOT NULL CHECK (maximum_writer_tokens > 0),
    maximum_controller_tokens INTEGER NOT NULL CHECK (maximum_controller_tokens >= 0),
    maximum_evaluations INTEGER NOT NULL CHECK (maximum_evaluations > 0),
    maximum_wall_time_ms INTEGER NOT NULL CHECK (maximum_wall_time_ms > 0),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_campaigns_validate_insert
BEFORE INSERT ON research_campaigns
WHEN NOT EXISTS (
    SELECT 1 FROM research_execution_records record
    WHERE record.record_fingerprint = NEW.record_fingerprint
      AND record.record_kind = 'campaign'
)
BEGIN
    SELECT RAISE(ABORT, 'campaign lacks an exact campaign record');
END;

CREATE TABLE research_trial_specs (
    trial_fingerprint TEXT PRIMARY KEY CHECK (
        length(trial_fingerprint) = 64
        AND trial_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    campaign_id TEXT NOT NULL REFERENCES research_campaigns(campaign_id) ON DELETE RESTRICT,
    trial_case_id TEXT NOT NULL CHECK (
        length(trial_case_id) = 26
        AND trial_case_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(trial_case_id, 1, 1) BETWEEN '0' AND '7'
    ),
    treatment_fingerprint TEXT NOT NULL CHECK (
        length(treatment_fingerprint) = 64
        AND treatment_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    prompt_content_fingerprint TEXT NOT NULL CHECK (
        length(prompt_content_fingerprint) = 64
        AND prompt_content_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    model_binding_fingerprint TEXT NOT NULL CHECK (
        length(model_binding_fingerprint) = 64
        AND model_binding_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    expected_writer_call_count INTEGER NOT NULL CHECK (
        expected_writer_call_count BETWEEN 1 AND 65535
    ),
    declared_writer_token_maximum INTEGER NOT NULL CHECK (declared_writer_token_maximum > 0),
    maximum_writer_tokens INTEGER NOT NULL CHECK (maximum_writer_tokens > 0),
    maximum_controller_tokens INTEGER NOT NULL CHECK (maximum_controller_tokens >= 0),
    maximum_evaluations INTEGER NOT NULL CHECK (maximum_evaluations > 0),
    maximum_wall_time_ms INTEGER NOT NULL CHECK (maximum_wall_time_ms > 0),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    UNIQUE (campaign_id, trial_case_id, treatment_fingerprint)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_trial_specs_validate_insert
BEFORE INSERT ON research_trial_specs
WHEN NOT EXISTS (
    SELECT 1 FROM research_execution_records record
    WHERE record.record_fingerprint = NEW.record_fingerprint
      AND record.record_kind = 'trial_spec'
)
BEGIN
    SELECT RAISE(ABORT, 'trial specification lacks an exact trial record');
END;

CREATE TABLE research_campaign_stage_specs (
    stage_id TEXT PRIMARY KEY CHECK (
        length(stage_id) = 26
        AND stage_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(stage_id, 1, 1) BETWEEN '0' AND '7'
    ),
    trial_fingerprint TEXT NOT NULL
        REFERENCES research_trial_specs(trial_fingerprint) ON DELETE RESTRICT,
    stage_ordinal INTEGER NOT NULL CHECK (stage_ordinal >= 0),
    stage_kind TEXT NOT NULL CHECK (stage_kind IN (
        'freeze_inputs', 'backtranslate_mask', 'plan', 'retrieve',
        'compile_prompt', 'generate', 'admit', 'assemble', 'gate',
        'evaluate', 'describe', 'archive'
    )),
    stage_spec_fingerprint TEXT NOT NULL UNIQUE CHECK (
        length(stage_spec_fingerprint) = 64
        AND stage_spec_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    maximum_writer_tokens INTEGER NOT NULL CHECK (maximum_writer_tokens >= 0),
    maximum_controller_tokens INTEGER NOT NULL CHECK (maximum_controller_tokens >= 0),
    maximum_evaluations INTEGER NOT NULL CHECK (maximum_evaluations >= 0),
    maximum_wall_time_ms INTEGER NOT NULL CHECK (maximum_wall_time_ms > 0),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    UNIQUE (trial_fingerprint, stage_ordinal)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_campaign_stage_specs_validate_insert
BEFORE INSERT ON research_campaign_stage_specs
WHEN NOT EXISTS (
    SELECT 1 FROM research_execution_records record
    WHERE record.record_fingerprint = NEW.record_fingerprint
      AND record.record_kind = 'stage_spec'
)
BEGIN
    SELECT RAISE(ABORT, 'stage specification lacks an exact stage record');
END;

CREATE TABLE research_campaign_stage_dependencies (
    stage_id TEXT NOT NULL REFERENCES research_campaign_stage_specs(stage_id) ON DELETE RESTRICT,
    dependency_stage_id TEXT NOT NULL REFERENCES research_campaign_stage_specs(stage_id) ON DELETE RESTRICT,
    dependency_ordinal INTEGER NOT NULL CHECK (dependency_ordinal >= 0),
    PRIMARY KEY (stage_id, dependency_stage_id),
    UNIQUE (stage_id, dependency_ordinal),
    CHECK (stage_id != dependency_stage_id)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_stage_dependencies_validate_insert
BEFORE INSERT ON research_campaign_stage_dependencies
WHEN NOT EXISTS (
    SELECT 1
    FROM research_campaign_stage_specs stage
    JOIN research_campaign_stage_specs dependency
      ON dependency.stage_id = NEW.dependency_stage_id
    WHERE stage.stage_id = NEW.stage_id
      AND stage.trial_fingerprint = dependency.trial_fingerprint
      AND dependency.stage_ordinal < stage.stage_ordinal
)
BEGIN
    SELECT RAISE(ABORT, 'stage dependency is cross-trial or points forward');
END;

CREATE TABLE research_campaign_trial_dependencies (
    trial_fingerprint TEXT NOT NULL
        REFERENCES research_trial_specs(trial_fingerprint) ON DELETE RESTRICT,
    dependency_trial_fingerprint TEXT NOT NULL
        REFERENCES research_trial_specs(trial_fingerprint) ON DELETE RESTRICT,
    dependency_ordinal INTEGER NOT NULL CHECK (dependency_ordinal >= 0),
    PRIMARY KEY (trial_fingerprint, dependency_trial_fingerprint),
    UNIQUE (trial_fingerprint, dependency_ordinal),
    CHECK (trial_fingerprint != dependency_trial_fingerprint)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_trial_dependencies_validate_insert
BEFORE INSERT ON research_campaign_trial_dependencies
WHEN NOT EXISTS (
    SELECT 1
    FROM research_trial_specs trial
    JOIN research_trial_specs dependency
      ON dependency.trial_fingerprint = NEW.dependency_trial_fingerprint
    WHERE trial.trial_fingerprint = NEW.trial_fingerprint
      AND trial.campaign_id = dependency.campaign_id
)
BEGIN
    SELECT RAISE(ABORT, 'trial dependency crosses a sealed campaign');
END;

CREATE TABLE research_trial_runs (
    trial_run_id TEXT PRIMARY KEY CHECK (
        length(trial_run_id) = 26
        AND trial_run_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(trial_run_id, 1, 1) BETWEEN '0' AND '7'
    ),
    trial_fingerprint TEXT NOT NULL
        REFERENCES research_trial_specs(trial_fingerprint) ON DELETE RESTRICT,
    origin_kind TEXT NOT NULL CHECK (origin_kind IN ('campaign', 'standalone', 'benchmark')),
    origin_campaign_id TEXT REFERENCES research_campaigns(campaign_id) ON DELETE RESTRICT,
    origin_benchmark_run_id TEXT REFERENCES research_benchmark_runs(run_id) ON DELETE RESTRICT,
    origin_benchmark_seal_fingerprint TEXT CHECK (
        origin_benchmark_seal_fingerprint IS NULL OR (
            length(origin_benchmark_seal_fingerprint) = 64
            AND origin_benchmark_seal_fingerprint NOT GLOB '*[^0-9a-f]*'
        )
    ),
    origin_benchmark_assignment_fingerprint TEXT CHECK (
        origin_benchmark_assignment_fingerprint IS NULL OR (
            length(origin_benchmark_assignment_fingerprint) = 64
            AND origin_benchmark_assignment_fingerprint NOT GLOB '*[^0-9a-f]*'
        )
    ),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    UNIQUE (trial_run_id, trial_fingerprint),
    CHECK (
        (origin_kind = 'campaign'
            AND origin_campaign_id IS NOT NULL
            AND origin_benchmark_run_id IS NULL
            AND origin_benchmark_seal_fingerprint IS NULL
            AND origin_benchmark_assignment_fingerprint IS NULL)
        OR (origin_kind = 'standalone'
            AND origin_campaign_id IS NULL
            AND origin_benchmark_run_id IS NULL
            AND origin_benchmark_seal_fingerprint IS NULL
            AND origin_benchmark_assignment_fingerprint IS NULL)
        OR (origin_kind = 'benchmark'
            AND origin_campaign_id IS NULL
            AND origin_benchmark_run_id IS NOT NULL
            AND origin_benchmark_seal_fingerprint IS NOT NULL
            AND origin_benchmark_assignment_fingerprint IS NOT NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_trial_runs_validate_insert
BEFORE INSERT ON research_trial_runs
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'trial_run'
    )
    OR (
        NEW.origin_kind = 'campaign'
        AND NOT EXISTS (
            SELECT 1
            FROM research_trial_specs trial
            WHERE trial.trial_fingerprint = NEW.trial_fingerprint
              AND trial.campaign_id = NEW.origin_campaign_id
        )
    )
    OR (
        NEW.origin_kind = 'benchmark'
        AND NOT EXISTS (
            SELECT 1 FROM research_benchmark_runs benchmark
            WHERE benchmark.run_id = NEW.origin_benchmark_run_id
              AND benchmark.seal_fingerprint = NEW.origin_benchmark_seal_fingerprint
              AND benchmark.assignment_fingerprint = NEW.origin_benchmark_assignment_fingerprint
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'trial run is unverified or crosses its frozen campaign');
END;

CREATE TABLE research_campaign_stage_attempts (
    stage_attempt_id TEXT PRIMARY KEY CHECK (
        length(stage_attempt_id) = 26
        AND stage_attempt_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(stage_attempt_id, 1, 1) BETWEEN '0' AND '7'
    ),
    trial_run_id TEXT NOT NULL REFERENCES research_trial_runs(trial_run_id) ON DELETE RESTRICT,
    stage_id TEXT NOT NULL REFERENCES research_campaign_stage_specs(stage_id) ON DELETE RESTRICT,
    attempt_ordinal INTEGER NOT NULL CHECK (attempt_ordinal >= 1),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    UNIQUE (trial_run_id, stage_id, attempt_ordinal)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_stage_attempts_validate_insert
BEFORE INSERT ON research_campaign_stage_attempts
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'stage_attempt'
    )
    OR NOT EXISTS (
        SELECT 1
        FROM research_trial_runs run
        JOIN research_campaign_stage_specs stage USING (trial_fingerprint)
        WHERE run.trial_run_id = NEW.trial_run_id
          AND stage.stage_id = NEW.stage_id
    )
    OR (
        NEW.attempt_ordinal > 1
        AND NOT EXISTS (
            SELECT 1 FROM research_campaign_stage_attempts prior
            WHERE prior.trial_run_id = NEW.trial_run_id
              AND prior.stage_id = NEW.stage_id
              AND prior.attempt_ordinal = NEW.attempt_ordinal - 1
              AND EXISTS (
                  SELECT 1 FROM research_trial_events terminal
                  WHERE terminal.trial_run_id = NEW.trial_run_id
                    AND terminal.stage_attempt_id = prior.stage_attempt_id
                    AND (
                        terminal.event_kind = 'attempt_abandoned'
                        OR (
                            terminal.event_kind = 'attempt_finished'
                            AND terminal.attempt_outcome != 'succeeded'
                        )
                    )
              )
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'stage attempt is unverified or not a retry of a terminal attempt');
END;

CREATE TABLE research_campaign_trial_attempts (
    trial_attempt_id TEXT PRIMARY KEY
        REFERENCES research_trial_runs(trial_run_id) ON DELETE RESTRICT CHECK (
        length(trial_attempt_id) = 26
        AND trial_attempt_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(trial_attempt_id, 1, 1) BETWEEN '0' AND '7'
    ),
    trial_fingerprint TEXT NOT NULL
        REFERENCES research_trial_specs(trial_fingerprint) ON DELETE RESTRICT,
    attempt_ordinal INTEGER NOT NULL CHECK (attempt_ordinal >= 1),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    UNIQUE (trial_fingerprint, attempt_ordinal),
    FOREIGN KEY (trial_attempt_id, trial_fingerprint)
        REFERENCES research_trial_runs(trial_run_id, trial_fingerprint) ON DELETE RESTRICT
) STRICT, WITHOUT ROWID;

CREATE TABLE research_campaign_events (
    campaign_id TEXT NOT NULL REFERENCES research_campaigns(campaign_id) ON DELETE RESTRICT,
    event_index INTEGER NOT NULL CHECK (event_index >= 0),
    previous_event_fingerprint TEXT CHECK (
        previous_event_fingerprint IS NULL OR (
            length(previous_event_fingerprint) = 64
            AND previous_event_fingerprint NOT GLOB '*[^0-9a-f]*'
        )
    ),
    event_fingerprint TEXT NOT NULL UNIQUE CHECK (
        length(event_fingerprint) = 64
        AND event_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    event_kind TEXT NOT NULL CHECK (event_kind IN (
        'prepared', 'started', 'pause_requested', 'paused', 'resumed',
        'cancel_requested', 'trial_reserved', 'trial_dispatched',
        'trial_finished', 'trial_reservation_released',
        'search_decision_recorded', 'campaign_closed'
    )),
    trial_attempt_id TEXT
        REFERENCES research_campaign_trial_attempts(trial_attempt_id) ON DELETE RESTRICT,
    attempt_outcome TEXT CHECK (attempt_outcome IS NULL OR attempt_outcome IN (
        'completed', 'failed', 'cancelled', 'interrupted', 'released'
    )),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    occurred_at_ms INTEGER NOT NULL CHECK (occurred_at_ms > 0),
    PRIMARY KEY (campaign_id, event_index),
    CHECK (
        (event_kind IN (
            'trial_reserved', 'trial_dispatched', 'trial_finished',
            'trial_reservation_released'
        ) AND trial_attempt_id IS NOT NULL)
        OR (event_kind NOT IN (
            'trial_reserved', 'trial_dispatched', 'trial_finished',
            'trial_reservation_released'
        ) AND trial_attempt_id IS NULL)
    ),
    CHECK (
        (event_kind = 'trial_finished' AND attempt_outcome IN (
            'completed', 'failed', 'cancelled', 'interrupted'
        ))
        OR (event_kind = 'trial_reservation_released' AND attempt_outcome = 'released')
        OR (event_kind NOT IN ('trial_finished', 'trial_reservation_released')
            AND attempt_outcome IS NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_campaign_events_validate_insert
BEFORE INSERT ON research_campaign_events
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'campaign_event'
    )
    OR (NEW.event_index = 0 AND (
        NEW.event_kind != 'prepared' OR NEW.previous_event_fingerprint IS NOT NULL
    ))
    OR (NEW.event_index > 0 AND NOT EXISTS (
        SELECT 1 FROM research_campaign_events prior
        WHERE prior.campaign_id = NEW.campaign_id
          AND prior.event_index = NEW.event_index - 1
          AND prior.event_fingerprint = NEW.previous_event_fingerprint
          AND prior.event_kind != 'campaign_closed'
    ))
    OR (
        NEW.trial_attempt_id IS NOT NULL
        AND NOT EXISTS (
            SELECT 1
            FROM research_campaign_trial_attempts attempt
            JOIN research_trial_specs trial USING (trial_fingerprint)
            WHERE attempt.trial_attempt_id = NEW.trial_attempt_id
              AND trial.campaign_id = NEW.campaign_id
        )
    )
    OR (
        NEW.event_kind IN ('trial_reserved', 'trial_dispatched')
        AND NOT EXISTS (
            SELECT 1 FROM research_campaign_trial_budget_reservations reservation
            WHERE reservation.trial_attempt_id = NEW.trial_attempt_id
        )
    )
    OR (
        NEW.event_kind = 'trial_finished'
        AND NOT EXISTS (
            SELECT 1
            FROM research_campaign_trial_budget_reservations reservation
            JOIN research_campaign_trial_budget_charges charge USING (reservation_id)
            WHERE reservation.trial_attempt_id = NEW.trial_attempt_id
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'campaign event is unverified or does not extend its exact hash chain');
END;

CREATE TRIGGER research_campaign_trial_attempts_validate_insert
BEFORE INSERT ON research_campaign_trial_attempts
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'campaign_trial_attempt'
    )
    OR NOT EXISTS (
        SELECT 1
        FROM research_trial_runs run
        WHERE run.trial_run_id = NEW.trial_attempt_id
          AND run.trial_fingerprint = NEW.trial_fingerprint
          AND run.origin_kind = 'campaign'
    )
    OR (
        NEW.attempt_ordinal > 1
        AND NOT EXISTS (
            SELECT 1
            FROM research_campaign_trial_attempts prior
            JOIN research_campaign_events terminal
              ON terminal.trial_attempt_id = prior.trial_attempt_id
            WHERE prior.trial_fingerprint = NEW.trial_fingerprint
              AND prior.attempt_ordinal = NEW.attempt_ordinal - 1
              AND (
                  (terminal.event_kind = 'trial_finished'
                    AND terminal.attempt_outcome != 'completed')
                  OR terminal.event_kind = 'trial_reservation_released'
              )
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'campaign trial attempt is unverified or not a retry');
END;

CREATE TABLE research_trial_events (
    trial_run_id TEXT NOT NULL REFERENCES research_trial_runs(trial_run_id) ON DELETE RESTRICT,
    trial_fingerprint TEXT NOT NULL
        REFERENCES research_trial_specs(trial_fingerprint) ON DELETE RESTRICT,
    event_index INTEGER NOT NULL CHECK (event_index >= 0),
    previous_event_fingerprint TEXT CHECK (
        previous_event_fingerprint IS NULL OR (
            length(previous_event_fingerprint) = 64
            AND previous_event_fingerprint NOT GLOB '*[^0-9a-f]*'
        )
    ),
    event_fingerprint TEXT NOT NULL UNIQUE CHECK (
        length(event_fingerprint) = 64
        AND event_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    event_kind TEXT NOT NULL CHECK (event_kind IN (
        'prepared', 'attempt_reserved', 'attempt_started', 'attempt_finished',
        'attempt_abandoned', 'trial_closed'
    )),
    stage_attempt_id TEXT
        REFERENCES research_campaign_stage_attempts(stage_attempt_id) ON DELETE RESTRICT,
    attempt_outcome TEXT CHECK (attempt_outcome IS NULL OR attempt_outcome IN (
        'succeeded', 'failed', 'cancelled', 'interrupted', 'abandoned'
    )),
    terminal_output_fingerprint TEXT CHECK (
        terminal_output_fingerprint IS NULL OR (
            length(terminal_output_fingerprint) = 64
            AND terminal_output_fingerprint NOT GLOB '*[^0-9a-f]*'
        )
    ),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    occurred_at_ms INTEGER NOT NULL CHECK (occurred_at_ms > 0),
    PRIMARY KEY (trial_run_id, event_index),
    FOREIGN KEY (trial_run_id, trial_fingerprint)
        REFERENCES research_trial_runs(trial_run_id, trial_fingerprint) ON DELETE RESTRICT,
    CHECK (
        (event_kind IN (
            'attempt_reserved', 'attempt_started', 'attempt_finished', 'attempt_abandoned'
        ) AND stage_attempt_id IS NOT NULL)
        OR (event_kind IN ('prepared', 'trial_closed') AND stage_attempt_id IS NULL)
    ),
    CHECK (
        (event_kind = 'attempt_finished' AND attempt_outcome IN (
            'succeeded', 'failed', 'cancelled', 'interrupted'
        ))
        OR (event_kind = 'attempt_abandoned' AND attempt_outcome = 'abandoned')
        OR (event_kind NOT IN ('attempt_finished', 'attempt_abandoned')
            AND attempt_outcome IS NULL)
    ),
    CHECK (
        (event_kind = 'attempt_finished' AND attempt_outcome = 'succeeded'
            AND terminal_output_fingerprint IS NOT NULL)
        OR ((event_kind != 'attempt_finished' OR attempt_outcome != 'succeeded')
            AND terminal_output_fingerprint IS NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_trial_events_validate_insert
BEFORE INSERT ON research_trial_events
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'trial_event'
    )
    OR (NEW.event_index = 0 AND (
        NEW.event_kind != 'prepared' OR NEW.previous_event_fingerprint IS NOT NULL
    ))
    OR (NEW.event_index > 0 AND NOT EXISTS (
        SELECT 1 FROM research_trial_events prior
        WHERE prior.trial_run_id = NEW.trial_run_id
          AND prior.event_index = NEW.event_index - 1
          AND prior.event_fingerprint = NEW.previous_event_fingerprint
          AND prior.event_kind != 'trial_closed'
    ))
    OR (
        NEW.stage_attempt_id IS NOT NULL
        AND NOT EXISTS (
            SELECT 1
            FROM research_campaign_stage_attempts attempt
            JOIN research_campaign_stage_specs stage USING (stage_id)
            WHERE attempt.stage_attempt_id = NEW.stage_attempt_id
              AND attempt.trial_run_id = NEW.trial_run_id
              AND stage.trial_fingerprint = NEW.trial_fingerprint
        )
    )
    OR (
        NEW.event_kind IN ('attempt_reserved', 'attempt_started')
        AND NOT EXISTS (
            SELECT 1 FROM research_campaign_budget_reservations reservation
            WHERE reservation.stage_attempt_id = NEW.stage_attempt_id
        )
    )
    OR (
        NEW.event_kind = 'attempt_finished'
        AND NOT EXISTS (
            SELECT 1
            FROM research_campaign_budget_reservations reservation
            JOIN research_campaign_budget_charges charge USING (reservation_id)
            WHERE reservation.stage_attempt_id = NEW.stage_attempt_id
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'trial event is unverified or does not extend its exact hash chain');
END;

CREATE TABLE research_campaign_budget_reservations (
    reservation_id TEXT PRIMARY KEY CHECK (
        length(reservation_id) = 26
        AND reservation_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(reservation_id, 1, 1) BETWEEN '0' AND '7'
    ),
    campaign_id TEXT NOT NULL REFERENCES research_campaigns(campaign_id) ON DELETE RESTRICT,
    stage_attempt_id TEXT NOT NULL UNIQUE
        REFERENCES research_campaign_stage_attempts(stage_attempt_id) ON DELETE RESTRICT,
    writer_tokens INTEGER NOT NULL CHECK (writer_tokens >= 0),
    controller_tokens INTEGER NOT NULL CHECK (controller_tokens >= 0),
    evaluations INTEGER NOT NULL CHECK (evaluations >= 0),
    wall_time_ms INTEGER NOT NULL CHECK (wall_time_ms > 0),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    reserved_at_ms INTEGER NOT NULL CHECK (reserved_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_budget_reservations_validate_insert
BEFORE INSERT ON research_campaign_budget_reservations
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'budget_reservation'
    )
    OR NOT EXISTS (
        SELECT 1
        FROM research_campaign_stage_attempts attempt
        JOIN research_campaign_stage_specs stage USING (stage_id)
        JOIN research_trial_specs trial USING (trial_fingerprint)
        WHERE attempt.stage_attempt_id = NEW.stage_attempt_id
          AND trial.campaign_id = NEW.campaign_id
          AND NEW.writer_tokens <= stage.maximum_writer_tokens
          AND NEW.controller_tokens <= stage.maximum_controller_tokens
          AND NEW.evaluations <= stage.maximum_evaluations
          AND NEW.wall_time_ms <= stage.maximum_wall_time_ms
    )
BEGIN
    SELECT RAISE(ABORT, 'budget reservation is unverified or exceeds its frozen stage');
END;

CREATE TABLE research_campaign_budget_charges (
    reservation_id TEXT PRIMARY KEY
        REFERENCES research_campaign_budget_reservations(reservation_id) ON DELETE RESTRICT,
    writer_tokens INTEGER NOT NULL CHECK (writer_tokens >= 0),
    controller_tokens INTEGER NOT NULL CHECK (controller_tokens >= 0),
    evaluations INTEGER NOT NULL CHECK (evaluations >= 0),
    wall_time_ms INTEGER NOT NULL CHECK (wall_time_ms >= 0),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    charged_at_ms INTEGER NOT NULL CHECK (charged_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_budget_charges_validate_insert
BEFORE INSERT ON research_campaign_budget_charges
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'budget_charge'
    )
    OR NOT EXISTS (
        SELECT 1 FROM research_campaign_budget_reservations reservation
        WHERE reservation.reservation_id = NEW.reservation_id
          AND NEW.writer_tokens <= reservation.writer_tokens
          AND NEW.controller_tokens <= reservation.controller_tokens
          AND NEW.evaluations <= reservation.evaluations
          AND NEW.wall_time_ms <= reservation.wall_time_ms
    )
BEGIN
    SELECT RAISE(ABORT, 'budget charge is unverified or exceeds its reservation');
END;

CREATE TABLE research_campaign_search_decisions (
    campaign_id TEXT NOT NULL REFERENCES research_campaigns(campaign_id) ON DELETE RESTRICT,
    decision_index INTEGER NOT NULL CHECK (decision_index >= 0),
    decision_kind TEXT NOT NULL CHECK (decision_kind IN (
        'blocked_factorial_scheduled', 'nested_pool_recorded',
        'successive_halving_applied', 'pressure_advanced', 'pressure_stopped',
        'map_elites_initialized', 'map_elites_advanced'
    )),
    parent_archive_fingerprint TEXT CHECK (
        parent_archive_fingerprint IS NULL OR (
            length(parent_archive_fingerprint) = 64
            AND parent_archive_fingerprint NOT GLOB '*[^0-9a-f]*'
        )
    ),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    decided_at_ms INTEGER NOT NULL CHECK (decided_at_ms > 0),
    PRIMARY KEY (campaign_id, decision_index)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_search_decisions_validate_insert
BEFORE INSERT ON research_campaign_search_decisions
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'search_decision'
    )
    OR (NEW.decision_index > 0 AND NOT EXISTS (
        SELECT 1 FROM research_campaign_search_decisions prior
        WHERE prior.campaign_id = NEW.campaign_id
          AND prior.decision_index = NEW.decision_index - 1
    ))
BEGIN
    SELECT RAISE(ABORT, 'search decision is unverified or not contiguous');
END;

CREATE TABLE research_campaign_trial_budget_reservations (
    reservation_id TEXT PRIMARY KEY CHECK (
        length(reservation_id) = 26
        AND reservation_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(reservation_id, 1, 1) BETWEEN '0' AND '7'
    ),
    trial_attempt_id TEXT NOT NULL UNIQUE
        REFERENCES research_campaign_trial_attempts(trial_attempt_id) ON DELETE RESTRICT,
    writer_tokens INTEGER NOT NULL CHECK (writer_tokens > 0),
    controller_tokens INTEGER NOT NULL CHECK (controller_tokens >= 0),
    evaluations INTEGER NOT NULL CHECK (evaluations > 0),
    wall_time_ms INTEGER NOT NULL CHECK (wall_time_ms > 0),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    reserved_at_ms INTEGER NOT NULL CHECK (reserved_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_trial_budget_reservations_validate_insert
BEFORE INSERT ON research_campaign_trial_budget_reservations
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'budget_reservation'
    )
    OR NOT EXISTS (
        SELECT 1
        FROM research_campaign_trial_attempts attempt
        JOIN research_trial_specs trial USING (trial_fingerprint)
        WHERE attempt.trial_attempt_id = NEW.trial_attempt_id
          AND NEW.writer_tokens = trial.maximum_writer_tokens
          AND NEW.controller_tokens = trial.maximum_controller_tokens
          AND NEW.evaluations = trial.maximum_evaluations
          AND NEW.wall_time_ms = trial.maximum_wall_time_ms
    )
BEGIN
    SELECT RAISE(ABORT, 'campaign trial reservation differs from its exact frozen maximum');
END;

CREATE TABLE research_campaign_trial_budget_charges (
    reservation_id TEXT PRIMARY KEY
        REFERENCES research_campaign_trial_budget_reservations(reservation_id) ON DELETE RESTRICT,
    writer_tokens INTEGER NOT NULL CHECK (writer_tokens >= 0),
    controller_tokens INTEGER NOT NULL CHECK (controller_tokens >= 0),
    evaluations INTEGER NOT NULL CHECK (evaluations >= 0),
    wall_time_ms INTEGER NOT NULL CHECK (wall_time_ms >= 0),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    charged_at_ms INTEGER NOT NULL CHECK (charged_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_trial_budget_charges_validate_insert
BEFORE INSERT ON research_campaign_trial_budget_charges
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'budget_charge'
    )
    OR NOT EXISTS (
        SELECT 1 FROM research_campaign_trial_budget_reservations reservation
        WHERE reservation.reservation_id = NEW.reservation_id
          AND NEW.writer_tokens <= reservation.writer_tokens
          AND NEW.controller_tokens <= reservation.controller_tokens
          AND NEW.evaluations <= reservation.evaluations
          AND NEW.wall_time_ms <= reservation.wall_time_ms
    )
BEGIN
    SELECT RAISE(ABORT, 'campaign trial charge is unverified or exceeds its reservation');
END;

-- Story, prompt-mask, and backtranslation evidence ----------------------------

CREATE TABLE research_story_graphs (
    story_graph_id TEXT PRIMARY KEY CHECK (
        length(story_graph_id) = 26
        AND story_graph_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(story_graph_id, 1, 1) BETWEEN '0' AND '7'
    ),
    project_id TEXT NOT NULL CHECK (
        length(project_id) = 26
        AND project_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(project_id, 1, 1) BETWEEN '0' AND '7'
    ),
    graph_fingerprint TEXT NOT NULL UNIQUE CHECK (
        length(graph_fingerprint) = 64
        AND graph_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    node_count INTEGER NOT NULL CHECK (node_count BETWEEN 1 AND 16384),
    relation_count INTEGER NOT NULL CHECK (relation_count BETWEEN 0 AND 65536),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_story_graphs_validate_insert
BEFORE INSERT ON research_story_graphs
WHEN NOT EXISTS (
    SELECT 1 FROM research_execution_records record
    WHERE record.record_fingerprint = NEW.record_fingerprint
      AND record.record_kind = 'story_graph'
)
BEGIN
    SELECT RAISE(ABORT, 'story graph lacks an exact graph record');
END;

CREATE TABLE research_story_states (
    story_state_id TEXT PRIMARY KEY CHECK (
        length(story_state_id) = 26
        AND story_state_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(story_state_id, 1, 1) BETWEEN '0' AND '7'
    ),
    story_graph_id TEXT NOT NULL REFERENCES research_story_graphs(story_graph_id) ON DELETE RESTRICT,
    anchor_story_node_id TEXT NOT NULL CHECK (
        length(anchor_story_node_id) = 26
        AND anchor_story_node_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(anchor_story_node_id, 1, 1) BETWEEN '0' AND '7'
    ),
    source_revision_id TEXT NOT NULL REFERENCES revisions(revision_id) ON DELETE RESTRICT,
    source_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    fact_count INTEGER NOT NULL CHECK (fact_count BETWEEN 1 AND 4096),
    state_fingerprint TEXT NOT NULL UNIQUE CHECK (
        length(state_fingerprint) = 64
        AND state_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_story_states_validate_insert
BEFORE INSERT ON research_story_states
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'story_state'
    )
    OR NOT EXISTS (
        SELECT 1
        FROM revisions revision
        JOIN artifacts artifact ON artifact.artifact_id = revision.artifact_id
        WHERE revision.revision_id = NEW.source_revision_id
          AND artifact.blob_id = NEW.source_blob_id
    )
BEGIN
    SELECT RAISE(ABORT, 'story state lacks an exact source revision or state record');
END;

CREATE TABLE research_prompt_masks (
    mask_fingerprint TEXT PRIMARY KEY CHECK (
        length(mask_fingerprint) = 64
        AND mask_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    campaign_id TEXT NOT NULL REFERENCES research_campaigns(campaign_id) ON DELETE RESTRICT,
    stage_attempt_id TEXT NOT NULL
        REFERENCES research_campaign_stage_attempts(stage_attempt_id) ON DELETE RESTRICT,
    mask_kind TEXT NOT NULL CHECK (mask_kind IN (
        'entity', 'beat', 'state', 'content_style', 'suffix', 'model_specific_fim'
    )),
    source_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    rendered_blob_id TEXT REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    backend_capability_fingerprint TEXT CHECK (
        backend_capability_fingerprint IS NULL OR (
            length(backend_capability_fingerprint) = 64
            AND backend_capability_fingerprint NOT GLOB '*[^0-9a-f]*'
        )
    ),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    CHECK (
        (mask_kind = 'model_specific_fim'
            AND backend_capability_fingerprint IS NOT NULL
            AND rendered_blob_id IS NULL)
        OR (mask_kind != 'model_specific_fim'
            AND backend_capability_fingerprint IS NULL
            AND rendered_blob_id IS NOT NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_prompt_masks_validate_insert
BEFORE INSERT ON research_prompt_masks
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'prompt_mask'
    )
    OR NOT EXISTS (SELECT 1 FROM blobs blob WHERE blob.blob_id = NEW.source_blob_id AND blob.byte_len > 0)
    OR (
        NEW.rendered_blob_id IS NOT NULL
        AND NOT EXISTS (
            SELECT 1 FROM blobs blob
            WHERE blob.blob_id = NEW.rendered_blob_id AND blob.byte_len > 0
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'prompt mask lacks exact non-empty source, output, or record evidence');
END;

CREATE TABLE research_backtranslation_proposals (
    proposal_fingerprint TEXT PRIMARY KEY CHECK (
        length(proposal_fingerprint) = 64
        AND proposal_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    source_revision_id TEXT NOT NULL REFERENCES revisions(revision_id) ON DELETE RESTRICT,
    source_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    source_start_byte INTEGER NOT NULL CHECK (source_start_byte >= 0),
    source_end_byte INTEGER NOT NULL CHECK (source_end_byte > source_start_byte),
    field_count INTEGER NOT NULL CHECK (field_count BETWEEN 0 AND 4096),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_backtranslation_proposals_validate_insert
BEFORE INSERT ON research_backtranslation_proposals
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'backtranslation_proposal'
    )
    OR NOT EXISTS (
        SELECT 1
        FROM revisions revision
        JOIN artifacts artifact ON artifact.artifact_id = revision.artifact_id
        JOIN blobs source ON source.blob_id = artifact.blob_id
        WHERE revision.revision_id = NEW.source_revision_id
          AND source.blob_id = NEW.source_blob_id
          AND NEW.source_end_byte <= source.byte_len
    )
BEGIN
    SELECT RAISE(ABORT, 'backtranslation proposal is not bound to its exact source range');
END;

CREATE TABLE research_backtranslation_auditions (
    audition_fingerprint TEXT PRIMARY KEY CHECK (
        length(audition_fingerprint) = 64
        AND audition_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    proposal_fingerprint TEXT NOT NULL
        REFERENCES research_backtranslation_proposals(proposal_fingerprint) ON DELETE RESTRICT,
    writer_evidence_fingerprint TEXT NOT NULL CHECK (
        length(writer_evidence_fingerprint) = 64
        AND writer_evidence_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    writer_batch_count INTEGER NOT NULL CHECK (writer_batch_count BETWEEN 1 AND 256),
    evaluator_evidence_fingerprint TEXT NOT NULL CHECK (
        length(evaluator_evidence_fingerprint) = 64
        AND evaluator_evidence_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    evaluator_receipt_count INTEGER NOT NULL CHECK (evaluator_receipt_count BETWEEN 1 AND 256),
    work_disjoint INTEGER NOT NULL CHECK (work_disjoint IN (0, 1)),
    causal_transfer_decision TEXT NOT NULL CHECK (causal_transfer_decision IN (
        'improved', 'tied', 'regressed', 'abstained'
    )),
    leakage_decision TEXT NOT NULL CHECK (leakage_decision IN (
        'clear', 'suspected', 'detected', 'abstained'
    )),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TABLE research_backtranslation_audition_batches (
    audition_fingerprint TEXT NOT NULL
        REFERENCES research_backtranslation_auditions(audition_fingerprint)
        ON DELETE RESTRICT DEFERRABLE INITIALLY DEFERRED,
    batch_ordinal INTEGER NOT NULL CHECK (batch_ordinal BETWEEN 0 AND 255),
    batch_verification_fingerprint TEXT NOT NULL
        REFERENCES research_verified_inference_batch_seals(batch_verification_fingerprint)
        ON DELETE RESTRICT,
    PRIMARY KEY (audition_fingerprint, batch_ordinal),
    UNIQUE (audition_fingerprint, batch_verification_fingerprint)
) STRICT, WITHOUT ROWID;

CREATE TABLE research_backtranslation_audition_evaluator_receipts (
    audition_fingerprint TEXT NOT NULL
        REFERENCES research_backtranslation_auditions(audition_fingerprint)
        ON DELETE RESTRICT DEFERRABLE INITIALLY DEFERRED,
    receipt_ordinal INTEGER NOT NULL CHECK (receipt_ordinal BETWEEN 0 AND 255),
    evaluator_receipt_fingerprint TEXT NOT NULL
        REFERENCES research_evaluation_receipts(receipt_fingerprint) ON DELETE RESTRICT
        CHECK (
        length(evaluator_receipt_fingerprint) = 64
        AND evaluator_receipt_fingerprint NOT GLOB '*[^0-9a-f]*'
        ),
    PRIMARY KEY (audition_fingerprint, receipt_ordinal),
    UNIQUE (audition_fingerprint, evaluator_receipt_fingerprint)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_backtranslation_auditions_validate_insert
BEFORE INSERT ON research_backtranslation_auditions
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'backtranslation_audition'
    )
    OR (
        SELECT COUNT(*) FROM research_backtranslation_audition_batches batch
        WHERE batch.audition_fingerprint = NEW.audition_fingerprint
    ) != NEW.writer_batch_count
    OR (
        SELECT COALESCE(MIN(batch_ordinal), -1)
        FROM research_backtranslation_audition_batches batch
        WHERE batch.audition_fingerprint = NEW.audition_fingerprint
    ) != 0
    OR (
        SELECT COALESCE(MAX(batch_ordinal), -1)
        FROM research_backtranslation_audition_batches batch
        WHERE batch.audition_fingerprint = NEW.audition_fingerprint
    ) != NEW.writer_batch_count - 1
    OR (
        SELECT COUNT(*) FROM research_backtranslation_audition_evaluator_receipts receipt
        WHERE receipt.audition_fingerprint = NEW.audition_fingerprint
    ) != NEW.evaluator_receipt_count
    OR (
        SELECT COALESCE(MIN(receipt_ordinal), -1)
        FROM research_backtranslation_audition_evaluator_receipts receipt
        WHERE receipt.audition_fingerprint = NEW.audition_fingerprint
    ) != 0
    OR (
        SELECT COALESCE(MAX(receipt_ordinal), -1)
        FROM research_backtranslation_audition_evaluator_receipts receipt
        WHERE receipt.audition_fingerprint = NEW.audition_fingerprint
    ) != NEW.evaluator_receipt_count - 1
BEGIN
    SELECT RAISE(ABORT, 'backtranslation audition lacks exact contiguous writer evidence');
END;

CREATE TABLE research_backtranslation_acceptances (
    acceptance_fingerprint TEXT PRIMARY KEY CHECK (
        length(acceptance_fingerprint) = 64
        AND acceptance_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    audition_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_backtranslation_auditions(audition_fingerprint) ON DELETE RESTRICT,
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    accepted_at_ms INTEGER NOT NULL CHECK (accepted_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_backtranslation_acceptances_validate_insert
BEFORE INSERT ON research_backtranslation_acceptances
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'backtranslation_acceptance'
    )
    OR NOT EXISTS (
        SELECT 1 FROM research_backtranslation_auditions audition
        WHERE audition.audition_fingerprint = NEW.audition_fingerprint
          AND audition.work_disjoint = 1
          AND audition.causal_transfer_decision = 'improved'
          AND audition.leakage_decision = 'clear'
    )
BEGIN
    SELECT RAISE(ABORT, 'backtranslation acceptance lacks passing fresh-writer evidence');
END;

-- Evaluation and quality-diversity archives -----------------------------------

CREATE TABLE research_evaluation_tasks (
    task_id TEXT PRIMARY KEY CHECK (
        length(task_id) = 26
        AND task_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(task_id, 1, 1) BETWEEN '0' AND '7'
    ),
    candidate_occurrence_id TEXT CHECK (
        candidate_occurrence_id IS NULL OR (
            length(candidate_occurrence_id) = 26
            AND candidate_occurrence_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
            AND substr(candidate_occurrence_id, 1, 1) BETWEEN '0' AND '7'
        )
    ),
    evaluation_kind TEXT NOT NULL CHECK (evaluation_kind IN (
        'hard_gate', 'criterion_card', 'blind_pairwise', 'descriptor',
        'close_read', 'human_review'
    )),
    pack_fingerprint TEXT NOT NULL CHECK (
        length(pack_fingerprint) = 64
        AND pack_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    packet_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    evidence_authority TEXT NOT NULL CHECK (evidence_authority IN (
        'claimed_diagnostic', 'verified_projection'
    )),
    candidate_blob_id TEXT REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    projection_binding_fingerprint TEXT CHECK (
        projection_binding_fingerprint IS NULL OR (
            length(projection_binding_fingerprint) = 64
            AND projection_binding_fingerprint NOT GLOB '*[^0-9a-f]*'
        )
    ),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    CHECK (
        (evaluation_kind = 'blind_pairwise' AND candidate_occurrence_id IS NULL)
        OR (evaluation_kind != 'blind_pairwise' AND candidate_occurrence_id IS NOT NULL)
    ),
    CHECK (
        (evidence_authority = 'claimed_diagnostic'
            AND candidate_blob_id IS NULL
            AND projection_binding_fingerprint IS NULL)
        OR (evidence_authority = 'verified_projection' AND (
            (evaluation_kind = 'blind_pairwise'
                AND candidate_blob_id IS NULL
                AND projection_binding_fingerprint IS NULL)
            OR (evaluation_kind != 'blind_pairwise'
                AND candidate_blob_id IS NOT NULL
                AND projection_binding_fingerprint IS NOT NULL)
        ))
    )
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_evaluation_tasks_validate_insert
BEFORE INSERT ON research_evaluation_tasks
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'evaluation_task'
    )
    OR NOT EXISTS (
        SELECT 1 FROM blobs packet
        WHERE packet.blob_id = NEW.packet_blob_id
          AND packet.byte_len BETWEEN 1 AND 16777216
    )
    OR (
        NEW.evidence_authority = 'verified_projection'
        AND NEW.evaluation_kind != 'blind_pairwise'
        AND NOT EXISTS (
            SELECT 1
            FROM research_candidate_projections projection
            JOIN research_admission_records admission
              ON admission.subject_kind = 'candidate_projection'
             AND admission.subject_id = projection.projection_id
            WHERE projection.projection_id = NEW.candidate_occurrence_id
              AND projection.resulting_blob_id = NEW.candidate_blob_id
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'evaluation task lacks an exact non-empty packet');
END;

CREATE TABLE research_evaluation_receipts (
    receipt_fingerprint TEXT PRIMARY KEY CHECK (
        length(receipt_fingerprint) = 64
        AND receipt_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    task_id TEXT NOT NULL REFERENCES research_evaluation_tasks(task_id) ON DELETE RESTRICT,
    evaluator_class TEXT NOT NULL CHECK (evaluator_class IN (
        'claimed_unknown', 'local_critic', 'frontier_critic', 'learned_head'
    )),
    receipt_authority TEXT NOT NULL CHECK (receipt_authority IN (
        'claimed_diagnostic', 'backend_verified'
    )),
    outcome TEXT NOT NULL CHECK (outcome IN ('validated', 'abstained', 'rejected')),
    evaluator_fingerprint TEXT NOT NULL CHECK (
        length(evaluator_fingerprint) = 64
        AND evaluator_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    pairwise_preference TEXT CHECK (
        pairwise_preference IS NULL
        OR pairwise_preference IN ('first', 'second', 'tie', 'abstain')
    ),
    raw_response_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    completed_at_ms INTEGER NOT NULL CHECK (completed_at_ms > 0),
    CHECK (
        (receipt_authority = 'claimed_diagnostic' AND evaluator_class = 'claimed_unknown')
        OR (receipt_authority = 'backend_verified' AND evaluator_class != 'claimed_unknown')
    )
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_evaluation_receipts_validate_insert
BEFORE INSERT ON research_evaluation_receipts
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'evaluation_receipt'
    )
    OR NOT EXISTS (
        SELECT 1 FROM blobs response
        WHERE response.blob_id = NEW.raw_response_blob_id
          AND response.byte_len BETWEEN 1 AND 4194304
    )
    OR NOT EXISTS (
        SELECT 1 FROM research_evaluation_tasks task
        WHERE task.task_id = NEW.task_id
          AND (
              (task.evaluation_kind = 'blind_pairwise'
                AND NEW.pairwise_preference IS NOT NULL)
              OR (task.evaluation_kind != 'blind_pairwise'
                AND NEW.pairwise_preference IS NULL)
          )
    )
BEGIN
    SELECT RAISE(ABORT, 'evaluation receipt lacks exact non-empty response evidence');
END;

CREATE TABLE research_evidence_spans (
    receipt_fingerprint TEXT NOT NULL
        REFERENCES research_evaluation_receipts(receipt_fingerprint) ON DELETE RESTRICT,
    evidence_index INTEGER NOT NULL CHECK (evidence_index >= 0),
    candidate_occurrence_id TEXT NOT NULL CHECK (
        length(candidate_occurrence_id) = 26
        AND candidate_occurrence_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(candidate_occurrence_id, 1, 1) BETWEEN '0' AND '7'
    ),
    candidate_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    start_byte INTEGER NOT NULL CHECK (start_byte >= 0),
    end_byte INTEGER NOT NULL CHECK (end_byte > start_byte),
    quote_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    criterion_id TEXT NOT NULL CHECK (length(CAST(criterion_id AS BLOB)) BETWEEN 1 AND 128),
    PRIMARY KEY (receipt_fingerprint, evidence_index)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_evidence_spans_validate_insert
BEFORE INSERT ON research_evidence_spans
WHEN NOT EXISTS (
    SELECT 1 FROM blobs candidate, blobs quote
    WHERE candidate.blob_id = NEW.candidate_blob_id
      AND quote.blob_id = NEW.quote_blob_id
      AND quote.byte_len = NEW.end_byte - NEW.start_byte
      AND NEW.end_byte <= candidate.byte_len
)
    OR NOT EXISTS (
        SELECT 1
        FROM research_evaluation_receipts receipt
        JOIN research_evaluation_tasks task USING (task_id)
        LEFT JOIN research_pairwise_assignments assignment USING (task_id)
        WHERE receipt.receipt_fingerprint = NEW.receipt_fingerprint
          AND (
              task.candidate_occurrence_id = NEW.candidate_occurrence_id
              OR (
                  task.evaluation_kind = 'blind_pairwise'
                  AND NEW.candidate_occurrence_id IN (
                      assignment.first_occurrence_id,
                      assignment.second_occurrence_id
                  )
              )
          )
          AND (
              task.evidence_authority = 'claimed_diagnostic'
              OR (
                  task.evidence_authority = 'verified_projection'
                  AND (
                      (task.evaluation_kind != 'blind_pairwise'
                        AND task.candidate_occurrence_id = NEW.candidate_occurrence_id
                        AND task.candidate_blob_id = NEW.candidate_blob_id)
                      OR (task.evaluation_kind = 'blind_pairwise'
                        AND assignment.evidence_authority = 'verified_projection'
                        AND (
                            (assignment.first_occurrence_id = NEW.candidate_occurrence_id
                              AND assignment.first_candidate_blob_id = NEW.candidate_blob_id)
                            OR (assignment.second_occurrence_id = NEW.candidate_occurrence_id
                              AND assignment.second_candidate_blob_id = NEW.candidate_blob_id)
                        ))
                  )
              )
          )
    )
BEGIN
    SELECT RAISE(ABORT, 'evaluation evidence range is outside its exact candidate bytes');
END;

CREATE TABLE research_pairwise_assignments (
    assignment_fingerprint TEXT PRIMARY KEY CHECK (
        length(assignment_fingerprint) = 64
        AND assignment_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    task_id TEXT NOT NULL UNIQUE REFERENCES research_evaluation_tasks(task_id) ON DELETE RESTRICT,
    first_occurrence_id TEXT NOT NULL CHECK (
        length(first_occurrence_id) = 26
        AND first_occurrence_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(first_occurrence_id, 1, 1) BETWEEN '0' AND '7'
    ),
    second_occurrence_id TEXT NOT NULL CHECK (
        length(second_occurrence_id) = 26
        AND second_occurrence_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(second_occurrence_id, 1, 1) BETWEEN '0' AND '7'
    ),
    evidence_authority TEXT NOT NULL CHECK (evidence_authority IN (
        'claimed_diagnostic', 'verified_projection'
    )),
    first_candidate_blob_id TEXT REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    first_projection_binding_fingerprint TEXT CHECK (
        first_projection_binding_fingerprint IS NULL OR (
            length(first_projection_binding_fingerprint) = 64
            AND first_projection_binding_fingerprint NOT GLOB '*[^0-9a-f]*'
        )
    ),
    second_candidate_blob_id TEXT REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    second_projection_binding_fingerprint TEXT CHECK (
        second_projection_binding_fingerprint IS NULL OR (
            length(second_projection_binding_fingerprint) = 64
            AND second_projection_binding_fingerprint NOT GLOB '*[^0-9a-f]*'
        )
    ),
    label_map_fingerprint TEXT NOT NULL CHECK (
        length(label_map_fingerprint) = 64
        AND label_map_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    order_cell INTEGER NOT NULL CHECK (order_cell BETWEEN 0 AND 1),
    criterion_order_cell INTEGER NOT NULL CHECK (criterion_order_cell BETWEEN 0 AND 1),
    anchor_order_cell INTEGER NOT NULL CHECK (anchor_order_cell BETWEEN 0 AND 1),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    CHECK (first_occurrence_id != second_occurrence_id),
    CHECK (
        (evidence_authority = 'claimed_diagnostic'
            AND first_candidate_blob_id IS NULL
            AND first_projection_binding_fingerprint IS NULL
            AND second_candidate_blob_id IS NULL
            AND second_projection_binding_fingerprint IS NULL)
        OR (evidence_authority = 'verified_projection'
            AND first_candidate_blob_id IS NOT NULL
            AND first_projection_binding_fingerprint IS NOT NULL
            AND second_candidate_blob_id IS NOT NULL
            AND second_projection_binding_fingerprint IS NOT NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_pairwise_assignments_validate_insert
BEFORE INSERT ON research_pairwise_assignments
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'pairwise_assignment'
    )
    OR NOT EXISTS (
        SELECT 1 FROM research_evaluation_tasks task
        WHERE task.task_id = NEW.task_id
          AND task.evaluation_kind = 'blind_pairwise'
          AND task.candidate_occurrence_id IS NULL
          AND task.evidence_authority = NEW.evidence_authority
    )
    OR (
        NEW.evidence_authority = 'verified_projection'
        AND (
            NOT EXISTS (
                SELECT 1
                FROM research_candidate_projections projection
                JOIN research_admission_records admission
                  ON admission.subject_kind = 'candidate_projection'
                 AND admission.subject_id = projection.projection_id
                WHERE projection.projection_id = NEW.first_occurrence_id
                  AND projection.resulting_blob_id = NEW.first_candidate_blob_id
            )
            OR NOT EXISTS (
                SELECT 1
                FROM research_candidate_projections projection
                JOIN research_admission_records admission
                  ON admission.subject_kind = 'candidate_projection'
                 AND admission.subject_id = projection.projection_id
                WHERE projection.projection_id = NEW.second_occurrence_id
                  AND projection.resulting_blob_id = NEW.second_candidate_blob_id
            )
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'pairwise assignment lacks an exact blinded assignment record');
END;

CREATE TABLE research_score_vectors (
    score_vector_fingerprint TEXT PRIMARY KEY CHECK (
        length(score_vector_fingerprint) = 64
        AND score_vector_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    receipt_fingerprint TEXT NOT NULL
        REFERENCES research_evaluation_receipts(receipt_fingerprint) ON DELETE RESTRICT,
    candidate_occurrence_id TEXT NOT NULL CHECK (
        length(candidate_occurrence_id) = 26
        AND candidate_occurrence_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(candidate_occurrence_id, 1, 1) BETWEEN '0' AND '7'
    ),
    criterion_count INTEGER NOT NULL CHECK (criterion_count BETWEEN 1 AND 64),
    pessimistic INTEGER NOT NULL CHECK (pessimistic IN (0, 1)),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_score_vectors_validate_insert
BEFORE INSERT ON research_score_vectors
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'score_vector'
    )
    OR NOT EXISTS (
        SELECT 1
        FROM research_evaluation_receipts receipt
        JOIN research_evaluation_tasks task USING (task_id)
        WHERE receipt.receipt_fingerprint = NEW.receipt_fingerprint
          AND task.candidate_occurrence_id = NEW.candidate_occurrence_id
    )
BEGIN
    SELECT RAISE(ABORT, 'score vector lacks an exact vector record');
END;

CREATE TABLE research_candidate_descriptors (
    descriptor_fingerprint TEXT PRIMARY KEY CHECK (
        length(descriptor_fingerprint) = 64
        AND descriptor_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    receipt_fingerprint TEXT NOT NULL
        REFERENCES research_evaluation_receipts(receipt_fingerprint) ON DELETE RESTRICT,
    candidate_occurrence_id TEXT NOT NULL CHECK (
        length(candidate_occurrence_id) = 26
        AND candidate_occurrence_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'
        AND substr(candidate_occurrence_id, 1, 1) BETWEEN '0' AND '7'
    ),
    axis_count INTEGER NOT NULL CHECK (axis_count BETWEEN 1 AND 32),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_candidate_descriptors_validate_insert
BEFORE INSERT ON research_candidate_descriptors
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'candidate_descriptor'
    )
    OR NOT EXISTS (
        SELECT 1
        FROM research_evaluation_receipts receipt
        JOIN research_evaluation_tasks task USING (task_id)
        WHERE receipt.receipt_fingerprint = NEW.receipt_fingerprint
          AND task.candidate_occurrence_id = NEW.candidate_occurrence_id
    )
BEGIN
    SELECT RAISE(ABORT, 'candidate descriptor lacks an exact descriptor record');
END;

CREATE TABLE research_preference_labels (
    label_fingerprint TEXT PRIMARY KEY CHECK (
        length(label_fingerprint) = 64
        AND label_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    assignment_fingerprint TEXT NOT NULL
        REFERENCES research_pairwise_assignments(assignment_fingerprint) ON DELETE RESTRICT,
    receipt_fingerprint TEXT NOT NULL
        REFERENCES research_evaluation_receipts(receipt_fingerprint) ON DELETE RESTRICT,
    label_source TEXT NOT NULL CHECK (label_source IN (
        'claimed_unknown', 'frontier_weak', 'local_critic_weak'
    )),
    preference TEXT NOT NULL CHECK (preference IN ('first', 'second', 'tie', 'abstain')),
    source_verifier_fingerprint TEXT CHECK (
        source_verifier_fingerprint IS NULL OR (
            length(source_verifier_fingerprint) = 64
            AND source_verifier_fingerprint NOT GLOB '*[^0-9a-f]*'
        )
    ),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    CHECK (
        (label_source = 'claimed_unknown' AND source_verifier_fingerprint IS NULL)
        OR (label_source != 'claimed_unknown' AND source_verifier_fingerprint IS NOT NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_preference_labels_validate_insert
BEFORE INSERT ON research_preference_labels
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'preference_label'
    )
    OR NOT EXISTS (
        SELECT 1
        FROM research_pairwise_assignments assignment
        JOIN research_evaluation_receipts receipt ON receipt.task_id = assignment.task_id
        WHERE assignment.assignment_fingerprint = NEW.assignment_fingerprint
          AND receipt.receipt_fingerprint = NEW.receipt_fingerprint
          AND (
              (NEW.label_source = 'claimed_unknown'
                AND assignment.evidence_authority = 'claimed_diagnostic'
                AND receipt.receipt_authority = 'claimed_diagnostic'
                AND receipt.evaluator_class = 'claimed_unknown')
              OR (NEW.label_source = 'frontier_weak'
                AND assignment.evidence_authority = 'verified_projection'
                AND receipt.receipt_authority = 'backend_verified'
                AND receipt.evaluator_class = 'frontier_critic')
              OR (NEW.label_source = 'local_critic_weak'
                AND assignment.evidence_authority = 'verified_projection'
                AND receipt.receipt_authority = 'backend_verified'
                AND receipt.evaluator_class = 'local_critic')
          )
          AND receipt.pairwise_preference = NEW.preference
    )
BEGIN
    SELECT RAISE(ABORT, 'preference label does not belong to its blinded task receipt');
END;

CREATE TABLE research_archive_snapshots (
    archive_fingerprint TEXT PRIMARY KEY CHECK (
        length(archive_fingerprint) = 64
        AND archive_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    campaign_id TEXT NOT NULL REFERENCES research_campaigns(campaign_id) ON DELETE RESTRICT,
    parent_archive_fingerprint TEXT
        REFERENCES research_archive_snapshots(archive_fingerprint) ON DELETE RESTRICT,
    generation INTEGER NOT NULL CHECK (generation >= 0),
    cell_count INTEGER NOT NULL CHECK (cell_count >= 0),
    candidate_count INTEGER NOT NULL CHECK (candidate_count >= 0),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    CHECK (
        (generation = 0 AND parent_archive_fingerprint IS NULL)
        OR (generation > 0 AND parent_archive_fingerprint IS NOT NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_archive_snapshots_validate_insert
BEFORE INSERT ON research_archive_snapshots
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'archive_snapshot'
    )
    OR (
        NEW.parent_archive_fingerprint IS NOT NULL
        AND NOT EXISTS (
            SELECT 1 FROM research_archive_snapshots parent
            WHERE parent.archive_fingerprint = NEW.parent_archive_fingerprint
              AND parent.campaign_id = NEW.campaign_id
              AND parent.generation + 1 = NEW.generation
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'archive snapshot is unverified or does not extend its parent');
END;

-- Sealed confirmatory benchmarks ----------------------------------------------

CREATE TABLE research_benchmark_suites (
    suite_fingerprint TEXT PRIMARY KEY CHECK (
        length(suite_fingerprint) = 64
        AND suite_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    manifest_source_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    manifest_fingerprint TEXT NOT NULL CHECK (
        length(manifest_fingerprint) = 64
        AND manifest_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    case_count INTEGER NOT NULL CHECK (case_count >= 30),
    genre_function_count INTEGER NOT NULL CHECK (genre_function_count = 5),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_benchmark_suites_validate_insert
BEFORE INSERT ON research_benchmark_suites
WHEN NOT EXISTS (
    SELECT 1 FROM research_execution_records record
    WHERE record.record_fingerprint = NEW.record_fingerprint
      AND record.record_kind = 'benchmark_suite'
)
BEGIN
    SELECT RAISE(ABORT, 'benchmark suite lacks an exact suite record');
END;

CREATE TABLE research_benchmark_seals (
    seal_fingerprint TEXT PRIMARY KEY CHECK (
        length(seal_fingerprint) = 64
        AND seal_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    suite_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_benchmark_suites(suite_fingerprint) ON DELETE RESTRICT,
    benchmark_manifest_fingerprint TEXT NOT NULL CHECK (
        length(benchmark_manifest_fingerprint) = 64
        AND benchmark_manifest_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    assignment_matrix_fingerprint TEXT NOT NULL CHECK (
        length(assignment_matrix_fingerprint) = 64
        AND assignment_matrix_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    frontier_review_binding_fingerprint TEXT NOT NULL CHECK (
        length(frontier_review_binding_fingerprint) = 64
        AND frontier_review_binding_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    sealed_contender_count INTEGER NOT NULL CHECK (sealed_contender_count >= 2),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    sealed_at_ms INTEGER NOT NULL CHECK (sealed_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_benchmark_seals_validate_insert
BEFORE INSERT ON research_benchmark_seals
WHEN NOT EXISTS (
    SELECT 1 FROM research_execution_records record
    WHERE record.record_fingerprint = NEW.record_fingerprint
      AND record.record_kind = 'benchmark_seal'
)
BEGIN
    SELECT RAISE(ABORT, 'benchmark seal lacks an exact seal record');
END;

CREATE TABLE research_benchmark_contenders (
    seal_fingerprint TEXT NOT NULL
        REFERENCES research_benchmark_seals(seal_fingerprint) ON DELETE RESTRICT,
    contender_ordinal INTEGER NOT NULL CHECK (contender_ordinal >= 0),
    profile_fingerprint TEXT NOT NULL CHECK (
        length(profile_fingerprint) = 64
        AND profile_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    frozen_profile_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    PRIMARY KEY (seal_fingerprint, contender_ordinal),
    UNIQUE (seal_fingerprint, profile_fingerprint)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_benchmark_contenders_validate_insert
BEFORE INSERT ON research_benchmark_contenders
WHEN NOT EXISTS (
        SELECT 1 FROM blobs profile
        WHERE profile.blob_id = NEW.frozen_profile_blob_id AND profile.byte_len > 0
    )
    OR NEW.contender_ordinal >= (
        SELECT sealed_contender_count FROM research_benchmark_seals seal
        WHERE seal.seal_fingerprint = NEW.seal_fingerprint
    )
BEGIN
    SELECT RAISE(ABORT, 'benchmark contender is outside its seal or lacks a frozen profile');
END;

CREATE TABLE research_benchmark_runs (
    run_id TEXT PRIMARY KEY CHECK (
        length(run_id) = 64
        AND run_id NOT GLOB '*[^0-9a-f]*'
    ),
    seal_fingerprint TEXT NOT NULL
        REFERENCES research_benchmark_seals(seal_fingerprint) ON DELETE RESTRICT,
    run_class TEXT NOT NULL CHECK (run_class IN ('primary', 'supplemental')),
    assignment_fingerprint TEXT NOT NULL CHECK (
        length(assignment_fingerprint) = 64
        AND assignment_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    requested_model_fingerprint TEXT NOT NULL CHECK (
        length(requested_model_fingerprint) = 64
        AND requested_model_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    observed_model_fingerprint TEXT NOT NULL CHECK (
        length(observed_model_fingerprint) = 64
        AND observed_model_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    outcome TEXT NOT NULL CHECK (outcome IN ('left_win', 'right_win', 'tie', 'abstain')),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    completed_at_ms INTEGER NOT NULL CHECK (completed_at_ms > 0),
    CHECK (requested_model_fingerprint = observed_model_fingerprint)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_benchmark_runs_validate_insert
BEFORE INSERT ON research_benchmark_runs
WHEN NOT EXISTS (
    SELECT 1 FROM research_execution_records record
    WHERE record.record_fingerprint = NEW.record_fingerprint
      AND record.record_kind = 'benchmark_run'
)
BEGIN
    SELECT RAISE(ABORT, 'benchmark run lacks an exact run record');
END;

CREATE TABLE research_benchmark_journals (
    journal_fingerprint TEXT NOT NULL CHECK (
        length(journal_fingerprint) = 64
        AND journal_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    journal_namespace TEXT NOT NULL CHECK (journal_namespace IN (
        'diagnostic', 'qualification'
    )),
    seal_fingerprint TEXT NOT NULL
        REFERENCES research_benchmark_seals(seal_fingerprint) ON DELETE RESTRICT,
    chain_head TEXT NOT NULL CHECK (
        length(chain_head) = 64
        AND chain_head NOT GLOB '*[^0-9a-f]*'
    ),
    run_count INTEGER NOT NULL CHECK (run_count >= 0),
    primary_run_count INTEGER NOT NULL CHECK (primary_run_count >= 0),
    supplemental_run_count INTEGER NOT NULL CHECK (supplemental_run_count >= 0),
    record_fingerprint TEXT NOT NULL
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    PRIMARY KEY (journal_fingerprint, journal_namespace),
    CHECK (run_count = primary_run_count + supplemental_run_count)
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX research_benchmark_one_qualification_journal_per_seal
ON research_benchmark_journals(seal_fingerprint)
WHERE journal_namespace = 'qualification';

CREATE TABLE research_benchmark_journal_members (
    journal_fingerprint TEXT NOT NULL,
    journal_namespace TEXT NOT NULL CHECK (journal_namespace IN (
        'diagnostic', 'qualification'
    )),
    event_sequence INTEGER NOT NULL CHECK (event_sequence >= 0),
    event_hash TEXT NOT NULL CHECK (
        length(event_hash) = 64
        AND event_hash NOT GLOB '*[^0-9a-f]*'
    ),
    run_id TEXT NOT NULL REFERENCES research_benchmark_runs(run_id) ON DELETE RESTRICT,
    run_class TEXT NOT NULL CHECK (run_class IN ('primary', 'supplemental')),
    assignment_fingerprint TEXT NOT NULL CHECK (
        length(assignment_fingerprint) = 64
        AND assignment_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    PRIMARY KEY (journal_fingerprint, journal_namespace, event_sequence),
    UNIQUE (journal_fingerprint, journal_namespace, event_hash),
    UNIQUE (journal_fingerprint, journal_namespace, run_id),
    FOREIGN KEY (journal_fingerprint, journal_namespace)
        REFERENCES research_benchmark_journals(journal_fingerprint, journal_namespace)
        ON DELETE RESTRICT DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX research_benchmark_qualification_primary_assignment_once
ON research_benchmark_journal_members(
    journal_fingerprint, journal_namespace, assignment_fingerprint
)
WHERE journal_namespace = 'qualification' AND run_class = 'primary';

CREATE TRIGGER research_benchmark_journal_members_validate_insert
BEFORE INSERT ON research_benchmark_journal_members
WHEN NOT EXISTS (
    SELECT 1 FROM research_benchmark_runs run
    WHERE run.run_id = NEW.run_id
      AND run.run_class = NEW.run_class
      AND run.assignment_fingerprint = NEW.assignment_fingerprint
)
BEGIN
    SELECT RAISE(ABORT, 'benchmark journal member differs from its exact run');
END;

CREATE TRIGGER research_benchmark_journals_validate_insert
BEFORE INSERT ON research_benchmark_journals
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'benchmark_journal'
    )
    OR (
        SELECT COUNT(*) FROM research_benchmark_journal_members member
        WHERE member.journal_fingerprint = NEW.journal_fingerprint
          AND member.journal_namespace = NEW.journal_namespace
    ) != NEW.run_count
    OR (
        SELECT COUNT(*) FROM research_benchmark_journal_members member
        WHERE member.journal_fingerprint = NEW.journal_fingerprint
          AND member.journal_namespace = NEW.journal_namespace
          AND member.run_class = 'primary'
    ) != NEW.primary_run_count
    OR (
        SELECT COUNT(*) FROM research_benchmark_journal_members member
        WHERE member.journal_fingerprint = NEW.journal_fingerprint
          AND member.journal_namespace = NEW.journal_namespace
          AND member.run_class = 'supplemental'
    ) != NEW.supplemental_run_count
    OR EXISTS (
        SELECT 1
        FROM research_benchmark_journal_members member
        JOIN research_benchmark_runs run USING (run_id)
        WHERE member.journal_fingerprint = NEW.journal_fingerprint
          AND member.journal_namespace = NEW.journal_namespace
          AND run.seal_fingerprint != NEW.seal_fingerprint
    )
BEGIN
    SELECT RAISE(ABORT, 'benchmark journal lacks its exact ordered membership');
END;

CREATE TABLE research_human_label_packets (
    packet_fingerprint TEXT PRIMARY KEY CHECK (
        length(packet_fingerprint) = 64
        AND packet_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    seal_fingerprint TEXT NOT NULL
        REFERENCES research_benchmark_seals(seal_fingerprint) ON DELETE RESTRICT,
    label_schema_fingerprint TEXT NOT NULL CHECK (
        length(label_schema_fingerprint) = 64
        AND label_schema_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    encryption_algorithm TEXT NOT NULL CHECK (encryption_algorithm IN (
        'xchacha20poly1305_v1', 'age_x25519_v1'
    )),
    key_id_fingerprint TEXT NOT NULL CHECK (
        length(key_id_fingerprint) = 64
        AND key_id_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    nonce_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    ciphertext_blob_id TEXT NOT NULL REFERENCES blobs(blob_id) ON DELETE RESTRICT,
    associated_data_fingerprint TEXT NOT NULL CHECK (
        length(associated_data_fingerprint) = 64
        AND associated_data_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_human_label_packets_validate_insert
BEFORE INSERT ON research_human_label_packets
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'human_label_packet'
    )
    OR NOT EXISTS (
        SELECT 1 FROM blobs nonce, blobs ciphertext
        WHERE nonce.blob_id = NEW.nonce_blob_id
          AND (
              (NEW.encryption_algorithm = 'xchacha20poly1305_v1' AND nonce.byte_len = 24)
              OR (NEW.encryption_algorithm = 'age_x25519_v1' AND nonce.byte_len = 0)
          )
          AND ciphertext.blob_id = NEW.ciphertext_blob_id
          AND (
              (NEW.encryption_algorithm = 'xchacha20poly1305_v1' AND ciphertext.byte_len >= 16)
              OR (NEW.encryption_algorithm = 'age_x25519_v1' AND ciphertext.byte_len > 0)
          )
    )
BEGIN
    SELECT RAISE(ABORT, 'human label packet is not valid encrypted evidence');
END;

CREATE TABLE research_benchmark_results (
    result_fingerprint TEXT PRIMARY KEY CHECK (
        length(result_fingerprint) = 64
        AND result_fingerprint NOT GLOB '*[^0-9a-f]*'
    ),
    seal_fingerprint TEXT NOT NULL
        REFERENCES research_benchmark_seals(seal_fingerprint) ON DELETE RESTRICT,
    result_status TEXT NOT NULL CHECK (result_status IN (
        'frontier_reviewed_provisional', 'human_confirmed', 'no_qualifying_profile'
    )),
    primary_run_count INTEGER NOT NULL CHECK (primary_run_count > 0),
    supplemental_run_count INTEGER NOT NULL CHECK (supplemental_run_count >= 0),
    human_label_packet_fingerprint TEXT
        REFERENCES research_human_label_packets(packet_fingerprint) ON DELETE RESTRICT,
    journal_fingerprint TEXT NOT NULL
        CHECK (
            length(journal_fingerprint) = 64
            AND journal_fingerprint NOT GLOB '*[^0-9a-f]*'
        ),
    journal_namespace TEXT NOT NULL CHECK (journal_namespace = 'qualification'),
    record_fingerprint TEXT NOT NULL UNIQUE
        REFERENCES research_execution_records(record_fingerprint) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
    CHECK (
        (result_status = 'human_confirmed' AND human_label_packet_fingerprint IS NOT NULL)
        OR (result_status != 'human_confirmed' AND human_label_packet_fingerprint IS NULL)
    ),
    FOREIGN KEY (journal_fingerprint, journal_namespace)
        REFERENCES research_benchmark_journals(journal_fingerprint, journal_namespace)
        ON DELETE RESTRICT
) STRICT, WITHOUT ROWID;

CREATE TRIGGER research_benchmark_results_validate_insert
BEFORE INSERT ON research_benchmark_results
WHEN NOT EXISTS (
        SELECT 1 FROM research_execution_records record
        WHERE record.record_fingerprint = NEW.record_fingerprint
          AND record.record_kind = 'benchmark_result'
    )
    OR (
        SELECT COUNT(*) FROM research_benchmark_journal_members member
        WHERE member.journal_fingerprint = NEW.journal_fingerprint
          AND member.journal_namespace = NEW.journal_namespace
          AND member.run_class = 'primary'
    ) != NEW.primary_run_count
    OR (
        SELECT COUNT(*) FROM research_benchmark_journal_members member
        WHERE member.journal_fingerprint = NEW.journal_fingerprint
          AND member.journal_namespace = NEW.journal_namespace
          AND member.run_class = 'supplemental'
    ) != NEW.supplemental_run_count
    OR (
        NEW.human_label_packet_fingerprint IS NOT NULL
        AND NOT EXISTS (
            SELECT 1 FROM research_human_label_packets packet
            WHERE packet.packet_fingerprint = NEW.human_label_packet_fingerprint
              AND packet.seal_fingerprint = NEW.seal_fingerprint
        )
    )
    OR NOT EXISTS (
        SELECT 1 FROM research_benchmark_journals journal
        WHERE journal.journal_fingerprint = NEW.journal_fingerprint
          AND journal.journal_namespace = NEW.journal_namespace
          AND journal.seal_fingerprint = NEW.seal_fingerprint
          AND journal.run_count = NEW.primary_run_count + NEW.supplemental_run_count
    )
BEGIN
    SELECT RAISE(ABORT, 'benchmark result does not cover its exact validated runs');
END;

-- Every semantic row is immutable.  State changes are represented only by a
-- new event, attempt, snapshot, run, or result row.
CREATE TRIGGER research_execution_records_immutable_update BEFORE UPDATE ON research_execution_records BEGIN SELECT RAISE(ABORT, 'research execution records are immutable'); END;
CREATE TRIGGER research_execution_records_immutable_delete BEFORE DELETE ON research_execution_records BEGIN SELECT RAISE(ABORT, 'research execution records are immutable'); END;
CREATE TRIGGER research_campaigns_immutable_update BEFORE UPDATE ON research_campaigns BEGIN SELECT RAISE(ABORT, 'research campaigns are immutable'); END;
CREATE TRIGGER research_campaigns_immutable_delete BEFORE DELETE ON research_campaigns BEGIN SELECT RAISE(ABORT, 'research campaigns are immutable'); END;
CREATE TRIGGER research_trial_specs_immutable_update BEFORE UPDATE ON research_trial_specs BEGIN SELECT RAISE(ABORT, 'research trial specifications are immutable'); END;
CREATE TRIGGER research_trial_specs_immutable_delete BEFORE DELETE ON research_trial_specs BEGIN SELECT RAISE(ABORT, 'research trial specifications are immutable'); END;
CREATE TRIGGER research_trial_runs_immutable_update BEFORE UPDATE ON research_trial_runs BEGIN SELECT RAISE(ABORT, 'research trial runs are immutable'); END;
CREATE TRIGGER research_trial_runs_immutable_delete BEFORE DELETE ON research_trial_runs BEGIN SELECT RAISE(ABORT, 'research trial runs are immutable'); END;
CREATE TRIGGER research_campaign_stage_specs_immutable_update BEFORE UPDATE ON research_campaign_stage_specs BEGIN SELECT RAISE(ABORT, 'research stage specifications are immutable'); END;
CREATE TRIGGER research_campaign_stage_specs_immutable_delete BEFORE DELETE ON research_campaign_stage_specs BEGIN SELECT RAISE(ABORT, 'research stage specifications are immutable'); END;
CREATE TRIGGER research_stage_dependencies_immutable_update BEFORE UPDATE ON research_campaign_stage_dependencies BEGIN SELECT RAISE(ABORT, 'research stage dependencies are immutable'); END;
CREATE TRIGGER research_stage_dependencies_immutable_delete BEFORE DELETE ON research_campaign_stage_dependencies BEGIN SELECT RAISE(ABORT, 'research stage dependencies are immutable'); END;
CREATE TRIGGER research_trial_dependencies_immutable_update BEFORE UPDATE ON research_campaign_trial_dependencies BEGIN SELECT RAISE(ABORT, 'research trial dependencies are immutable'); END;
CREATE TRIGGER research_trial_dependencies_immutable_delete BEFORE DELETE ON research_campaign_trial_dependencies BEGIN SELECT RAISE(ABORT, 'research trial dependencies are immutable'); END;
CREATE TRIGGER research_stage_attempts_immutable_update BEFORE UPDATE ON research_campaign_stage_attempts BEGIN SELECT RAISE(ABORT, 'research stage attempts are immutable'); END;
CREATE TRIGGER research_stage_attempts_immutable_delete BEFORE DELETE ON research_campaign_stage_attempts BEGIN SELECT RAISE(ABORT, 'research stage attempts are immutable'); END;
CREATE TRIGGER research_campaign_trial_attempts_immutable_update BEFORE UPDATE ON research_campaign_trial_attempts BEGIN SELECT RAISE(ABORT, 'research campaign trial attempts are immutable'); END;
CREATE TRIGGER research_campaign_trial_attempts_immutable_delete BEFORE DELETE ON research_campaign_trial_attempts BEGIN SELECT RAISE(ABORT, 'research campaign trial attempts are immutable'); END;
CREATE TRIGGER research_campaign_events_immutable_update BEFORE UPDATE ON research_campaign_events BEGIN SELECT RAISE(ABORT, 'research campaign events are immutable'); END;
CREATE TRIGGER research_campaign_events_immutable_delete BEFORE DELETE ON research_campaign_events BEGIN SELECT RAISE(ABORT, 'research campaign events are immutable'); END;
CREATE TRIGGER research_trial_events_immutable_update BEFORE UPDATE ON research_trial_events BEGIN SELECT RAISE(ABORT, 'research trial events are immutable'); END;
CREATE TRIGGER research_trial_events_immutable_delete BEFORE DELETE ON research_trial_events BEGIN SELECT RAISE(ABORT, 'research trial events are immutable'); END;
CREATE TRIGGER research_budget_reservations_immutable_update BEFORE UPDATE ON research_campaign_budget_reservations BEGIN SELECT RAISE(ABORT, 'research budget reservations are immutable'); END;
CREATE TRIGGER research_budget_reservations_immutable_delete BEFORE DELETE ON research_campaign_budget_reservations BEGIN SELECT RAISE(ABORT, 'research budget reservations are immutable'); END;
CREATE TRIGGER research_budget_charges_immutable_update BEFORE UPDATE ON research_campaign_budget_charges BEGIN SELECT RAISE(ABORT, 'research budget charges are immutable'); END;
CREATE TRIGGER research_budget_charges_immutable_delete BEFORE DELETE ON research_campaign_budget_charges BEGIN SELECT RAISE(ABORT, 'research budget charges are immutable'); END;
CREATE TRIGGER research_search_decisions_immutable_update BEFORE UPDATE ON research_campaign_search_decisions BEGIN SELECT RAISE(ABORT, 'research search decisions are immutable'); END;
CREATE TRIGGER research_search_decisions_immutable_delete BEFORE DELETE ON research_campaign_search_decisions BEGIN SELECT RAISE(ABORT, 'research search decisions are immutable'); END;
CREATE TRIGGER research_trial_budget_reservations_immutable_update BEFORE UPDATE ON research_campaign_trial_budget_reservations BEGIN SELECT RAISE(ABORT, 'research campaign trial budget reservations are immutable'); END;
CREATE TRIGGER research_trial_budget_reservations_immutable_delete BEFORE DELETE ON research_campaign_trial_budget_reservations BEGIN SELECT RAISE(ABORT, 'research campaign trial budget reservations are immutable'); END;
CREATE TRIGGER research_trial_budget_charges_immutable_update BEFORE UPDATE ON research_campaign_trial_budget_charges BEGIN SELECT RAISE(ABORT, 'research campaign trial budget charges are immutable'); END;
CREATE TRIGGER research_trial_budget_charges_immutable_delete BEFORE DELETE ON research_campaign_trial_budget_charges BEGIN SELECT RAISE(ABORT, 'research campaign trial budget charges are immutable'); END;
CREATE TRIGGER research_story_graphs_immutable_update BEFORE UPDATE ON research_story_graphs BEGIN SELECT RAISE(ABORT, 'research story graphs are immutable'); END;
CREATE TRIGGER research_story_graphs_immutable_delete BEFORE DELETE ON research_story_graphs BEGIN SELECT RAISE(ABORT, 'research story graphs are immutable'); END;
CREATE TRIGGER research_story_states_immutable_update BEFORE UPDATE ON research_story_states BEGIN SELECT RAISE(ABORT, 'research story states are immutable'); END;
CREATE TRIGGER research_story_states_immutable_delete BEFORE DELETE ON research_story_states BEGIN SELECT RAISE(ABORT, 'research story states are immutable'); END;
CREATE TRIGGER research_prompt_masks_immutable_update BEFORE UPDATE ON research_prompt_masks BEGIN SELECT RAISE(ABORT, 'research prompt masks are immutable'); END;
CREATE TRIGGER research_prompt_masks_immutable_delete BEFORE DELETE ON research_prompt_masks BEGIN SELECT RAISE(ABORT, 'research prompt masks are immutable'); END;
CREATE TRIGGER research_backtranslation_proposals_immutable_update BEFORE UPDATE ON research_backtranslation_proposals BEGIN SELECT RAISE(ABORT, 'research backtranslation proposals are immutable'); END;
CREATE TRIGGER research_backtranslation_proposals_immutable_delete BEFORE DELETE ON research_backtranslation_proposals BEGIN SELECT RAISE(ABORT, 'research backtranslation proposals are immutable'); END;
CREATE TRIGGER research_backtranslation_auditions_immutable_update BEFORE UPDATE ON research_backtranslation_auditions BEGIN SELECT RAISE(ABORT, 'research backtranslation auditions are immutable'); END;
CREATE TRIGGER research_backtranslation_auditions_immutable_delete BEFORE DELETE ON research_backtranslation_auditions BEGIN SELECT RAISE(ABORT, 'research backtranslation auditions are immutable'); END;
CREATE TRIGGER research_backtranslation_audition_batches_immutable_update BEFORE UPDATE ON research_backtranslation_audition_batches BEGIN SELECT RAISE(ABORT, 'research backtranslation audition batches are immutable'); END;
CREATE TRIGGER research_backtranslation_audition_batches_immutable_delete BEFORE DELETE ON research_backtranslation_audition_batches BEGIN SELECT RAISE(ABORT, 'research backtranslation audition batches are immutable'); END;
CREATE TRIGGER research_backtranslation_audition_evaluator_receipts_immutable_update BEFORE UPDATE ON research_backtranslation_audition_evaluator_receipts BEGIN SELECT RAISE(ABORT, 'research backtranslation audition evaluator receipts are immutable'); END;
CREATE TRIGGER research_backtranslation_audition_evaluator_receipts_immutable_delete BEFORE DELETE ON research_backtranslation_audition_evaluator_receipts BEGIN SELECT RAISE(ABORT, 'research backtranslation audition evaluator receipts are immutable'); END;
CREATE TRIGGER research_backtranslation_acceptances_immutable_update BEFORE UPDATE ON research_backtranslation_acceptances BEGIN SELECT RAISE(ABORT, 'research backtranslation acceptances are immutable'); END;
CREATE TRIGGER research_backtranslation_acceptances_immutable_delete BEFORE DELETE ON research_backtranslation_acceptances BEGIN SELECT RAISE(ABORT, 'research backtranslation acceptances are immutable'); END;
CREATE TRIGGER research_evaluation_tasks_immutable_update BEFORE UPDATE ON research_evaluation_tasks BEGIN SELECT RAISE(ABORT, 'research evaluation tasks are immutable'); END;
CREATE TRIGGER research_evaluation_tasks_immutable_delete BEFORE DELETE ON research_evaluation_tasks BEGIN SELECT RAISE(ABORT, 'research evaluation tasks are immutable'); END;
CREATE TRIGGER research_evaluation_receipts_immutable_update BEFORE UPDATE ON research_evaluation_receipts BEGIN SELECT RAISE(ABORT, 'research evaluation receipts are immutable'); END;
CREATE TRIGGER research_evaluation_receipts_immutable_delete BEFORE DELETE ON research_evaluation_receipts BEGIN SELECT RAISE(ABORT, 'research evaluation receipts are immutable'); END;
CREATE TRIGGER research_evidence_spans_immutable_update BEFORE UPDATE ON research_evidence_spans BEGIN SELECT RAISE(ABORT, 'research evidence spans are immutable'); END;
CREATE TRIGGER research_evidence_spans_immutable_delete BEFORE DELETE ON research_evidence_spans BEGIN SELECT RAISE(ABORT, 'research evidence spans are immutable'); END;
CREATE TRIGGER research_pairwise_assignments_immutable_update BEFORE UPDATE ON research_pairwise_assignments BEGIN SELECT RAISE(ABORT, 'research pairwise assignments are immutable'); END;
CREATE TRIGGER research_pairwise_assignments_immutable_delete BEFORE DELETE ON research_pairwise_assignments BEGIN SELECT RAISE(ABORT, 'research pairwise assignments are immutable'); END;
CREATE TRIGGER research_score_vectors_immutable_update BEFORE UPDATE ON research_score_vectors BEGIN SELECT RAISE(ABORT, 'research score vectors are immutable'); END;
CREATE TRIGGER research_score_vectors_immutable_delete BEFORE DELETE ON research_score_vectors BEGIN SELECT RAISE(ABORT, 'research score vectors are immutable'); END;
CREATE TRIGGER research_candidate_descriptors_immutable_update BEFORE UPDATE ON research_candidate_descriptors BEGIN SELECT RAISE(ABORT, 'research candidate descriptors are immutable'); END;
CREATE TRIGGER research_candidate_descriptors_immutable_delete BEFORE DELETE ON research_candidate_descriptors BEGIN SELECT RAISE(ABORT, 'research candidate descriptors are immutable'); END;
CREATE TRIGGER research_preference_labels_immutable_update BEFORE UPDATE ON research_preference_labels BEGIN SELECT RAISE(ABORT, 'research preference labels are immutable'); END;
CREATE TRIGGER research_preference_labels_immutable_delete BEFORE DELETE ON research_preference_labels BEGIN SELECT RAISE(ABORT, 'research preference labels are immutable'); END;
CREATE TRIGGER research_archive_snapshots_immutable_update BEFORE UPDATE ON research_archive_snapshots BEGIN SELECT RAISE(ABORT, 'research archive snapshots are immutable'); END;
CREATE TRIGGER research_archive_snapshots_immutable_delete BEFORE DELETE ON research_archive_snapshots BEGIN SELECT RAISE(ABORT, 'research archive snapshots are immutable'); END;
CREATE TRIGGER research_benchmark_suites_immutable_update BEFORE UPDATE ON research_benchmark_suites BEGIN SELECT RAISE(ABORT, 'research benchmark suites are immutable'); END;
CREATE TRIGGER research_benchmark_suites_immutable_delete BEFORE DELETE ON research_benchmark_suites BEGIN SELECT RAISE(ABORT, 'research benchmark suites are immutable'); END;
CREATE TRIGGER research_benchmark_seals_immutable_update BEFORE UPDATE ON research_benchmark_seals BEGIN SELECT RAISE(ABORT, 'research benchmark seals are immutable'); END;
CREATE TRIGGER research_benchmark_seals_immutable_delete BEFORE DELETE ON research_benchmark_seals BEGIN SELECT RAISE(ABORT, 'research benchmark seals are immutable'); END;
CREATE TRIGGER research_benchmark_contenders_immutable_update BEFORE UPDATE ON research_benchmark_contenders BEGIN SELECT RAISE(ABORT, 'research benchmark contenders are immutable'); END;
CREATE TRIGGER research_benchmark_contenders_immutable_delete BEFORE DELETE ON research_benchmark_contenders BEGIN SELECT RAISE(ABORT, 'research benchmark contenders are immutable'); END;
CREATE TRIGGER research_benchmark_runs_immutable_update BEFORE UPDATE ON research_benchmark_runs BEGIN SELECT RAISE(ABORT, 'research benchmark runs are immutable'); END;
CREATE TRIGGER research_benchmark_runs_immutable_delete BEFORE DELETE ON research_benchmark_runs BEGIN SELECT RAISE(ABORT, 'research benchmark runs are immutable'); END;
CREATE TRIGGER research_benchmark_journal_members_immutable_update BEFORE UPDATE ON research_benchmark_journal_members BEGIN SELECT RAISE(ABORT, 'research benchmark journal members are immutable'); END;
CREATE TRIGGER research_benchmark_journal_members_immutable_delete BEFORE DELETE ON research_benchmark_journal_members BEGIN SELECT RAISE(ABORT, 'research benchmark journal members are immutable'); END;
CREATE TRIGGER research_benchmark_journals_immutable_update BEFORE UPDATE ON research_benchmark_journals BEGIN SELECT RAISE(ABORT, 'research benchmark journals are immutable'); END;
CREATE TRIGGER research_benchmark_journals_immutable_delete BEFORE DELETE ON research_benchmark_journals BEGIN SELECT RAISE(ABORT, 'research benchmark journals are immutable'); END;
CREATE TRIGGER research_human_label_packets_immutable_update BEFORE UPDATE ON research_human_label_packets BEGIN SELECT RAISE(ABORT, 'research human label packets are immutable'); END;
CREATE TRIGGER research_human_label_packets_immutable_delete BEFORE DELETE ON research_human_label_packets BEGIN SELECT RAISE(ABORT, 'research human label packets are immutable'); END;
CREATE TRIGGER research_benchmark_results_immutable_update BEFORE UPDATE ON research_benchmark_results BEGIN SELECT RAISE(ABORT, 'research benchmark results are immutable'); END;
CREATE TRIGGER research_benchmark_results_immutable_delete BEFORE DELETE ON research_benchmark_results BEGIN SELECT RAISE(ABORT, 'research benchmark results are immutable'); END;
