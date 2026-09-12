import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type {
  BranchBody,
  BranchPage,
  BranchPageCursor,
  BranchSummary,
  BuildModelPolicySummary,
  CommandReceipt,
  CompletionSnapshot,
  CoWriterSummary,
  ContextAttachment,
  DocumentContextSnapshot,
  CuratedModelCatalogSnapshot,
  DesktopGenerationEnvelope,
  DocumentKind,
  DocumentSummary,
  ModelCapabilitySummary,
  ModelDownloadSnapshot,
  ModelUnloadOutcome,
  OpenDocument,
  ProjectCloseReceipt,
  ProjectSnapshot,
  RecoveryReport,
  ReconciliationPreview,
  SpeechInputSnapshot,
  SpeechInputTarget,
  SpeechRecordingSnapshot,
  LoomFailure,
  TransientDraftSnapshot,
  TransientDraftWriteReceipt,
  WeaveStarted
} from './types';
import { decodeBuildModelPolicy } from './buildModelPolicy';
import type { ImageAttachmentReceipt } from './attachments';
import type { DocumentFilesystemHint } from './documentFilesystemHint';

export type { DocumentFilesystemHint } from './documentFilesystemHint';

const PREFIX = 'plugin:loom|';

// Project commands share one renderer-side ordering lane. Classify by the
// smaller, fail-safe exception set: an unrecognized future command is queued.
// Native session admission is authoritative across renderers and blocks for
// its bounded critical sections, so renderer classification is an ordering and
// latency optimization rather than a correctness boundary.
const INDEPENDENT_COMMANDS = new Set([
  'application_close_abort',
  'application_close_pending',
  'build_model_policy_get',
  'model_catalog_list',
  'model_download_cancel',
  'model_download_list',
  'model_download_start',
  'model_download_status',
  'model_list',
  'model_load',
  'model_load_catalog_candidate',
  'model_load_policy_candidate',
  'model_unload'
]);

const PROJECT_TRANSITION_RETRY_DELAYS_MS = [20, 40, 80, 160, 240, 360, 500] as const;

let sessionCommandTail: Promise<void> = Promise.resolve();

export function isDesktopRuntime(): boolean {
  return '__TAURI_INTERNALS__' in window;
}

function isTypedProjectAdmissionPending(command: string, error: unknown): boolean {
  if (!error || typeof error !== 'object') return false;
  const value = error as Record<string, unknown>;
  return value.retryable === true &&
    command === 'project_current' &&
    value.code === 'project_transition_in_progress';
}

function wait(milliseconds: number): Promise<void> {
  return new Promise((resolve) => globalThis.setTimeout(resolve, milliseconds));
}

async function invokeWhenProjectSessionAdmitted<T>(
  command: string,
  args: Record<string, unknown>
): Promise<T> {
  for (let attempt = 0; ; attempt += 1) {
    try {
      return await invoke<T>(`${PREFIX}${command}`, args);
    } catch (error) {
      if (!isTypedProjectAdmissionPending(command, error)) throw error;
      // A detached chooser/close transition is observable native state, not an
      // error. Keep waiting for its exact outcome without spinning.
      const delay = PROJECT_TRANSITION_RETRY_DELAYS_MS[
        Math.min(attempt, PROJECT_TRANSITION_RETRY_DELAYS_MS.length - 1)
      ];
      await wait(delay);
    }
  }
}

function enqueueSessionCommand<T>(operation: () => Promise<T>): Promise<T> {
  const result = sessionCommandTail.then(operation);
  sessionCommandTail = result.then(
    () => undefined,
    () => undefined
  );
  return result;
}

async function call<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  if (!isDesktopRuntime()) {
    throw { code: 'desktop_runtime_required', message: 'This command requires the Loom desktop runtime.' };
  }

  if (INDEPENDENT_COMMANDS.has(command)) {
    return invoke<T>(`${PREFIX}${command}`, args);
  }

  return enqueueSessionCommand(() => invokeWhenProjectSessionAdmitted<T>(command, args));
}

export function openDefaultProject(): Promise<ProjectSnapshot> {
  return call('project_open_default');
}

export function prepareProjectOpen(): Promise<string | null> {
  return call('project_prepare_open');
}

export function commitProjectOpen(preparationId: string): Promise<ProjectSnapshot> {
  return call('project_commit_open', { preparationId });
}

export function discardProjectOpen(preparationId: string): Promise<void> {
  return call('project_discard_open', { preparationId });
}

