import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type {
  BranchBody,
  BranchPage,
  BranchPageCursor,
  BranchSummary,
  BuildModelPolicySummary,
  CommandReceipt,
  DesktopGenerationEnvelope,
  DocumentKind,
  ModelCapabilitySummary,
  ModelDownloadSnapshot,
  ModelUnloadOutcome,
  OpenDocument,
  ProjectCloseReceipt,
  ProjectSnapshot,
  ResearchPromotionPrompt,
  ResearchPromotionResult,
  RecoveryReport,
  ReconciliationPreview,
  LoomFailure,
  TransientDraftSnapshot,
  TransientDraftWriteReceipt,
  WeaveStarted
} from './types';
import { decodeBuildModelPolicy } from './buildModelPolicy';

const PREFIX = 'plugin:loom|';

export function isDesktopRuntime(): boolean {
  return '__TAURI_INTERNALS__' in window;
}

async function call<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  if (!isDesktopRuntime()) {
    throw { code: 'desktop_runtime_required', message: 'This command requires the Loom desktop runtime.' };
  }
  return invoke<T>(`${PREFIX}${command}`, args);
}

export function chooseAndCreateProject(title: string): Promise<ProjectSnapshot> {
  return call('project_choose_create', { title });
}

export function openDefaultProject(): Promise<ProjectSnapshot> {
  return call('project_open_default');
}

export function chooseAndOpenProject(): Promise<ProjectSnapshot> {
  return call('project_choose_open');
}

export function currentProjectSession(): Promise<ProjectSnapshot> {
  return call('project_current');
}

export async function getBuildModelPolicy(): Promise<BuildModelPolicySummary> {
  const value = await call<unknown>('build_model_policy_get');
  return decodeBuildModelPolicy(value);
}

export function openDocument(
  projectId: string,
  sessionId: string,
  documentId: string,
  relativePath: string
): Promise<OpenDocument> {
  return call('document_open', { projectId, sessionId, documentId, relativePath });
}

export function checkpointDocument(
  projectId: string,
  sessionId: string,
  documentId: string,
  relativePath: string,
  text: string,
  kind: DocumentKind,
  expectedRevisionId: string | null,
  expectedVisibleBlobId: string,
  commandId: string,
  draftVersion: string | null
): Promise<CommandReceipt> {
  return call('document_checkpoint', {
    projectId,
    sessionId,
    documentId,
    relativePath,
    text,
    kind,
    expectedRevisionId,
    expectedVisibleBlobId,
    commandId,
    draftVersion
  });
}

export function upsertTransientDraft(
  projectId: string,
  sessionId: string,
  documentId: string,
  relativePath: string,
  text: string,
  kind: DocumentKind,
  sourceRevisionId: string,
  expectedVersion: string
): Promise<TransientDraftWriteReceipt> {
  return call('document_draft_upsert', {
    projectId,
    sessionId,
    documentId,
    relativePath,
    text,
    kind,
    sourceRevisionId,
    expectedVersion
  });
}

export function clearTransientDraft(
  projectId: string,
  sessionId: string,
  documentId: string,
  relativePath: string,
  expectedVersion: string
): Promise<boolean> {
  return call('document_draft_clear', {
    projectId,
    sessionId,
    documentId,
    relativePath,
    expectedVersion
  });
}

export function previewDocumentReconciliation(
  projectId: string,
  sessionId: string,
  documentId: string,
  relativePath: string,
  expectedRevisionId: string,
  expectedBaseBlobId: string,
  appText: string | null
): Promise<ReconciliationPreview> {
  return call('document_reconciliation_preview', {
    projectId,
    sessionId,
    documentId,
    relativePath,
    expectedRevisionId,
    expectedBaseBlobId,
    appText
  });
}

export function applyDocumentReconciliation(
  projectId: string,
  sessionId: string,
  documentId: string,
  relativePath: string,
  expectedRevisionId: string,
  expectedBaseBlobId: string,
  expectedExternalVisibleBlobId: string,
  resolvedText: string,
  kind: DocumentKind,
  reason: string,
  commandId: string
): Promise<CommandReceipt> {
  return call('document_reconcile_apply', {
    projectId,
    sessionId,
    documentId,
    relativePath,
    expectedRevisionId,
    expectedBaseBlobId,
    expectedExternalVisibleBlobId,
    resolvedText,
    kind,
    reason,
    commandId
  });
}

