export type DocumentKind = 'prose' | 'verse' | 'hybrid';
export type EditorMode = 'visual' | 'source' | 'split';
export type SaveState = 'clean' | 'dirty' | 'saving' | 'saved' | 'uncertain' | 'error';

export interface DocumentSummary {
  document_id: string;
  relative_path: string;
  title: string;
  kind: DocumentKind;
  revision_id: string | null;
  active_blob_id: string | null;
  word_count: number;
  externally_modified: boolean;
}

export interface ProjectSnapshot {
  project_id: string;
  session_id: string;
  title: string;
  root: string;
  schema_version: number;
  documents: DocumentSummary[];
  pending_recovery: number;
}

export interface ProjectCloseReceipt {
  command_id: string;
  project_id: string;
  session_id: string;
  closed_at_unix_ms: number;
}

export interface OpenDocument {
  summary: DocumentSummary;
  visible_blob_id: string;
  text: string;
  transient_draft: TransientDraftSnapshot | null;
}

export interface TransientDraftSnapshot {
  document_id: string;
  source_revision_id: string;
  blob_id: string;
  /** Decimal u64, preserved as text across the JS boundary. */
  version: string;
  kind: DocumentKind;
  text: string;
  updated_at_unix_ms: number;
  replayed: boolean;
}

export type TransientDraftWriteReceipt = Omit<TransientDraftSnapshot, 'text'>;

export type VisibleProjectionState =
  | { status: 'applied' }
  | {
      status: 'pending_conflict';
      outbox_id: number;
      relative_path: string;
    }
  | {
      status: 'pending_retry';
      outbox_id: number;
      relative_path: string;
      error: string;
    };

export interface CommandReceipt {
  command_id: string;
  command_kind: string;
  project_id: string;
  schema_version: number;
  source_revision_id: string | null;
  result_revision_id: string | null;
  result_blob_id: string | null;
  request_fingerprint: string | null;
  replayed: boolean;
  visible_projection: VisibleProjectionState | null;
  artifact_ids: string[];
  completed_at_unix_ms: number;
}

export interface RecoveryReport {
  recovered: number;
  conflicts: string[];
}

export interface ModelCapabilitySummary {
  model_id: string;
  display_name: string;
  local: boolean;
  loaded: boolean;
  chat: boolean;
  completion: boolean;
  fill_in_middle: boolean;
  output_tokens: boolean;
  logprobs: boolean;
  model_path: string;
  file_bytes: number;
  header_verified: boolean;
  architecture: string | null;
  context_tokens: number | null;
  model_sha256: string | null;
  projector_present: boolean | null;
  media_kinds: Array<'image' | 'audio'>;
  /** Size-only build-policy hint. It is never evidence that the file matches. */
  policy_candidate: ModelPolicyProfile | null;
  /** Exact policy identity established by native descriptor verification. */
  policy_verified: ModelPolicyProfile | null;
  /** Compatibility alias for policy_verified.profile_id. */
  tested_profile: string | null;
}

export interface ModelPolicyProfile {
  profile_id: string;
  rank: number;
}

export interface ModelUnloadOutcome {
  model_id: string | null;
  resident_slot_released: boolean;
}

export type ModelDownloadPhase =
  | 'inspecting_existing'
  | 'hashing_partial'
  | 'downloading'
  | 'verifying'
  | 'installing'
  | 'complete';

export type ModelDownloadStatus =
  | { status: 'queued' }
  | { status: 'running' }
  | {
      status: 'completed';
      bytes: number;
      sha256: string;
      disposition:
        | 'reused_existing'
        | 'downloaded_fresh'
        | 'downloaded_resumed'
        | 'downloaded_after_restart';
    }
  | { status: 'cancelled' }
  | {
      status: 'failed';
      message: string;
      retryable: boolean;
    };

export interface ModelDownloadSnapshot {
  command_id: string;
  request_fingerprint: string;
  display_name: string;
  target_path: string;
  expected_sha256: string;
  expected_bytes: number | null;
  phase: ModelDownloadPhase | null;
  downloaded_bytes: number;
  total_bytes: number | null;
  resumed_from_bytes: number;
  status: ModelDownloadStatus;
  cancel_requested: boolean;
  event_sequence: number;
  event_delivery_failures: number;
  updated_at_unix_ms: number;
  replayed: boolean;
}

