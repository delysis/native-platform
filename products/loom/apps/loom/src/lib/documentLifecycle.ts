import type { DocumentSummary } from './types';

export interface DocumentRefreshDecision {
  readonly current: DocumentSummary | null;
  readonly successor: DocumentSummary | null;
  readonly currentDisappeared: boolean;
}

export interface DocumentBoundaryFacts {
  readonly sourceDirty: boolean;
  readonly editVersion: number;
  readonly savedVersion: number;
  readonly saveState: 'clean' | 'dirty' | 'saving' | 'saved' | 'uncertain' | 'error';
  readonly saveInFlight: boolean;
  readonly draftInFlight: boolean;
  readonly uncertainSave: boolean;
  readonly uncertainDraft: boolean;
}

export interface ProjectFilesystemRefreshBoundaryState {
  readonly projectToken: object | null;
  readonly documentId: string | null;
  readonly documentEpoch: number;
  readonly editVersion: number;
  readonly navigationSerial: number;
  readonly lifecycleIdle: boolean;
}

export type ProjectFilesystemRefreshBoundaryCapture = Omit<
  ProjectFilesystemRefreshBoundaryState,
  'lifecycleIdle'
>;

export type ProjectFilesystemRefreshBoundaryDisposition =
  | { readonly kind: 'current' }
  | {
      readonly kind: 'retry';
      readonly reason:
        | 'lifecycle_busy'
        | 'project_projection_changed'
        | 'navigation_changed'
        | 'document_changed'
        | 'editor_changed';
    };

export type GuardedProjectFilesystemRefreshResult =
  | { readonly kind: 'applied' }
  | Exclude<ProjectFilesystemRefreshBoundaryDisposition, { readonly kind: 'current' }>;

export function captureProjectFilesystemRefreshBoundary(
  state: ProjectFilesystemRefreshBoundaryState
): ProjectFilesystemRefreshBoundaryCapture {
  return {
    projectToken: state.projectToken,
    documentId: state.documentId,
    documentEpoch: state.documentEpoch,
    editVersion: state.editVersion,
    navigationSerial: state.navigationSerial
  };
}

/**
 * A native project pull is only safe to project while the exact renderer
 * boundary that requested it is still current. File commands replace the
 * project object; navigation and edits advance their own monotonic witnesses.
 */
export function projectFilesystemRefreshBoundaryDisposition(
  captured: ProjectFilesystemRefreshBoundaryCapture,
  current: ProjectFilesystemRefreshBoundaryState
): ProjectFilesystemRefreshBoundaryDisposition {
  if (!current.lifecycleIdle) return { kind: 'retry', reason: 'lifecycle_busy' };
  if (current.projectToken !== captured.projectToken) {
    return { kind: 'retry', reason: 'project_projection_changed' };
  }
  if (current.navigationSerial !== captured.navigationSerial) {
    return { kind: 'retry', reason: 'navigation_changed' };
  }
  if (
    current.documentId !== captured.documentId ||
    current.documentEpoch !== captured.documentEpoch
  ) {
    return { kind: 'retry', reason: 'document_changed' };
  }
  if (current.editVersion !== captured.editVersion) {
    return { kind: 'retry', reason: 'editor_changed' };
  }
  return { kind: 'current' };
}

/** Resolve and apply a native pull only if its renderer boundary stayed exact. */
export async function applyGuardedProjectFilesystemRefresh<T>(
  captured: ProjectFilesystemRefreshBoundaryCapture,
  pull: () => Promise<T>,
  current: () => ProjectFilesystemRefreshBoundaryState,
  apply: (value: T) => void | Promise<void>
): Promise<GuardedProjectFilesystemRefreshResult> {
  const value = await pull();
  const disposition = projectFilesystemRefreshBoundaryDisposition(captured, current());
  if (disposition.kind === 'retry') return disposition;
  await apply(value);
  return { kind: 'applied' };
}

export function documentBoundaryNeedsRecovery(facts: DocumentBoundaryFacts): boolean {
  return facts.sourceDirty ||
    facts.editVersion !== facts.savedVersion ||
    facts.saveState === 'dirty' ||
    facts.saveState === 'saving' ||
    facts.saveState === 'uncertain' ||
    facts.saveState === 'error' ||
    facts.saveInFlight ||
    facts.draftInFlight ||
    facts.uncertainSave ||
    facts.uncertainDraft;
}

export function missingDocumentJournalIsDurable(
  hadUnsavedText: boolean,
  journalSettled: boolean,
  uncertainDraft: boolean,
  draftSavedEditVersion: number,
  editVersion: number
): boolean {
  return !hadUnsavedText || Boolean(
    journalSettled && !uncertainDraft && draftSavedEditVersion >= editVersion
  );
}

