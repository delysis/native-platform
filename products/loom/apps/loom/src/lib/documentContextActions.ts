import type { DocumentKind, DocumentSummary, ProjectSnapshot } from './types';

export type DocumentContextAction = 'open' | 'rename' | 'export_text' | 'reveal';
export const MAX_DOCUMENT_TITLE_BYTES = 256;

/**
 * Immutable renderer-side presentation target captured when the menu opens.
 *
 * Deliberately absent: a filesystem path. Native document commands resolve
 * the registered path from this exact store identity and never accept path
 * authority from the renderer.
 */
export interface CapturedDocumentTarget {
  readonly projectId: string;
  readonly sessionId: string;
  readonly documentId: string;
  readonly expectedRevisionId: string;
  readonly expectedBlobId: string;
  readonly title: string;
  readonly kind: DocumentKind;
  readonly wordCount: number;
  readonly externallyModified: boolean;
}

export interface MenuPoint {
  readonly x: number;
  readonly y: number;
}

export interface DocumentRenameCompositionGuard {
  readonly active: boolean;
  reset(): void;
  start(): void;
  ownsCommandKey(
    event: Pick<KeyboardEvent, 'isComposing' | 'keyCode'>
  ): boolean;
  blurShouldCommit(): boolean;
  finish(): boolean;
}

/**
 * Own the rename input's IME lifetime independently from manuscript editing.
 *
 * WebKit may report an active composition through either `isComposing` or the
 * legacy 229 key code, and may deliver blur before compositionend. The guard
 * keeps those cases from committing a partial title while preserving the
 * explicit blur as a deferred commit request.
 */
export function createDocumentRenameCompositionGuard(): DocumentRenameCompositionGuard {
  let active = false;
  let commitAfterBlur = false;
  return {
    get active() {
      return active;
    },
    reset() {
      active = false;
      commitAfterBlur = false;
    },
    start() {
      active = true;
    },
    ownsCommandKey(event) {
      if (event.isComposing || event.keyCode === 229) active = true;
      return active;
    },
    blurShouldCommit() {
      if (!active) return true;
      commitAfterBlur = true;
      return false;
    },
    finish() {
      active = false;
      const shouldCommit = commitAfterBlur;
      commitAfterBlur = false;
      return shouldCommit;
    }
  };
}

export type DocumentMenuKeyAction =
  | { readonly kind: 'focus'; readonly index: number }
  | { readonly kind: 'activate'; readonly index: number }
  | { readonly kind: 'dismiss' }
  | { readonly kind: 'none' };

export function captureDocumentTarget(
  project: Pick<ProjectSnapshot, 'project_id' | 'session_id' | 'documents'>,
  summary: DocumentSummary
): CapturedDocumentTarget | null {
  const registered = project.documents.find(
    (candidate) => candidate.document_id === summary.document_id
  );
  if (
    !registered ||
    !summary.revision_id ||
    !summary.active_blob_id ||
    !registered.revision_id ||
    !registered.active_blob_id ||
    registered.revision_id !== summary.revision_id ||
    registered.active_blob_id !== summary.active_blob_id
  ) return null;

  return Object.freeze({
    projectId: project.project_id,
    sessionId: project.session_id,
    documentId: registered.document_id,
    expectedRevisionId: registered.revision_id,
    expectedBlobId: registered.active_blob_id,
    title: registered.title,
    kind: registered.kind,
    wordCount: registered.word_count,
    externallyModified: registered.externally_modified
  });
}

export function capturedDocumentBelongsToSession(
  target: CapturedDocumentTarget,
  project: Pick<ProjectSnapshot, 'project_id' | 'session_id'> | null
): boolean {
  return project?.project_id === target.projectId && project.session_id === target.sessionId;
}

/**
 * Recheck the renderer's live store identity before applying a command receipt.
 * Session equality alone is insufficient: a concurrent checkpoint may have
 * advanced the document while the native command was in flight.
 */
export function capturedDocumentIdentityIsCurrent(
  target: CapturedDocumentTarget,
  project: Pick<ProjectSnapshot, 'project_id' | 'session_id' | 'documents'> | null
): boolean {
  if (!project || !capturedDocumentBelongsToSession(target, project)) return false;
  const current = project.documents.find(
    (candidate) => candidate.document_id === target.documentId
  );
  return current?.revision_id === target.expectedRevisionId &&
    current.active_blob_id === target.expectedBlobId;
}

/**
 * Flush a current manuscript before freezing rename authority, then recapture
 * the revision/blob pair that is authoritative after that save.
 */
