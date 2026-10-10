import { newUlid } from './ulid';
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
  WeaveStarted,
  TerminalRun,
  TerminalRunRequest
} from './types';
import { decodeBuildModelPolicy } from './buildModelPolicy';
import type { ImageAttachmentReceipt } from './attachments';
import type { DocumentFilesystemHint } from './documentFilesystemHint';

export type { DocumentFilesystemHint } from './documentFilesystemHint';

const PREFIX = 'plugin:loom|';

export interface LoomPreferences {
  revision: string;
  last_local_model: string | null;
  project_suggestions: Record<string, boolean>;
}
export type PreferenceChange =
  | { kind: 'remember_model'; path: string }
  | { kind: 'forget_model'; expected_path: string }
  | { kind: 'suggestions'; project_id: string; enabled: boolean | null };
export function getPreferences(): Promise<LoomPreferences> {
  return call('preferences_get', {});
}
export function updatePreferences(change: PreferenceChange): Promise<LoomPreferences> {
  return call('preferences_update', { change });
}


export function documentReferenceDiagnostics(projectId: string, sessionId: string, text: string): Promise<import('./referenceDiagnostics').ReferenceDiagnostic[]> {
  return call('document_reference_diagnostics', { projectId, sessionId, text });
}

// Project commands share one renderer-side ordering lane. Classify by the
// smaller, fail-safe exception set: an unrecognized future command is queued.
// Native session admission is authoritative across renderers and blocks for
// its bounded critical sections, so renderer classification is an ordering and
// latency optimization rather than a correctness boundary.
const INDEPENDENT_COMMANDS = new Set([
  'material_pdf_page',
  'workspace_source_import_cancel',
  'import_account_cancel',
  'collection_cancel',
  'collection_status',
  'collection_refresh',
  'import_source_url',
  'import_accounts',
  'import_account_connect',
  'import_account_disconnect',
  'application_close_abort',
  'application_close_pending',
  'audio_synthesize',
  'build_model_policy_get',
  'shader_preview',
  'model_catalog_list',
  'model_download_cancel',
  'model_download_list',
  'model_download_start',
  'model_download_status',
  'inference_status',
  'visual_ghost_rendered',
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

export function prepareProjectOpenPath(path: string): Promise<string | null> {
  return call('project_prepare_open_path', { path });
}

export function getWorkspaceRoots(projectId: string, sessionId: string): Promise<import('./workspaceFolders').WorkspaceRootsSnapshot> {
  return call('workspace_roots_get', { projectId, sessionId });
}

export function prepareWorkspaceRoot(projectId: string, sessionId: string, rootId: string): Promise<string | null> {
  return call('workspace_root_prepare', { projectId, sessionId, rootId });
}

export function removeWorkspaceRoot(projectId: string, sessionId: string, rootId: string): Promise<void> {
  return call('workspace_root_remove', { projectId, sessionId, rootId });
}

export function projectDropDirectories(paths: string[]): Promise<string[]> {
  return call('project_drop_directories', { paths });
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
  paths: readonly string[],
  operationId = newUlid()
): Promise<ImportBatch> {
  return call('attachment_import_paths', { projectId, sessionId, paths: [...paths], operationId });
}

export function revealAttachmentOriginal(projectId: string, sessionId: string, attachmentId: string): Promise<void> {
  return call('attachment_reveal_original', { projectId, sessionId, attachmentId });
}

export interface WorkspaceCopyReport {
  copied: string[];
  materials: import('./materials').MaterialEntry[];
  failures: { name: string; message: string }[];
}

export function copyWorkspaceFiles(projectId: string, sessionId: string, destination: string, paths: readonly string[]): Promise<WorkspaceCopyReport> {
  return call('workspace_copy_files', { projectId, sessionId, destination, paths: [...paths], operationId: newUlid() });
}

export function copyWorkspaceFolder(projectId: string, sessionId: string, destination: string, operationId = newUlid()): Promise<WorkspaceCopyReport | null> {
  return call('workspace_copy_folder_choose', { projectId, sessionId, destination, operationId });
}

export function chooseAttachments(
  projectId: string,
  sessionId: string,
  operationId = newUlid()
): Promise<ImportBatch> {
  return call('attachment_import_choose', { projectId, sessionId, operationId });
}

export interface WorkspaceSourceImportReport {
  workspace_id: string;
  workspace_session_id: string;
  operation_id: string;
  imported: Array<{ attachment: ContextAttachment; material: import('./materials').MaterialEntry | null }>;
  failures: Array<{ name: string; message: string }>;
  cancelled: boolean;
}

export function chooseWorkspaceSources(projectId: string, sessionId: string, operationId = newUlid()): Promise<WorkspaceSourceImportReport> {
  return call('workspace_source_import_choose', { projectId, sessionId, operationId });
}

export function importWorkspaceSourcePaths(projectId: string, sessionId: string, paths: readonly string[], operationId = newUlid()): Promise<WorkspaceSourceImportReport> {
  return call('workspace_source_import_paths', { projectId, sessionId, operationId, paths: [...paths] });
}

export function pasteWorkspaceSources(projectId: string, sessionId: string, text: string, separator: string, operationId = newUlid()): Promise<WorkspaceSourceImportReport> {
  return call('workspace_source_import_paste', { projectId, sessionId, operationId, text, separator });
}

export function cancelWorkspaceSourceImport(projectId: string, sessionId: string, operationId: string): Promise<void> {
  return call('workspace_source_import_cancel', { projectId, sessionId, operationId });
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
  attachmentIds: readonly string[],
  materials?: readonly import('./types').ContextMaterial[]
): Promise<DocumentContextSnapshot> {
  return call('document_context_snapshot_set', {
    projectId,
    sessionId,
    documentId,
    markdown,
    attachmentIds: [...attachmentIds],
    ...(materials ? { materials: [...materials] } : {})
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

export interface AudioActivity {
  duration_ms: number;
  signal_detected: boolean;
  limit_reached: boolean;
  segments: Array<{ start_ms: number; end_ms: number }>;
}

export interface AudioRecording {
  recording_id: string;
  document_id: string;
  attachment: ContextAttachment;
  activity: AudioActivity;
}

export interface AudioSpeech {
  wav: number[];
}

export function startAudioRecording(
  projectId: string,
  sessionId: string,
  documentId: string
): Promise<SpeechRecordingSnapshot> {
  return call('audio_record_start', { projectId, sessionId, documentId });
}

export function stopAudioRecording(
  projectId: string,
  sessionId: string,
  recordingId: string
): Promise<AudioRecording> {
  return call('audio_record_stop', { projectId, sessionId, recordingId });
}

export function synthesizeAudio(text: string): Promise<AudioSpeech> {
  return call('audio_synthesize', { text });
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

export function importExternalDocument(
  projectId: string, sessionId: string, documentId: string,
  expectedRevisionId: string, expectedBlobId: string
): Promise<DocumentSummary | null> {
  return call('document_import_external', {
    projectId, sessionId, documentId, expectedRevisionId, expectedBlobId
  });
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

export function getInferenceStatus(): Promise<{ suggestions: { model_id: string; completion: boolean } | null }> {
  return call('inference_status');
}

export function recordVisualGhostRendered(): Promise<void> {
  return call('visual_ghost_rendered');
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
    | { kind: 'automatic_v3' }
    | { kind: 'automatic_visual_v4' }
    | { kind: 'loompad_v2'; sample_target: 4 | 16 | 64 | 256; batch_offset: number }
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
    const recovery = value.speculation_recovery as Record<string, unknown> | undefined;
    const validRecovery = recovery && typeof recovery.snapshot_id === 'string' && /^[a-f0-9]{64}$/.test(recovery.snapshot_id) &&
      Number.isInteger(recovery.next_offset) && Number(recovery.next_offset) >= 0 && Number(recovery.next_offset) <= 256 &&
      Array.isArray(recovery.command_ids) && recovery.command_ids.length * 4 === recovery.next_offset &&
      recovery.command_ids.every(id => typeof id === 'string' && /^[0-9A-HJKMNP-TV-Z]{26}$/.test(id)) &&
      new Set(recovery.command_ids).size === recovery.command_ids.length;
    return {
      ...(validRecovery ? { speculation_recovery: recovery as NonNullable<LoomFailure['speculation_recovery']> } : {}),
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

export function runTerminal(request: TerminalRunRequest): Promise<TerminalRun> {
  return call('terminal_run', { ...request });
}

export function listTerminalRuns(projectId: string, sessionId: string): Promise<TerminalRun[]> {
  return call('terminal_list', { projectId, sessionId });
}

export function cancelTerminalRun(projectId: string, sessionId: string, runId: string): Promise<void> {
  return call('terminal_cancel', { projectId, sessionId, runId });
}

export function runWorkspacePane(request: import('./workspacePaneDrafts').WorkspacePaneRunRequest): Promise<import('./workspacePaneDrafts').WorkspacePaneSubmission> {
  return call('workspace_pane_run', { request });
}

export function listWorkspacePaneRuns(projectId: string, sessionId: string): Promise<TerminalRun[]> {
  return call('workspace_pane_list', { projectId, sessionId });
}

export function cancelWorkspacePaneRun(projectId: string, sessionId: string, runId: string): Promise<void> {
  return call('workspace_pane_cancel', { projectId, sessionId, runId });
}

export function readWorkspacePaneOutput(projectId: string, sessionId: string, runId: string): Promise<OpenDocument | null> {
  return call('workspace_pane_output', { projectId, sessionId, runId });
}

export function resolveWorkspaceDocument(projectId: string, sessionId: string, reference: string): Promise<OpenDocument | null> {
  return call('workspace_document_resolve', { projectId, sessionId, reference });
}

export function resolveWorkspaceReference(projectId: string, sessionId: string, reference: string): Promise<{ root_id: string; document_id: string; workspace_session_id: string }> {
  return call('workspace_reference_resolve', { projectId, sessionId, reference });
}

export function compileShaderPreview(source: string): Promise<{ fragment: string }> {
  return call('shader_preview', { source });
}

export function getWorkspaceTemplate(projectId: string, sessionId: string): Promise<import('./workspaceTemplate').WorkspaceTemplateSnapshot> {
  return call('workspace_template_get', { projectId, sessionId });
}
export function enableWorkspaceTemplate(projectId: string, sessionId: string): Promise<import('./workspaceTemplate').WorkspaceTemplateSnapshot> {
  return call('workspace_template_enable', { projectId, sessionId });
}

export type ImportSource = 'gmail' | 'google_alerts' | 'linked_in' | 'drive';
export interface ImportAccount { service: 'gmail' | 'drive'; email: string | null }
export interface ImportBatch { imported: ContextAttachment[]; references?: string[]; failures: { name: string; message: string }[]; next_page_token: string | null }
export function importAccounts(projectId: string, sessionId: string): Promise<ImportAccount[]> {
  return call('import_accounts', { projectId, sessionId });
}
export function connectImportAccount(projectId: string, sessionId: string, service: 'gmail' | 'drive', clientId: string, clientSecret: string, operationId: string = newUlid()): Promise<ImportAccount> {
  return call('import_account_connect', { projectId, sessionId, service, clientId, clientSecret, operationId });
}
export function disconnectImportAccount(projectId: string, sessionId: string, service: 'gmail' | 'drive', accountEmail: string): Promise<void> {
  return call('import_account_disconnect', { projectId, sessionId, service, accountEmail });
}
export function chooseImportBatch(projectId: string, sessionId: string, folder: boolean, operationId: string = newUlid()): Promise<ImportBatch> {
  return call('attachment_import_batch_choose', { projectId, sessionId, folder, operationId });
}

export function importSourceUrl(projectId: string, sessionId: string, url: string, operationId: string = newUlid()): Promise<ImportBatch> {
  return call('import_source_url', { projectId, sessionId, url, operationId });
}

export function importPastedSources(projectId: string, sessionId: string, text: string, separator: string, operationId: string = newUlid()): Promise<ImportBatch> {
  return call('import_text_sources', { projectId, sessionId, text, separator, operationId });
}

export function cancelImportAccount(projectId: string, sessionId: string, operationId: string): Promise<void> {
  return call('import_account_cancel', { projectId, sessionId, operationId });
}

// Named material uses the same project/session ordering lane as document edits.
export function listMaterials(projectId: string, sessionId: string): Promise<import('./materials').MaterialEntry[]> {
  return call('material_list', { projectId, sessionId });
}
export function bindAttachmentMaterial(projectId: string, sessionId: string, attachmentId: string, retention: 'ordinary' | 'protected' = 'ordinary'): Promise<import('./materials').MaterialEntry> {
  return call('material_bind_attachment', { projectId, sessionId, attachmentId, retention });
}
export function readMaterial(projectId: string, sessionId: string, materialId: string): Promise<import('./materials').MaterialRead> {
  return call('material_read', { projectId, sessionId, id: materialId });
}

// Page requests share a separate lane: they neither delay edits nor race one
// another for the native preview worker. Authority is rechecked natively.
let pdfPageTail: Promise<unknown> = Promise.resolve();
export function readMaterialPdfPage(token: string, page: number, signal?: AbortSignal): Promise<import('./types').MaterialPdfPage> {
  const operationId = newUlid();
  const result = pdfPageTail.then(() => {
    signal?.throwIfAborted();
    return call<import('./types').MaterialPdfPage>('material_pdf_page', { token, page, operationId });
  });
  pdfPageTail = result.catch(() => undefined);
  return result;
}
export function searchMaterial(projectId: string, sessionId: string, materialId: string, query: string): Promise<import('./materials').MaterialSearch> {
  return call('material_search', { projectId, sessionId, id: materialId, query });
}
export function readMaterialEvidence(projectId: string, sessionId: string, materialId: string, evidenceId: string): Promise<import('./materials').MaterialEvidence> {
  return call('material_read_evidence', { projectId, sessionId, id: materialId, evidenceId });
}

export function resolveMaterialReference(projectId: string, sessionId: string, reference: string): Promise<import('./materialEvidenceScope').MaterialReferenceResolution> {
  return call('material_resolve_reference', { projectId, sessionId, reference });
}
export function addLibraryMaterial(projectId: string, sessionId: string): Promise<import('./materials').MaterialEntry | null> {
  return call('material_add_library', { projectId, sessionId });
}
export function addLibraryMaterialPath(projectId: string, sessionId: string, path: string): Promise<import('./materials').MaterialEntry> {
  return call('material_add_library_path', { projectId, sessionId, path });
}
export function pinMaterial(projectId: string, sessionId: string, materialId: string, pinned: boolean, expectedMetadataRevision: string): Promise<import('./materials').MaterialEntry> {
  return call('material_set_pinned', { projectId, sessionId, id: materialId, pinned, expectedMetadataRevision });
}

export function removeMaterial(projectId: string, sessionId: string, materialId: string, expectedMetadataRevision: string): Promise<void> {
  return call('material_remove', { projectId, sessionId, id: materialId, expectedMetadataRevision });
}

/** Metadata-only rename. No source path or document write enters this command. */
export function renameMaterial(request: import('./materialMetadata').MaterialRenameRequest): Promise<import('./materialMetadata').MaterialRenameReceipt> {
  return call('material_rename', { ...request });
}

export function addCollection(projectId: string, sessionId: string, name: string, scope: import('./types').CollectionScope, accountEmail: string, operationId: string = newUlid()): Promise<import('./materials').MaterialEntry> {
  return call('collection_add', { projectId, sessionId, operationId, name, scope, accountEmail });
}
export function refreshCollection(projectId: string, sessionId: string, id: string, mode: 'fresh' | 'resume'): Promise<import('./types').CollectionStatus> {
  return call('collection_refresh', { projectId, sessionId, id, mode });
}
export function authorizeCollection(projectId: string, sessionId: string, id: string, definitionFingerprint: string, accountEmail: string): Promise<import('./types').CollectionStatus> {
  return call('collection_authorize', { projectId, sessionId, id, definitionFingerprint, accountEmail });
}
export function collectionStatus(projectId: string, sessionId: string, id: string): Promise<import('./types').CollectionStatus> {
  return call('collection_status', { projectId, sessionId, id });
}
export function cancelCollection(projectId: string, sessionId: string, id: string, jobId: string): Promise<import('./types').CollectionStatus> {
  return call('collection_cancel', { projectId, sessionId, id, jobId });
}
export function collectionMembers(projectId: string, sessionId: string, id: string, offset = 0, snapshotId: string | null = null): Promise<import('./types').CollectionMemberPage> {
  return call('collection_members', { projectId, sessionId, id, offset, snapshotId });
}
export function readCollectionMember(projectId: string, sessionId: string, id: string, occurrenceId: string, snapshotId: string): Promise<import('./materials').MaterialRead> {
  return call('collection_read_member', { projectId, sessionId, id, occurrenceId, snapshotId });
}