export function currentProjectSession(): Promise<ProjectSnapshot> {
  return call('project_current');
}

export function createDocument(projectId: string, sessionId: string): Promise<ProjectSnapshot> {
  return call('document_create', { projectId, sessionId });
}

export function renameDocument(
  projectId: string,
  sessionId: string,
  documentId: string,
  expectedRevisionId: string,
  expectedBlobId: string,
  title: string
): Promise<DocumentSummary> {
  return call('document_rename', {
    projectId,
    sessionId,
    documentId,
    expectedRevisionId,
    expectedBlobId,
    title
  });
}

export function deleteDocument(
  projectId: string,
  sessionId: string,
  documentId: string,
  expectedRevisionId: string,
  expectedBlobId: string,
  commandId: string
): Promise<ProjectSnapshot> {
  return call('document_delete', {
    projectId,
    sessionId,
    documentId,
    expectedRevisionId,
    expectedBlobId,
    commandId
  });
}

export function ingestImageAttachment(
  projectId: string,
  sessionId: string,
  mediaType: string,
  base64: string
): Promise<ImageAttachmentReceipt> {
  return call('attachment_ingest', {
    projectId,
    sessionId,
    mediaType,
    encoded: base64
  });
}

export function importAttachmentPaths(
  projectId: string,
  sessionId: string,
  paths: readonly string[]
): Promise<ContextAttachment[]> {
  return call('attachment_import_paths', { projectId, sessionId, paths: [...paths] });
}

export function chooseAttachments(
  projectId: string,
  sessionId: string
): Promise<ContextAttachment[]> {
  return call('attachment_import_choose', { projectId, sessionId });
}

export function listDocumentContext(
  projectId: string,
  sessionId: string,
  documentId: string
): Promise<DocumentContextSnapshot> {
  return call('document_context_list', { projectId, sessionId, documentId });
}

export function addDocumentContext(
  projectId: string,
  sessionId: string,
  documentId: string,
  attachmentId: string
): Promise<DocumentContextSnapshot> {
  return call('document_context_add', { projectId, sessionId, documentId, attachmentId });
}

export function addDocumentContexts(
  projectId: string,
  sessionId: string,
  documentId: string,
  attachmentIds: readonly string[]
): Promise<DocumentContextSnapshot> {
  return call('document_context_add_many', {
    projectId,
    sessionId,
    documentId,
    attachmentIds: [...attachmentIds]
  });
}

export function removeDocumentContext(
  projectId: string,
  sessionId: string,
  documentId: string,
  attachmentId: string
): Promise<DocumentContextSnapshot> {
  return call('document_context_remove', { projectId, sessionId, documentId, attachmentId });
}

export function getDocumentContextText(
  projectId: string,
  sessionId: string,
  documentId: string
): Promise<string> {
  return call('document_context_text_get', { projectId, sessionId, documentId });
}

export function setDocumentContextText(
  projectId: string,
  sessionId: string,
  documentId: string,
  text: string
): Promise<string> {
  return call('document_context_text_set', { projectId, sessionId, documentId, text });
}

export function setDocumentContextSnapshot(
  projectId: string,
  sessionId: string,
  documentId: string,
  markdown: string,
  attachmentIds: readonly string[]
): Promise<DocumentContextSnapshot> {
  return call('document_context_snapshot_set', {
    projectId,
    sessionId,
    documentId,
    markdown,
    attachmentIds: [...attachmentIds]
  });
}

export function listCoWriters(
  projectId: string,
  sessionId: string
): Promise<CoWriterSummary[]> {
  return call('co_writer_list', { projectId, sessionId });
}

export function saveCoWriter(
  projectId: string,
  sessionId: string,
  documentId: string,
  name: string
): Promise<CoWriterSummary> {
  return call('co_writer_save', { projectId, sessionId, documentId, name });
}

export function applyCoWriter(
  projectId: string,
  sessionId: string,
  documentId: string,
  profileId: string
): Promise<DocumentContextSnapshot> {
  return call('co_writer_apply', { projectId, sessionId, documentId, profileId });
}

export function deleteCoWriter(
  projectId: string,
  sessionId: string,
  profileId: string
): Promise<CoWriterSummary[]> {
  return call('co_writer_delete', { projectId, sessionId, profileId });
}

export function getSpeechInputCapabilities(
  projectId: string,
  sessionId: string
): Promise<unknown> {
  return call('speech_input_capabilities', { projectId, sessionId });
}

