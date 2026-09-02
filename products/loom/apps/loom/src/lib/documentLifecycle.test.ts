import { describe, expect, it, vi } from 'vitest';
import {
  applyGuardedProjectFilesystemRefresh,
  beginMissingDocumentCaptureBoundary,
  captureProjectFilesystemRefreshBoundary,
  documentBoundaryNeedsRecovery,
  documentRefreshDecision,
  missingDocumentJournalIsDurable,
  missingDocumentRecoveryBlocksReplacement,
  missingDocumentRecoveryRequiresCopy,
  openedDocumentSubsumesMissingRecovery,
  projectFilesystemRefreshBoundaryDisposition,
  type ProjectFilesystemRefreshBoundaryState
} from './documentLifecycle';
import type { DocumentSummary } from './types';

function document(document_id: string): DocumentSummary {
  return {
    document_id,
    relative_path: `manuscript/${document_id}.md`,
    title: document_id,
    kind: 'prose',
    revision_id: `revision-${document_id}`,
    active_blob_id: document_id.padEnd(64, '0').slice(0, 64),
    word_count: 1,
    externally_modified: false
  };
}

describe('authoritative document refresh selection', () => {
  it('keeps the selected document when its stable ID survives a rename', () => {
    const previous = [document('a'), document('b')];
    const renamed = {
      ...previous[0],
      title: 'Renamed',
      relative_path: 'manuscript/Renamed.md'
    };

    expect(documentRefreshDecision(previous, [renamed, previous[1]], 'a')).toEqual({
      current: renamed,
      successor: null,
      currentDisappeared: false
    });
  });

  it('selects the next survivor, then the previous survivor', () => {
    const previous = [document('a'), document('b'), document('c')];
    expect(documentRefreshDecision(previous, [previous[0], previous[2]], 'b').successor)
      .toBe(previous[2]);
    expect(documentRefreshDecision(previous, [previous[0]], 'b').successor)
      .toBe(previous[0]);
  });

  it('returns a clean empty selection when the only document disappears', () => {
    expect(documentRefreshDecision([document('a')], [], 'a')).toEqual({
      current: null,
      successor: null,
      currentDisappeared: true
    });
  });

  it('does not invent a selection when no document was open', () => {
    expect(documentRefreshDecision([document('a')], [document('a')], null)).toEqual({
      current: null,
      successor: null,
      currentDisappeared: false
    });
  });
});