export function recoverProject(projectId: string, sessionId: string): Promise<RecoveryReport> {
  return call('project_recover', { projectId, sessionId });
}

export function closeProject(
  projectId: string,
  sessionId: string,
  commandId: string
): Promise<ProjectCloseReceipt> {
  return call('project_close', { projectId, sessionId, commandId });
}

export function listModels(): Promise<ModelCapabilitySummary[]> {
  return call('model_list');
}

export function chooseModel(): Promise<ModelCapabilitySummary | null> {
  return call('model_choose');
}

export function loadModel(modelPath: string): Promise<ModelCapabilitySummary> {
  return call('model_load', { modelPath });
}

export function loadPolicyModelCandidate(
  profileId: string,
  modelPath: string
): Promise<ModelCapabilitySummary> {
  return call('model_load_policy_candidate', { profileId, modelPath });
}

export function unloadModel(): Promise<ModelUnloadOutcome> {
  return call('model_unload');
}

export interface StartModelDownloadArgs {
  commandId: string;
  url: string;
  fileName: string;
  expectedSha256: string;
  expectedBytes: number | null;
  maxBytes: number;
}

export function startModelDownload(
  args: StartModelDownloadArgs
): Promise<ModelDownloadSnapshot> {
  return call('model_download_start', { ...args });
}

export function cancelModelDownload(commandId: string): Promise<ModelDownloadSnapshot> {
  return call('model_download_cancel', { commandId });
}

export function getModelDownloadStatus(commandId: string): Promise<ModelDownloadSnapshot> {
  return call('model_download_status', { commandId });
}

export function listModelDownloads(): Promise<ModelDownloadSnapshot[]> {
  return call('model_download_list');
}

export async function listenForModelDownloadEvents(
  handler: (event: ModelDownloadSnapshot) => void
): Promise<UnlistenFn> {
  if (!isDesktopRuntime()) {
    return Promise.reject({
      code: 'desktop_runtime_required',
      message: 'Model download events require the Loom desktop runtime.'
    });
  }
  const unlistenProgress = await listen<ModelDownloadSnapshot>(
    'loom://model-download-progress',
    ({ payload }) => handler(payload)
  );
  try {
    const unlistenTerminal = await listen<ModelDownloadSnapshot>(
      'loom://model-download-terminal',
      ({ payload }) => handler(payload)
    );
    return () => {
      unlistenProgress();
      unlistenTerminal();
    };
  } catch (error) {
    unlistenProgress();
    throw error;
  }
}

export function getBranchPage(
  projectId: string,
  sessionId: string,
  documentId: string,
  after: BranchPageCursor | null,
  limit: number
): Promise<BranchPage> {
  return call('branch_page', { projectId, sessionId, documentId, after, limit });
}

export function getBranch(
  projectId: string,
  sessionId: string,
  documentId: string,
  runId: string
): Promise<BranchSummary | null> {
  return call('branch_get', { projectId, sessionId, documentId, runId });
}

export function getBranchBody(
  projectId: string,
  sessionId: string,
  documentId: string,
  runId: string,
  maxBytes: number
): Promise<BranchBody | null> {
  return call('branch_body', { projectId, sessionId, documentId, runId, maxBytes });
}

export interface WeaveStartArgs {
  projectId: string;
  sessionId: string;
  commandId: string;
  documentId: string;
  relativePath: string;
  sourceRevisionId: string;
  expectedVisibleBlobId: string;
  cursorByte: number;
  policy:
    | { kind: 'automatic_v2' }
    | {
        kind: 'manual_v2';
        branch_count: number;
        max_tokens: number;
        temperature: number;
      };
}

export function startWeave(args: WeaveStartArgs): Promise<WeaveStarted> {
  return call('weave_start', { ...args });
}

