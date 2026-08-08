import { invoke } from '@tauri-apps/api/core';
import type {
  CommandReceipt,
  DocumentKind,
  ModelCapabilitySummary,
  OpenDocument,
  ProjectCloseReceipt,
  ProjectSnapshot,
  RecoveryReport,
  ReconciliationPreview,
  LoomFailure,
  TransientDraftSnapshot,
  TransientDraftWriteReceipt
} from './types';

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

export function chooseAndOpenProject(): Promise<ProjectSnapshot> {
  return call('project_choose_open');
}

export function currentProjectSession(): Promise<ProjectSnapshot> {
  return call('project_current');
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

export function setFocusMode(
  projectId: string,
  sessionId: string,
  enabled: boolean
): Promise<void> {
  return call('focus_mode_set', { projectId, sessionId, enabled });
}

export function requestApplicationClose(): Promise<void> {
  return call('application_close');
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