export async function refreshDocumentRenameTarget(
  target: CapturedDocumentTarget,
  currentDocumentId: string | null,
  flushCurrentDocument: () => Promise<boolean>,
  currentProject: () => ProjectSnapshot | null
): Promise<CapturedDocumentTarget | null> {
  const before = currentProject();
  if (!capturedDocumentBelongsToSession(target, before)) return null;
  if (target.documentId === currentDocumentId && !(await flushCurrentDocument())) return null;
  const project = currentProject();
  if (!project || !capturedDocumentBelongsToSession(target, project)) return null;
  const summary = project.documents.find(
    (candidate) => candidate.document_id === target.documentId
  );
  return summary ? captureDocumentTarget(project, summary) : null;
}

/** Match the native store's UTF-8 byte ceiling without splitting a code point. */
export function boundedDocumentTitleInput(value: string): string {
  let bytes = 0;
  let codeUnits = 0;
  for (const character of value) {
    const codePoint = character.codePointAt(0) ?? 0;
    const characterBytes = codePoint <= 0x7f ? 1 : codePoint <= 0x7ff ? 2 : codePoint <= 0xffff ? 3 : 4;
    if (bytes + characterBytes > MAX_DOCUMENT_TITLE_BYTES) break;
    bytes += characterBytes;
    codeUnits += character.length;
  }
  return codeUnits === value.length ? value : value.slice(0, codeUnits);
}

/** Release Svelte's disabled state before restoring the failed rename editor. */
export async function releaseDocumentRenameAndRestoreFocus(
  releaseBusyState: () => void,
  afterDomUpdate: () => Promise<void>,
  currentInput: () => HTMLInputElement | null | undefined
): Promise<boolean> {
  releaseBusyState();
  await afterDomUpdate();
  const input = currentInput();
  if (!input || input.disabled || !input.isConnected) return false;
  input.focus();
  input.select();
  return input.ownerDocument.activeElement === input;
}

export function isDocumentContextTriggerKey(
  event: Pick<KeyboardEvent, 'key' | 'shiftKey' | 'altKey' | 'ctrlKey' | 'metaKey'>
): boolean {
  if (event.altKey || event.ctrlKey || event.metaKey) return false;
  return event.key === 'ContextMenu' || (event.shiftKey && event.key === 'F10');
}

export function documentMenuKeyAction(
  event: Pick<KeyboardEvent, 'key'>,
  currentIndex: number,
  itemCount: number
): DocumentMenuKeyAction {
  if (!Number.isInteger(itemCount) || itemCount <= 0) return { kind: 'none' };
  const current = Number.isInteger(currentIndex)
    ? Math.min(Math.max(currentIndex, 0), itemCount - 1)
    : 0;
  switch (event.key) {
    case 'ArrowDown':
      return { kind: 'focus', index: (current + 1) % itemCount };
    case 'ArrowUp':
      return { kind: 'focus', index: (current - 1 + itemCount) % itemCount };
    case 'Home':
      return { kind: 'focus', index: 0 };
    case 'End':
      return { kind: 'focus', index: itemCount - 1 };
    case 'Enter':
    case ' ':
      return { kind: 'activate', index: current };
    case 'Escape':
      return { kind: 'dismiss' };
    default:
      return { kind: 'none' };
  }
}

export function clampDocumentMenuPoint(
  requested: MenuPoint,
  menuWidth: number,
  menuHeight: number,
  viewportWidth: number,
  viewportHeight: number,
  margin = 8
): MenuPoint {
  const width = Number.isFinite(menuWidth) ? Math.max(0, menuWidth) : 0;
  const height = Number.isFinite(menuHeight) ? Math.max(0, menuHeight) : 0;
  const viewportX = Number.isFinite(viewportWidth) ? Math.max(0, viewportWidth) : 0;
  const viewportY = Number.isFinite(viewportHeight) ? Math.max(0, viewportHeight) : 0;
  const inset = Number.isFinite(margin) ? Math.max(0, margin) : 0;
  const maximumX = Math.max(inset, viewportX - width - inset);
  const maximumY = Math.max(inset, viewportY - height - inset);
  return {
    x: Math.min(Math.max(Number.isFinite(requested.x) ? requested.x : inset, inset), maximumX),
    y: Math.min(Math.max(Number.isFinite(requested.y) ? requested.y : inset, inset), maximumY)
  };
}

export function documentRevealLabel(platform: string, userAgent: string): string | null {
  const normalizedPlatform = platform.toLowerCase();
  const normalizedAgent = userAgent.toLowerCase();
  if (/android|iphone|ipad|ipod/.test(normalizedAgent)) return null;
  if (normalizedPlatform.includes('mac')) return 'Reveal in Finder';
  if (normalizedPlatform.includes('win')) return 'Show in File Explorer';
  if (/linux|freebsd|openbsd|netbsd|dragonfly/.test(normalizedPlatform)) {
    return 'Show in File Manager';
  }
  return null;
}
