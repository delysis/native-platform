-- The branch shelf is a hot read path. Materialize the immutable weave command
-- authority once so branch refreshes never scan and parse every command receipt.
CREATE TABLE generation_weave_commands (
    run_id TEXT PRIMARY KEY
        REFERENCES generation_runs(run_id) ON DELETE RESTRICT,
    command_id TEXT NOT NULL
        REFERENCES command_receipts(command_id) ON DELETE RESTRICT,
    run_artifact_id TEXT NOT NULL UNIQUE
        REFERENCES artifacts(artifact_id) ON DELETE RESTRICT
) STRICT, WITHOUT ROWID;

-- Backfill every historical weave result, but only after proving that the row
-- columns, embedded receipt, generation run, artifact, and generating operation
-- all describe the same authority. Candidate receipts are selected broadly so a
-- malformed historical weave receipt cannot disappear from the migration. The
-- destination NOT NULL constraints deliberately abort migration for malformed,
-- incomplete, duplicate, or conflicting authority. Historical loom-store APIs
-- could write up to loom-host's hard 4,096-branch ceiling; the backfill accepts
-- that bounded shape so a valid v12 store remains migratable. The insert trigger
-- below separately enforces the shipped product's current 64-branch authority.
WITH candidate_receipts AS (
    SELECT
        receipt.command_id,
        receipt.command_kind AS receipt_command_kind,
        request.command_kind AS request_command_kind,
        json_valid(receipt.receipt_json) AS receipt_is_valid,
        CASE
            WHEN json_valid(receipt.receipt_json) THEN receipt.receipt_json
            ELSE '{}'
        END AS receipt_json
    FROM command_receipts receipt
    LEFT JOIN command_requests request ON request.command_id = receipt.command_id
    WHERE receipt.command_kind = 'weave'
       OR request.command_kind = 'weave'
       OR json_extract(
            CASE
                WHEN json_valid(receipt.receipt_json) THEN receipt.receipt_json
                ELSE '{}'
            END,
            '$.command'
       ) = 'weave'
), receipt_members AS (
    SELECT
        receipt.*,
        artifact_member.key AS member_index,
        artifact_member.value AS run_artifact_id,
        artifact_member.type AS run_artifact_type,
        operation_member.value AS operation_id,
        operation_member.type AS operation_id_type
    FROM candidate_receipts receipt
    LEFT JOIN json_each(receipt.receipt_json, '$.resulting_artifact_ids') artifact_member
        ON TRUE
    LEFT JOIN json_each(receipt.receipt_json, '$.resulting_operation_ids') operation_member
        ON operation_member.key = artifact_member.key
), authoritative_members AS (
    SELECT
        member.command_id,
        member.run_artifact_id,
        run.run_id,
        CASE
            WHEN member.receipt_is_valid = 1
             AND member.receipt_command_kind = 'weave'
             -- Schema-v2 through pre-family Loom wrote one-run weave receipts
             -- without command_requests. Migration 0006 marks exactly those
             -- legacy runs with a NULL indexed seed. Preserve that historical
             -- shape without admitting a request-less modern family.
             AND (
                 member.request_command_kind = 'weave'
                 OR (
                     member.request_command_kind IS NULL
                     AND json_array_length(
                         member.receipt_json,
                         '$.resulting_artifact_ids'
                     ) = 1
                     AND run_index.seed_decimal IS NULL
                 )
             )
             AND json_type(member.receipt_json, '$.command_id') = 'text'
             AND json_extract(member.receipt_json, '$.command_id') = member.command_id
             AND json_type(member.receipt_json, '$.command') = 'text'
             AND json_extract(member.receipt_json, '$.command') = 'weave'
             AND json_type(member.receipt_json, '$.source_revision_id') = 'text'
             AND json_extract(member.receipt_json, '$.source_revision_id') = run.source_revision_id
             AND json_type(member.receipt_json, '$.resulting_artifact_ids') = 'array'
             AND json_array_length(member.receipt_json, '$.resulting_artifact_ids') BETWEEN 1 AND 4096
             AND json_type(member.receipt_json, '$.resulting_operation_ids') = 'array'
             AND json_array_length(member.receipt_json, '$.resulting_operation_ids')
                 = json_array_length(member.receipt_json, '$.resulting_artifact_ids')
             AND json_type(member.receipt_json, '$.resulting_revision_ids') = 'array'
             AND json_array_length(member.receipt_json, '$.resulting_revision_ids') = 0
             AND member.member_index IS NOT NULL
             AND member.run_artifact_type = 'text'
             AND member.operation_id_type = 'text'
             AND run.run_id IS NOT NULL
             AND run_artifact.artifact_kind = 'generation_run'
             AND json_valid(run_artifact.metadata_json)
             AND json_type(run_artifact.metadata_json, '$.run_id') = 'text'
             AND json_extract(run_artifact.metadata_json, '$.run_id') = run.run_id
             AND generation_operation.operation_kind = 'generate'
             AND json_valid(generation_operation.metadata_json)
             AND json_type(generation_operation.metadata_json, '$.run_id') = 'text'
             AND json_extract(generation_operation.metadata_json, '$.run_id') = run.run_id
             AND operation_output.artifact_id = member.run_artifact_id
             AND NOT EXISTS (
                 SELECT 1
                 FROM operation_outputs other_output
                 WHERE other_output.operation_id = member.operation_id
                   AND (
                       other_output.position <> 0
                       OR other_output.artifact_id <> member.run_artifact_id
                   )
             )
            THEN 1
            ELSE 0
        END AS authoritative
    FROM receipt_members member
    LEFT JOIN generation_runs run ON run.run_artifact_id = member.run_artifact_id
    LEFT JOIN generation_run_index run_index ON run_index.run_id = run.run_id
    LEFT JOIN artifacts run_artifact ON run_artifact.artifact_id = member.run_artifact_id
    LEFT JOIN operations generation_operation ON generation_operation.operation_id = member.operation_id
    LEFT JOIN operation_outputs operation_output
        ON operation_output.operation_id = member.operation_id
       AND operation_output.position = 0
)
INSERT INTO generation_weave_commands(run_id, command_id, run_artifact_id)
SELECT
    CASE WHEN authoritative = 1 THEN run_id ELSE NULL END,
    command_id,
    CASE WHEN authoritative = 1 THEN CAST(run_artifact_id AS TEXT) ELSE NULL END