describe('project filesystem refresh boundary', () => {
  function boundary(
    overrides: Partial<ProjectFilesystemRefreshBoundaryState> = {}
  ): ProjectFilesystemRefreshBoundaryState {
    return {
      projectToken: {},
      documentId: 'a',
      documentEpoch: 3,
      editVersion: 8,
      navigationSerial: 5,
      lifecycleIdle: true,
      ...overrides
    };
  }

  function deferred<T>(): {
    readonly promise: Promise<T>;
    readonly resolve: (value: T) => void;
  } {
    let resolve!: (value: T) => void;
    const promise = new Promise<T>((settle) => { resolve = settle; });
    return { promise, resolve };
  }

  it('preserves a newly selected and edited B when an older pull reports A missing', async () => {
    const started = boundary();
    const captured = captureProjectFilesystemRefreshBoundary(started);
    const nativePull = deferred<{ readonly missing: string; readonly successor: string }>();
    let live = started;
    let editor = { documentId: 'a' as string | null, text: 'A text' };
    const detach = vi.fn(() => { editor = { documentId: null, text: '' }; });
    const select = vi.fn((documentId: string) => {
      editor = { documentId, text: `${documentId} native text` };
    });

    const resolution = applyGuardedProjectFilesystemRefresh(
      captured,
      () => nativePull.promise,
      () => live,
      (snapshot) => {
        detach();
        select(snapshot.successor);
      }
    );

    editor = { documentId: 'b', text: 'B edited while refresh was pending' };
    live = boundary({
      projectToken: started.projectToken,
      documentId: 'b',
      documentEpoch: 4,
      editVersion: 9,
      navigationSerial: 6
    });
    nativePull.resolve({ missing: 'a', successor: 'c' });

    await expect(resolution).resolves.toEqual({ kind: 'retry', reason: 'navigation_changed' });
    expect(editor).toEqual({
      documentId: 'b',
      text: 'B edited while refresh was pending'
    });
    expect(detach).not.toHaveBeenCalled();
    expect(select).not.toHaveBeenCalled();
  });

  it('does not let a pre-rename pull overwrite the completed native path projection', async () => {
    const started = boundary();
    const captured = captureProjectFilesystemRefreshBoundary(started);
    const nativePull = deferred<{ readonly title: string; readonly relativePath: string }>();
    let live = started;
    let projection = {
      title: 'Visible title',
      relativePath: 'manuscript/Untitled-7.md'
    };
    const apply = vi.fn((stale: typeof projection) => { projection = stale; });
    const resolution = applyGuardedProjectFilesystemRefresh(
      captured,
      () => nativePull.promise,
      () => live,
      apply
    );

    projection = {
      title: 'Visible title',
      relativePath: 'manuscript/Visible-title.md'
    };
    live = boundary({
      ...started,
      projectToken: {}
    });
    nativePull.resolve({
      title: 'Visible title',
      relativePath: 'manuscript/Untitled-7.md'
    });

    await expect(resolution).resolves.toEqual({
      kind: 'retry',
      reason: 'project_projection_changed'
    });
    expect(projection.relativePath).toBe('manuscript/Visible-title.md');
    expect(apply).not.toHaveBeenCalled();
  });

  it('retries a pull when a file command is still in flight or the editor changed', () => {
    const started = boundary();
    const captured = captureProjectFilesystemRefreshBoundary(started);
    expect(projectFilesystemRefreshBoundaryDisposition(
      captured,
      boundary({ ...started, lifecycleIdle: false })
    )).toEqual({ kind: 'retry', reason: 'lifecycle_busy' });
    expect(projectFilesystemRefreshBoundaryDisposition(
      captured,
      boundary({ ...started, editVersion: started.editVersion + 1 })
    )).toEqual({ kind: 'retry', reason: 'editor_changed' });
  });
});