export function getWeaveStatus(
  projectId: string,
  sessionId: string,
  commandId: string
): Promise<WeaveStarted | null> {
  return call('weave_status', { projectId, sessionId, commandId });
}

export function cancelGeneration(
  projectId: string,
  sessionId: string,
  commandId: string,
  runId: string
): Promise<CommandReceipt> {
  return call('generation_cancel', { projectId, sessionId, commandId, runId });
}

export function keepCandidate(
  projectId: string,
  sessionId: string,
  commandId: string,
  candidateId: string
): Promise<CommandReceipt> {
  return call('candidate_keep', { projectId, sessionId, commandId, candidateId });
}

export function promoteCandidate(
  projectId: string,
  sessionId: string,
  commandId: string,
  candidateId: string,
  expectedSourceRevisionId: string,
  expectedVisibleBlobId: string
): Promise<CommandReceipt> {
  return call('candidate_promote', {
    projectId,
    sessionId,
    commandId,
    candidateId,
    expectedSourceRevisionId,
    expectedVisibleBlobId
  });
}

export function listPendingResearchPromotions(
  projectId: string,
  sessionId: string
): Promise<ResearchPromotionPrompt[]> {
  return call('research_promotion_pending', { projectId, sessionId });
}

export function importResearchPromotion(
  projectId: string,
  sessionId: string
): Promise<ResearchPromotionPrompt | null> {
  return call('research_promotion_import', { projectId, sessionId });
}

export function confirmResearchPromotion(
  projectId: string,
  sessionId: string,
  input: ResearchPromotionPrompt
): Promise<ResearchPromotionResult> {
  return call('research_promotion_confirm', {
    projectId,
    sessionId,
    input: {
      command_id: input.command_id,
      nonce: input.nonce,
      document_id: input.document_id,
      candidate_fingerprint: input.candidate_fingerprint,
      promotion_fingerprint: input.promotion_fingerprint
    }
  });
}

export function listenForGenerationEvents(
  handler: (event: DesktopGenerationEnvelope) => void
): Promise<UnlistenFn> {
  if (!isDesktopRuntime()) {
    return Promise.reject({
      code: 'desktop_runtime_required',
      message: 'Generation events require the Loom desktop runtime.'
    });
  }
  return listen<DesktopGenerationEnvelope>('loom://generation', ({ payload }) => handler(payload));
}

export function listenForApplicationCloseRequests(
  handler: () => void
): Promise<UnlistenFn> {
  if (!isDesktopRuntime()) {
    return Promise.reject({
      code: 'desktop_runtime_required',
      message: 'Application close requests require the Loom desktop runtime.'
    });
  }
  return listen('loom://application-close-requested', handler);
}

export function setFocusMode(
  projectId: string,
  sessionId: string,
  enabled: boolean
): Promise<void> {
  return call('focus_mode_set', { projectId, sessionId, enabled });
}

export function setSuggestions(
  projectId: string,
  sessionId: string,
  enabled: boolean
): Promise<void> {
  return call('suggestions_set', { projectId, sessionId, enabled });
}

export function requestApplicationClose(): Promise<void> {
  return call('application_close');
}

export function abortApplicationClose(): Promise<void> {
  return call('application_close_abort');
}

export function applicationClosePending(): Promise<boolean> {
  return call('application_close_pending');
}

export function describeFailure(error: unknown): string {
  return normalizeFailure(error).message;
}

export function normalizeFailure(error: unknown): LoomFailure {
  if (typeof error === 'string') {
    return { code: 'command_transport_failed', message: error, retryable: true };
  }
  if (error && typeof error === 'object') {
    const value = error as Record<string, unknown>;
    const message = typeof value.message === 'string'
      ? value.message
      : typeof value.error === 'string'
        ? value.error
        : 'Loom could not complete that command.';
    return {
      code: typeof value.code === 'string' ? value.code : 'command_failed',
      message,
      retryable: value.retryable === true
    };
  }
  return {
    code: 'command_transport_failed',
    message: 'Loom could not complete that command.',
    retryable: true
  };
}
