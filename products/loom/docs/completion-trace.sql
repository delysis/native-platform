-- Read-only diagnosis, not product acceptance. Run with sqlite3 -readonly.
-- Includes the first 32 runs of an isolated smoke, in durable admission order.
WITH selected AS (
  SELECT r.*, i.sequence AS admission_sequence, i.seed_decimal, i.model_identifier
  FROM generation_runs r
  LEFT JOIN generation_run_index i ON i.run_id=r.run_id
  ORDER BY COALESCE(i.sequence, 0), r.created_at_ms, r.run_id LIMIT 32
), run_rows AS (
  SELECT r.*, w.command_id, q.request_fingerprint, q.created_at_ms AS request_created_at_ms,
    c.receipt_json, t.status AS terminal_status, t.error AS terminal_error,
    t.created_at_ms AS terminal_created_at_ms, g.candidate_id, g.output_blob_id,
    b.byte_len AS output_byte_len
  FROM selected r
  LEFT JOIN generation_weave_commands w ON w.run_id=r.run_id
  LEFT JOIN command_requests q ON q.command_id=w.command_id
  LEFT JOIN command_receipts c ON c.command_id=w.command_id
  LEFT JOIN generation_terminals t ON t.run_id=r.run_id
  LEFT JOIN generation_candidates g ON g.run_id=r.run_id
  LEFT JOIN blobs b ON b.blob_id=g.output_blob_id
)
SELECT json_object(
  'evidence_kind', 'diagnosis_only_not_acceptance',
  'total_runs', (SELECT count(*) FROM generation_runs),
  'runs_truncated', json(CASE WHEN (SELECT count(*) FROM generation_runs)>32 THEN 'true' ELSE 'false' END),
  'runs', json((SELECT json_group_array(json_object(
    'run_id', r.run_id, 'branch_id', r.branch_id, 'admission_sequence', r.admission_sequence,
    'command_id', r.command_id, 'request_fingerprint', r.request_fingerprint,
    'document_id', r.document_id, 'source_revision_id', r.source_revision_id,
    'source_blob_id', r.source_blob_id, 'target_start_byte', r.target_start_byte,
    'target_end_byte', r.target_end_byte, 'model_identifier', r.model_identifier,
    'seed_decimal', r.seed_decimal, 'created_at_ms', r.created_at_ms,
    'request_created_at_ms', r.request_created_at_ms,
    'terminal_status', r.terminal_status, 'terminal_error', r.terminal_error,
    'terminal_created_at_ms', r.terminal_created_at_ms,
    'candidate_id', r.candidate_id, 'output_blob_id', r.output_blob_id,
    'output_byte_len', r.output_byte_len, 'command_receipt_raw', r.receipt_json,
    'event_count', (SELECT count(*) FROM generation_events e WHERE e.run_id=r.run_id),
    'events_truncated', json(CASE WHEN (SELECT count(*) FROM generation_events e WHERE e.run_id=r.run_id)>4096 THEN 'true' ELSE 'false' END),
    'events', json((SELECT json_group_array(json_object(
      'event_id', e.event_id, 'sequence', e.sequence, 'kind', e.event_kind,
      'payload_raw', e.payload_json, 'is_terminal', e.is_terminal, 'created_at_ms', e.created_at_ms
    )) FROM (SELECT * FROM generation_events WHERE run_id=r.run_id ORDER BY sequence LIMIT 4096) e))
  )) FROM run_rows r))
) AS completion_trace_json;