export interface MissingDocumentRecoveryGuard {
  readonly documentId: string;
  readonly journalDurable: boolean;
  readonly copied: boolean;
}

export interface MissingDocumentRecoveryTextGuard extends MissingDocumentRecoveryGuard {
  readonly text: string;
}

export interface MissingDocumentCaptureIdentity {
  readonly projectId: string;
  readonly sessionId: string;
  readonly documentId: string;
  readonly revisionId: string | null;
  readonly blobId: string;
}

export type MissingDocumentCaptureAdmission =
  | {
      readonly kind: 'capture';
      readonly pending: MissingDocumentCaptureIdentity;
    }
  | {
      readonly kind: 'wait_for_recovery_copy';
      readonly pending: MissingDocumentCaptureIdentity;
    };

/** Whether the sole recovery authority still exists only in renderer memory. */
export function missingDocumentRecoveryRequiresCopy(
  existing: MissingDocumentRecoveryGuard | null
): boolean {
  return Boolean(existing && !existing.journalDurable && !existing.copied);
}

/** Never replace the sole in-memory recovery authority before it is safe. */
export function missingDocumentRecoveryBlocksReplacement(
  existing: MissingDocumentRecoveryGuard | null,
  _incomingDocumentId: string
): boolean {
  return missingDocumentRecoveryRequiresCopy(existing);
}

/**
 * A recreated file with the same stable ID may clear recovery only when the
 * recovery is already safe or its complete exact text is present again.
 */
export function openedDocumentSubsumesMissingRecovery(
  recovery: MissingDocumentRecoveryTextGuard,
  openedDocumentId: string,
  openedText: string
): boolean {
  return recovery.documentId === openedDocumentId && Boolean(
    recovery.journalDurable || recovery.copied || recovery.text === openedText
  );
}

function sameMissingDocumentCaptureIdentity(
  left: MissingDocumentCaptureIdentity,
  right: MissingDocumentCaptureIdentity
): boolean {
  return left.projectId === right.projectId &&
    left.sessionId === right.sessionId &&
    left.documentId === right.documentId &&
    left.revisionId === right.revisionId &&
    left.blobId === right.blobId;
}

/**
 * Lock a newly missing mounted manuscript before projecting its omission.
 * Repeated native hints reuse the exact pending authority. A prior recovery
 * that exists only in memory must be copied before this capture can replace it.
 */
export function beginMissingDocumentCaptureBoundary(
  existing: MissingDocumentRecoveryGuard | null,
  incoming: MissingDocumentCaptureIdentity,
  pending: MissingDocumentCaptureIdentity | null
): MissingDocumentCaptureAdmission {
  const exactPending = pending && sameMissingDocumentCaptureIdentity(pending, incoming)
    ? pending
    : Object.freeze({ ...incoming });
  return missingDocumentRecoveryBlocksReplacement(existing, incoming.documentId)
    ? { kind: 'wait_for_recovery_copy', pending: exactPending }
    : { kind: 'capture', pending: exactPending };
}

/**
 * Reconcile one authoritative native document snapshot with the renderer's
 * current selection. When the selected file disappeared, prefer the next
 * surviving outline entry, then the previous one. This preserves spatial
 * context without trusting filesystem paths in the renderer.
 */
export function documentRefreshDecision(
  previous: readonly DocumentSummary[],
  refreshed: readonly DocumentSummary[],
  currentDocumentId: string | null
): DocumentRefreshDecision {
  if (!currentDocumentId) {
    return { current: null, successor: null, currentDisappeared: false };
  }

  const current = refreshed.find((candidate) => candidate.document_id === currentDocumentId) ?? null;
  if (current) return { current, successor: null, currentDisappeared: false };

  const previousIndex = previous.findIndex(
    (candidate) => candidate.document_id === currentDocumentId
  );
  if (previousIndex < 0) {
    return {
      current: null,
      successor: refreshed[0] ?? null,
      currentDisappeared: true
    };
  }

  const refreshedById = new Map(
    refreshed.map((candidate) => [candidate.document_id, candidate] as const)
  );
  for (let index = previousIndex + 1; index < previous.length; index += 1) {
    const survivor = refreshedById.get(previous[index].document_id);
    if (survivor) return { current: null, successor: survivor, currentDisappeared: true };
  }
  for (let index = previousIndex - 1; index >= 0; index -= 1) {
    const survivor = refreshedById.get(previous[index].document_id);
    if (survivor) return { current: null, successor: survivor, currentDisappeared: true };
  }

  return {
    current: null,
    successor: refreshed[Math.min(previousIndex, refreshed.length - 1)] ?? null,
    currentDisappeared: true
  };
}
