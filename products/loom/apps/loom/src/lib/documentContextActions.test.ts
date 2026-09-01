import { describe, expect, it } from 'vitest';
import {
  MAX_DOCUMENT_TITLE_BYTES,
  boundedDocumentTitleInput,
  captureDocumentTarget,
  capturedDocumentBelongsToSession,
  capturedDocumentIdentityIsCurrent,
  clampDocumentMenuPoint,
  createDocumentRenameCompositionGuard,
  documentMenuKeyAction,
  documentRevealLabel,
  isDocumentContextTriggerKey,
  refreshDocumentRenameTarget
} from './documentContextActions';
import type { DocumentSummary, ProjectSnapshot } from './types';

function document(overrides: Partial<DocumentSummary> = {}): DocumentSummary {
  return {
    document_id: 'document-1',
    relative_path: 'manuscript/one.md',
    title: 'One',
    kind: 'prose',
    revision_id: 'revision-1',
    active_blob_id: 'a'.repeat(64),
    word_count: 12,
    externally_modified: false,
    ...overrides
  };
}

function project(summary = document()): ProjectSnapshot {
  return {
    project_id: 'project-1',
    session_id: 'session-1',
    title: 'Project',
    root: '/private/project',
    schema_version: 1,
    documents: [summary],
    pending_recovery: 0
  };
}

describe('captured document context target', () => {
  it('freezes exact store identity without carrying renderer path authority', () => {
    const summary = document();
    const captured = captureDocumentTarget(project(summary), summary);
    expect(captured).toEqual({
      projectId: 'project-1',
      sessionId: 'session-1',
      documentId: 'document-1',
      expectedRevisionId: 'revision-1',
      expectedBlobId: 'a'.repeat(64),
      title: 'One',
      kind: 'prose',
      wordCount: 12,
      externallyModified: false
    });
    expect(captured).not.toHaveProperty('relativePath');
    summary.revision_id = 'revision-2';
    expect(captured?.expectedRevisionId).toBe('revision-1');
    expect(Object.isFrozen(captured)).toBe(true);
  });

  it('refuses incomplete or already-divergent outline identity', () => {
    const missing = document({ revision_id: null });
    expect(captureDocumentTarget(project(missing), missing)).toBeNull();
    const stale = document();
    expect(captureDocumentTarget(project(document({ active_blob_id: 'b'.repeat(64) })), stale))
      .toBeNull();
  });

  it('binds later actions to the captured project session', () => {
    const captured = captureDocumentTarget(project(), document())!;
    expect(capturedDocumentBelongsToSession(captured, project())).toBe(true);
    expect(capturedDocumentBelongsToSession(captured, {
      project_id: 'project-1',
      session_id: 'session-2'
    })).toBe(false);
    expect(capturedDocumentBelongsToSession(captured, null)).toBe(false);
  });

  it('requires the captured revision and blob to remain current before applying a receipt', () => {
    const liveProject = project();
    const captured = captureDocumentTarget(liveProject, liveProject.documents[0])!;
    expect(capturedDocumentIdentityIsCurrent(captured, liveProject)).toBe(true);
    expect(capturedDocumentIdentityIsCurrent(captured, project(document({
      revision_id: 'revision-2',
      active_blob_id: 'b'.repeat(64)
    })))).toBe(false);
    expect(capturedDocumentIdentityIsCurrent(captured, {
      ...liveProject,
      session_id: 'session-2'
    })).toBe(false);
    expect(capturedDocumentIdentityIsCurrent(captured, null)).toBe(false);
  });

  it('flushes a current manuscript and recaptures the post-save rename identity', async () => {
    let liveProject = project();
    const captured = captureDocumentTarget(liveProject, liveProject.documents[0])!;
    let flushes = 0;

    const refreshed = await refreshDocumentRenameTarget(
      captured,
      captured.documentId,
      async () => {
        flushes += 1;
        liveProject = project(document({
          revision_id: 'revision-2',
          active_blob_id: 'b'.repeat(64)
        }));
        return true;
      },
      () => liveProject
    );

    expect(flushes).toBe(1);
    expect(refreshed).toMatchObject({
      expectedRevisionId: 'revision-2',
      expectedBlobId: 'b'.repeat(64)
    });
  });

  it('does not mint rename authority when a current save or project session changes', async () => {
    let liveProject = project();
    const captured = captureDocumentTarget(liveProject, liveProject.documents[0])!;
    expect(await refreshDocumentRenameTarget(
      captured,
      captured.documentId,
      async () => false,
      () => liveProject
    )).toBeNull();

    liveProject = { ...liveProject, session_id: 'session-2' };
    expect(await refreshDocumentRenameTarget(
      captured,
      'another-document',
      async () => true,
      () => liveProject
    )).toBeNull();
  });
});