export interface BranchCard {
  run_id: string;
  branch_id: string;
  candidate_id: string | null;
  source_revision_id: string;
  target_start_byte: number;
  target_end_byte: number;
  text: string;
  output_blob_id: string | null;
  output_byte_len: number | null;
  status: 'queued' | 'generating' | 'ready' | 'failed' | 'cancelled' | 'pruned' | 'rejected' | 'interrupted';
  /** Decimal u64. Strings preserve replay identity across the JS boundary. */
  seed: string | null;
  model_id: string | null;
  selection: 'keep_alternative' | 'promote' | 'reject' | null;
  error: string | null;
  error_truncated: boolean;
  created_at_unix_ms: number;
}

export type BranchSummary = Omit<BranchCard, 'text'>;

export interface BranchPageCursor {
  /** Decimal u64, preserved as text across the JS boundary. */
  sequence: string;
  run_id: string;
}

export interface BranchPage {
  branches: BranchSummary[];
  next_cursor: BranchPageCursor | null;
  has_more: boolean;
}

export interface BranchBody {
  run_id: string;
  output_blob_id: string;
  byte_len: number;
  text: string;
}

export interface WeaveStarted {
  command_id: string;
  request_id: string;
  project_id: string;
  session_id: string;
  document_id: string;
  source_revision_id: string;
  exact_prompt_blob_id: string;
  branches: BranchCard[];
}

export type GenerationEventKind =
  | { kind: 'queued' }
  | { kind: 'prefilling' }
  | { kind: 'generating' }
  | { kind: 'text_delta'; text: string }
  | { kind: 'token'; observation: unknown }
  | { kind: 'warning'; code: string; message: string }
  | { kind: 'cancellation_requested' }
  | {
      kind: 'candidate_ready';
      candidate_id: string;
      generated_span_artifact_id: string;
    };

export interface GenerationProgressEvent {
  event_id: string;
  run_id: string;
  branch_id: string;
  sequence: number;
  kind: GenerationEventKind;
  occurred_at_ms: number;
}

export interface GenerationTerminalEvent {
  event_id: string;
  run_id: string;
  branch_id: string;
  sequence: number;
  status: 'cancelled' | 'completed' | 'failed' | 'pruned' | 'rejected';
  candidate_id?: string;
  error?: string;
  occurred_at_ms: number;
}

export type GenerationStreamEvent =
  | { event: 'generation'; payload: GenerationProgressEvent }
  | { event: 'generation_terminal'; payload: GenerationTerminalEvent };

export interface DesktopGenerationEnvelope {
  project_id: string;
  session_id: string;
  document_id: string;
  request_id: string;
  event: GenerationStreamEvent;
}

export interface LoomFailure {
  code: string;
  message: string;
  retryable?: boolean;
}

export interface MergeByteRange {
  start: number;
  end: number;
}

export interface MergeConflictSpan {
  range: MergeByteRange;
  text: string;
}

export interface MergeConflict {
  kind: 'competing_insertions' | 'overlapping_edits';
  base: MergeConflictSpan;
  app_base_range: MergeByteRange;
  app: MergeConflictSpan;
  external_base_range: MergeByteRange;
  external: MergeConflictSpan;
}

export type MergeOutcome =
  | { status: 'merged'; content: string }
  | { status: 'conflict'; conflicts: MergeConflict[] };

export interface ReconciliationPreview {
  project_id: string;
  session_id: string;
  document_id: string;
  relative_path: string;
  kind: DocumentKind;
  active_revision_id: string;
  active_artifact_id: string;
  base_blob_id: string;
  app_blob_id: string;
  external_blob_id: string;
  external_visible_blob_id: string;
  base_text: string;
  app_text: string;
  external_text: string;
  external_visible_text: string;
  app_source: 'caller' | 'transient_draft' | 'base';
  draft_version: string | null;
  outcome: MergeOutcome;
}
