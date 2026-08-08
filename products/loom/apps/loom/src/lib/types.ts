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
  completion: boolean;
  fill_in_middle: boolean;
  output_tokens: boolean;
  logprobs: boolean;
  model_path: string;
  file_bytes: number;
  header_verified: boolean;
}

export interface BranchCard {
  branch_id: string;
  source_revision_id: string;
  text: string;
  status: 'queued' | 'generating' | 'ready' | 'failed' | 'cancelled' | 'pruned';
  /** Decimal u64. Strings preserve replay identity across the JS boundary. */
  seed: string;
  model_id: string;
  created_at_unix_ms: number;
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