describe('missing current-document boundary', () => {
  const clean = {
    sourceDirty: false,
    editVersion: 4,
    savedVersion: 4,
    saveState: 'clean' as const,
    saveInFlight: false,
    draftInFlight: false,
    uncertainSave: false,
    uncertainDraft: false
  };

  it('requires recovery for dirty text and every pending or uncertain boundary', () => {
    expect(documentBoundaryNeedsRecovery(clean)).toBe(false);
    expect(documentBoundaryNeedsRecovery({ ...clean, editVersion: 5 })).toBe(true);
    expect(documentBoundaryNeedsRecovery({ ...clean, draftInFlight: true })).toBe(true);
    expect(documentBoundaryNeedsRecovery({ ...clean, saveInFlight: true })).toBe(true);
    expect(documentBoundaryNeedsRecovery({ ...clean, uncertainDraft: true })).toBe(true);
    expect(documentBoundaryNeedsRecovery({ ...clean, uncertainSave: true })).toBe(true);
  });

  it('calls a dirty missing document durable only after its exact edit version is journaled', () => {
    expect(missingDocumentJournalIsDurable(true, true, false, 7, 7)).toBe(true);
    expect(missingDocumentJournalIsDurable(true, true, false, 6, 7)).toBe(false);
    expect(missingDocumentJournalIsDurable(true, true, true, 7, 7)).toBe(false);
    expect(missingDocumentJournalIsDurable(true, false, false, 7, 7)).toBe(false);
    expect(missingDocumentJournalIsDurable(false, false, true, 0, 0)).toBe(true);
  });

  it('fails closed before replacing a different non-durable, uncopied recovery', () => {
    const existing = {
      documentId: 'first-missing',
      journalDurable: false,
      copied: false
    };

    expect(missingDocumentRecoveryBlocksReplacement(existing, 'second-missing')).toBe(true);
    expect(missingDocumentRecoveryBlocksReplacement(existing, 'first-missing')).toBe(true);
    expect(missingDocumentRecoveryBlocksReplacement({
      ...existing,
      journalDurable: true
    }, 'second-missing')).toBe(false);
    expect(missingDocumentRecoveryBlocksReplacement({
      ...existing,
      copied: true
    }, 'second-missing')).toBe(false);
    expect(missingDocumentRecoveryBlocksReplacement(null, 'second-missing')).toBe(false);
  });

  it('does not clear or overwrite unsafe recovery when the same document ID reappears', () => {
    const recovery = {
      documentId: 'a',
      text: 'A unsaved exact text',
      journalDurable: false,
      copied: false
    };
    const capture = {
      projectId: 'project',
      sessionId: 'session',
      documentId: 'a',
      revisionId: 'revision-a',
      blobId: 'a'.repeat(64)
    };

    expect(missingDocumentRecoveryRequiresCopy(recovery)).toBe(true);
    expect(openedDocumentSubsumesMissingRecovery(
      recovery,
      'a',
      'A recreated from the last saved bytes'
    )).toBe(false);
    expect(beginMissingDocumentCaptureBoundary(recovery, capture, null).kind)
      .toBe('wait_for_recovery_copy');

    expect(openedDocumentSubsumesMissingRecovery(
      recovery,
      'a',
      'A unsaved exact text'
    )).toBe(true);
    expect(openedDocumentSubsumesMissingRecovery(
      { ...recovery, copied: true },
      'a',
      'different bytes are safe after explicit copy'
    )).toBe(true);
    expect(openedDocumentSubsumesMissingRecovery(
      recovery,
      'different-document',
      recovery.text
    )).toBe(false);
  });

  it('retains and locks B when recovery A is unsafe, then admits the exact B capture after copy', () => {
    const recoveryA = {
      documentId: 'a',
      journalDurable: false,
      copied: false
    };
    const captureB = {
      projectId: 'project',
      sessionId: 'session',
      documentId: 'b',
      revisionId: 'revision-b',
      blobId: 'b'.repeat(64)
    };
    const projectedBeforeHint = {
      documents: [document('a'), document('b')]
    };
    const mountedEditor = {
      documentId: 'b',
      text: 'B text that has not been captured yet'
    };

    const blocked = beginMissingDocumentCaptureBoundary(recoveryA, captureB, null);
    const projectedWhileBlocked = blocked.kind === 'wait_for_recovery_copy'
      ? projectedBeforeHint
      : { documents: [document('a')] };

    expect(blocked).toEqual({
      kind: 'wait_for_recovery_copy',
      pending: captureB
    });
    expect(projectedWhileBlocked).toBe(projectedBeforeHint);
    expect(projectedWhileBlocked.documents.map(({ document_id }) => document_id))
      .toEqual(['a', 'b']);
    expect(mountedEditor).toEqual({
      documentId: 'b',
      text: 'B text that has not been captured yet'
    });
    expect(blocked.pending.documentId).toBe(mountedEditor.documentId);

    const repeatedHint = beginMissingDocumentCaptureBoundary(
      recoveryA,
      captureB,
      blocked.pending
    );
    expect(repeatedHint.kind).toBe('wait_for_recovery_copy');
    expect(repeatedHint.pending).toBe(blocked.pending);

    const afterCopy = beginMissingDocumentCaptureBoundary(
      { ...recoveryA, copied: true },
      captureB,
      blocked.pending
    );
    expect(afterCopy.kind).toBe('capture');
    expect(afterCopy.pending).toBe(blocked.pending);

    const reopenedSession = beginMissingDocumentCaptureBoundary(
      null,
      { ...captureB, sessionId: 'new-session' },
      blocked.pending
    );
    expect(reopenedSession.pending).not.toBe(blocked.pending);
    expect(reopenedSession.pending.sessionId).toBe('new-session');
  });
});