export function startSpeechRecording(
  projectId: string,
  sessionId: string,
  documentId: string,
  target: SpeechInputTarget
): Promise<SpeechRecordingSnapshot> {
  return call('speech_input_record_start', { projectId, sessionId, documentId, target });
}

export function stopSpeechRecording(
  projectId: string,
  sessionId: string,
  recordingId: string
): Promise<SpeechInputSnapshot> {
  return call('speech_input_record_stop', { projectId, sessionId, recordingId });
}

export function cancelSpeechRecording(
  projectId: string,
  sessionId: string,
  recordingId: string
): Promise<SpeechRecordingSnapshot> {
  return call('speech_input_record_cancel', { projectId, sessionId, recordingId });
}

export function getSpeechInputStatus(
  projectId: string,
  sessionId: string,
  requestId: string
): Promise<SpeechInputSnapshot> {
  return call('speech_input_status', { projectId, sessionId, requestId });
}

export function cancelSpeechInput(
  projectId: string,
  sessionId: string,
  requestId: string
): Promise<SpeechInputSnapshot> {
  return call('speech_input_cancel', { projectId, sessionId, requestId });
}

export async function getBuildModelPolicy(): Promise<BuildModelPolicySummary> {
  const value = await call<unknown>('build_model_policy_get');
  return decodeBuildModelPolicy(value);
}

export function openDocument(
  projectId: string,
  sessionId: string,
  documentId: string,
  expectedRevisionId: string,
  expectedBlobId: string
): Promise<OpenDocument> {
  return call('document_open', {
    projectId,
    sessionId,
    documentId,
    expectedRevisionId,
    expectedBlobId
  });
}

export function exportDocumentCopy(
  projectId: string,
  sessionId: string,
  documentId: string,
  expectedRevisionId: string,
  expectedBlobId: string
): Promise<CommandReceipt | null> {
  return call('document_export_choose', {
    projectId,
    sessionId,
    documentId,
    expectedRevisionId,
    expectedBlobId
  });
}

export function revealDocument(
  projectId: string,
  sessionId: string,
  documentId: string,
  expectedRevisionId: string,
  expectedBlobId: string
): Promise<void> {
  return call('document_reveal', {
    projectId,
    sessionId,
    documentId,
    expectedRevisionId,
    expectedBlobId
  });
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
  expectedRevisionId: string,
  expectedBaseBlobId: string,
  appText: string | null
): Promise<ReconciliationPreview> {
  return call('document_reconciliation_preview', {
    projectId,
    sessionId,
    documentId,
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

export function listCuratedModels(): Promise<CuratedModelCatalogSnapshot> {
  return call('model_catalog_list');
}

export function chooseModel(): Promise<ModelCapabilitySummary | null> {
  return call('model_choose');
}

export function loadModel(modelPath: string): Promise<ModelCapabilitySummary> {
  return call('model_load', { modelPath });
}

export function loadCatalogModelCandidate(
  catalogId: string,
  modelPath: string
): Promise<ModelCapabilitySummary> {
  return call('model_load_catalog_candidate', { catalogId, modelPath });
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

export function getCompletionSnapshot(
  projectId: string,
  sessionId: string,
  documentId: string,
  observedRunIds: string[]
): Promise<CompletionSnapshot> {
  return call('completion_snapshot', {
    projectId,
    sessionId,
    documentId,
    observedRunIds
  });
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

export function listenForDocumentFilesystemHints(
  handler: (event: DocumentFilesystemHint) => void
): Promise<UnlistenFn> {
  if (!isDesktopRuntime()) {
    return Promise.reject({
      code: 'desktop_runtime_required',
      message: 'Document filesystem hints require the Loom desktop runtime.'
    });
  }
  return listen<DocumentFilesystemHint>(
    'loom://document-filesystem-hint',
    ({ payload }) => handler(payload)
  );
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

export interface FileCommandEvent {
  command: 'new_document' | 'open_project' | 'save' | 'export_copy';
}

export function listenForFileCommands(
  handler: (event: FileCommandEvent) => void
): Promise<UnlistenFn> {
  if (!isDesktopRuntime()) {
    return Promise.reject({
      code: 'desktop_runtime_required',
      message: 'File menu commands require the Loom desktop runtime.'
    });
  }
  return listen<FileCommandEvent>('loom://file-command', ({ payload }) => handler(payload));
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