describe('document title byte contract', () => {
  it('keeps exact UTF-8 prefixes at the native 256-byte ceiling', () => {
    expect(boundedDocumentTitleInput('a'.repeat(MAX_DOCUMENT_TITLE_BYTES)))
      .toBe('a'.repeat(MAX_DOCUMENT_TITLE_BYTES));
    expect(boundedDocumentTitleInput('😀'.repeat(65))).toBe('😀'.repeat(64));
    expect(new TextEncoder().encode(boundedDocumentTitleInput(`a${'é'.repeat(200)}`)).byteLength)
      .toBeLessThanOrEqual(MAX_DOCUMENT_TITLE_BYTES);
  });
});

describe('document rename composition ownership', () => {
  it('recognizes browser and legacy IME witnesses without consuming ordinary keys', () => {
    const guard = createDocumentRenameCompositionGuard();
    expect(guard.ownsCommandKey({ isComposing: false, keyCode: 13 })).toBe(false);
    expect(guard.ownsCommandKey({ isComposing: true, keyCode: 13 })).toBe(true);
    guard.reset();
    expect(guard.ownsCommandKey({ isComposing: false, keyCode: 229 })).toBe(true);
  });

  it('defers blur through compositionend and clears every pending intent on reset', () => {
    const guard = createDocumentRenameCompositionGuard();
    guard.start();
    expect(guard.active).toBe(true);
    expect(guard.blurShouldCommit()).toBe(false);
    expect(guard.finish()).toBe(true);
    expect(guard.active).toBe(false);
    expect(guard.finish()).toBe(false);

    guard.start();
    expect(guard.blurShouldCommit()).toBe(false);
    guard.reset();
    expect(guard.active).toBe(false);
    expect(guard.finish()).toBe(false);
    expect(guard.blurShouldCommit()).toBe(true);
  });
});

describe('document context menu keyboard contract', () => {
  it('opens only for Menu or unmodified Shift-F10', () => {
    expect(isDocumentContextTriggerKey({
      key: 'ContextMenu', shiftKey: false, altKey: false, ctrlKey: false, metaKey: false
    })).toBe(true);
    expect(isDocumentContextTriggerKey({
      key: 'F10', shiftKey: true, altKey: false, ctrlKey: false, metaKey: false
    })).toBe(true);
    expect(isDocumentContextTriggerKey({
      key: 'F10', shiftKey: false, altKey: false, ctrlKey: false, metaKey: false
    })).toBe(false);
    expect(isDocumentContextTriggerKey({
      key: 'ContextMenu', shiftKey: false, altKey: false, ctrlKey: true, metaKey: false
    })).toBe(false);
  });

  it('implements wrapping arrows, Home/End, activation, and Escape', () => {
    expect(documentMenuKeyAction({ key: 'ArrowDown' }, 2, 3)).toEqual({ kind: 'focus', index: 0 });
    expect(documentMenuKeyAction({ key: 'ArrowUp' }, 0, 3)).toEqual({ kind: 'focus', index: 2 });
    expect(documentMenuKeyAction({ key: 'Home' }, 2, 3)).toEqual({ kind: 'focus', index: 0 });
    expect(documentMenuKeyAction({ key: 'End' }, 0, 3)).toEqual({ kind: 'focus', index: 2 });
    expect(documentMenuKeyAction({ key: 'Enter' }, 1, 3)).toEqual({ kind: 'activate', index: 1 });
    expect(documentMenuKeyAction({ key: ' ' }, 2, 3)).toEqual({ kind: 'activate', index: 2 });
    expect(documentMenuKeyAction({ key: 'Escape' }, 0, 3)).toEqual({ kind: 'dismiss' });
    expect(documentMenuKeyAction({ key: 'Tab' }, 0, 3)).toEqual({ kind: 'none' });
  });

  it('clamps pointer and keyboard anchors within the visible viewport', () => {
    expect(clampDocumentMenuPoint({ x: 990, y: 790 }, 220, 150, 1000, 800))
      .toEqual({ x: 772, y: 642 });
    expect(clampDocumentMenuPoint({ x: -5, y: Number.NaN }, 220, 150, 1000, 800))
      .toEqual({ x: 8, y: 8 });
  });
});

describe('platform reveal wording', () => {
  it('uses native desktop language and omits unsupported mobile platforms', () => {
    expect(documentRevealLabel('MacIntel', 'Mozilla/5.0')).toBe('Reveal in Finder');
    expect(documentRevealLabel('Win32', 'Mozilla/5.0')).toBe('Show in File Explorer');
    expect(documentRevealLabel('Linux x86_64', 'Mozilla/5.0')).toBe('Show in File Manager');
    expect(documentRevealLabel('Linux armv8l', 'Mozilla/5.0 (Linux; Android 15)')).toBeNull();
    expect(documentRevealLabel('iPhone', 'Mozilla/5.0 (iPhone)')).toBeNull();
  });
});