FROM authoritative_members;

-- A historical generation run without exactly one validated weave receipt is
-- also invalid. Force a constraint failure instead of silently migrating it.
INSERT INTO generation_weave_commands(run_id, command_id, run_artifact_id)
SELECT NULL, NULL, NULL
WHERE EXISTS (
    SELECT 1
    FROM generation_runs run
    LEFT JOIN generation_weave_commands weave ON weave.run_id = run.run_id
    WHERE weave.run_id IS NULL
);

CREATE INDEX generation_weave_commands_command_id
ON generation_weave_commands(command_id);

CREATE TRIGGER generation_weave_commands_validate_insert
BEFORE INSERT ON generation_weave_commands
WHEN NOT EXISTS (
    SELECT 1
    FROM generation_runs run
    JOIN artifacts run_artifact ON run_artifact.artifact_id = NEW.run_artifact_id
    JOIN command_requests request ON request.command_id = NEW.command_id
    JOIN command_receipts receipt ON receipt.command_id = request.command_id
    CROSS JOIN json_each(
        CASE
            WHEN json_valid(receipt.receipt_json) THEN receipt.receipt_json
            ELSE '{}'
        END,
        '$.resulting_artifact_ids'
    ) artifact_member
    JOIN json_each(
        CASE
            WHEN json_valid(receipt.receipt_json) THEN receipt.receipt_json
            ELSE '{}'
        END,
        '$.resulting_operation_ids'
    ) operation_member ON operation_member.key = artifact_member.key
    JOIN operations generation_operation ON generation_operation.operation_id = operation_member.value
    JOIN operation_outputs operation_output
      ON operation_output.operation_id = operation_member.value
     AND operation_output.position = 0
     AND operation_output.artifact_id = artifact_member.value
    WHERE run.run_id = NEW.run_id
      AND run.run_artifact_id = NEW.run_artifact_id
      AND artifact_member.value = NEW.run_artifact_id
      AND artifact_member.type = 'text'
      AND operation_member.type = 'text'
      AND run_artifact.artifact_kind = 'generation_run'
      AND json_valid(run_artifact.metadata_json)
      AND json_type(run_artifact.metadata_json, '$.run_id') = 'text'
      AND json_extract(run_artifact.metadata_json, '$.run_id') = run.run_id
      AND generation_operation.operation_kind = 'generate'
      AND json_valid(generation_operation.metadata_json)
      AND json_type(generation_operation.metadata_json, '$.run_id') = 'text'
      AND json_extract(generation_operation.metadata_json, '$.run_id') = run.run_id
      AND NOT EXISTS (
          SELECT 1
          FROM operation_outputs other_output
          WHERE other_output.operation_id = operation_member.value
            AND (
                other_output.position <> 0
                OR other_output.artifact_id <> artifact_member.value
            )
      )
      AND request.command_kind = 'weave'
      AND receipt.command_kind = 'weave'
      AND json_valid(receipt.receipt_json)
      AND json_type(receipt.receipt_json, '$.command_id') = 'text'
      AND json_extract(receipt.receipt_json, '$.command_id') = NEW.command_id
      AND json_type(receipt.receipt_json, '$.command') = 'text'
      AND json_extract(receipt.receipt_json, '$.command') = 'weave'
      AND json_type(receipt.receipt_json, '$.source_revision_id') = 'text'
      AND json_extract(receipt.receipt_json, '$.source_revision_id') = run.source_revision_id
      AND json_type(receipt.receipt_json, '$.resulting_artifact_ids') = 'array'
      AND json_array_length(receipt.receipt_json, '$.resulting_artifact_ids') BETWEEN 1 AND 64
      AND json_type(receipt.receipt_json, '$.resulting_operation_ids') = 'array'
      AND json_array_length(receipt.receipt_json, '$.resulting_operation_ids')
          = json_array_length(receipt.receipt_json, '$.resulting_artifact_ids')
      AND json_type(receipt.receipt_json, '$.resulting_revision_ids') = 'array'
      AND json_array_length(receipt.receipt_json, '$.resulting_revision_ids') = 0
)
BEGIN
    SELECT RAISE(ABORT, 'generation weave command does not match its run and receipt');
END;

CREATE TRIGGER generation_weave_commands_are_immutable_update
BEFORE UPDATE ON generation_weave_commands
BEGIN
    SELECT RAISE(ABORT, 'generation weave commands are immutable');
END;

CREATE TRIGGER generation_weave_commands_are_immutable_delete
BEFORE DELETE ON generation_weave_commands
BEGIN
    SELECT RAISE(ABORT, 'generation weave commands are immutable');
END;
