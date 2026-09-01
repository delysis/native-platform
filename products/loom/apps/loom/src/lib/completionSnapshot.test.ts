import { describe, expect, it } from 'vitest';
import { completionSnapshotFacts, type CompletionSnapshotScope } from './completionSnapshot';
import type {
  BranchSummary,
  CompletionPartialTextSnapshot,
  CompletionSnapshot
} from './types';

const scope: CompletionSnapshotScope = {
  projectId: 'project-1',
  sessionId: 'session-1',
  documentId: 'document-1'
};

const branch: BranchSummary = {
  run_id: 'run-1',
  branch_id: 'branch-1',
  weave_command_id: '01K00000000000000000000001',
  document_id: scope.documentId,
  candidate_id: null,
  source_revision_id: 'revision-1',
  target_start_byte: 5,
  target_end_byte: 5,
  output_blob_id: null,
  output_byte_len: null,
  status: 'generating',
  seed: '1',
  model_id: 'model-1',
  selection: null,
  error: null,
  error_truncated: false,
  created_at_unix_ms: 1
};

function snapshot(partialText: CompletionPartialTextSnapshot | null): CompletionSnapshot {
  return {
    project_id: scope.projectId,
    session_id: scope.sessionId,
    document_id: scope.documentId,
    active_operations: [{
      request_id: 'request-1',
      attempt_id: 'request-1:1',
      operation_sequence: '1',
      phase: 'running',
      cancellation_requested: false,
      authoritative_terminal: null,
      final_projection: null,
      progress_sequences: ['1', '2', '3'],
      branches: [{
        run_id: branch.run_id,
        branch_id: branch.branch_id,
        partial_text: partialText
      }]
    }],
    branches: [branch],
    next_cursor: null,
    has_more: false
  };
}

describe('completion snapshot partial-text projection', () => {
  it('returns full replacement text and sequence maps keyed by exact active run', () => {
    const first = completionSnapshotFacts(snapshot({
      text: 'échoes 🙂',
      sequence: '18446744073709551615',
      utf8_byte_len: '12'
    }), scope);

    expect(first).toEqual({
      activeRunIds: ['run-1'],
      cancellationRequestedRunIds: [],
      liveTextByRun: { 'run-1': 'échoes 🙂' },
      liveTextSequenceByRun: { 'run-1': '18446744073709551615' }
    });

    const next = completionSnapshotFacts(snapshot(null), scope);
    expect(next.liveTextByRun).toEqual({});
    expect(next.liveTextSequenceByRun).toEqual({});
    expect(first.liveTextByRun).toEqual({ 'run-1': 'échoes 🙂' });
  });

  it.each([
    '',
    '01',
    '-1',
    '1.0',
    '18446744073709551616'
  ])('rejects non-canonical text-delta sequence %j', (sequence) => {
    expect(() => completionSnapshotFacts(snapshot({
      text: 'hello',
      sequence,
      utf8_byte_len: '5'
    }), scope)).toThrow('invalid completion partial-text projection');
  });

  it('rejects a UTF-16 or otherwise forged byte length', () => {
    expect(() => completionSnapshotFacts(snapshot({
      text: 'é',
      sequence: '2',
      utf8_byte_len: '1'
    }), scope)).toThrow('invalid completion partial-text projection');
  });

  it('rejects a cumulative partial above the native IPC projection bound', () => {
    const text = 'x'.repeat(256 * 1024 + 1);
    expect(() => completionSnapshotFacts(snapshot({
      text,
      sequence: '2',
      utf8_byte_len: String(text.length)
    }), scope)).toThrow('invalid completion partial-text projection');
  });

  it('rejects a missing partial-text field instead of retaining stale renderer text', () => {
    const forged = snapshot(null);
    delete (forged.active_operations[0].branches[0] as Partial<
      typeof forged.active_operations[0]['branches'][0]
    >).partial_text;

    expect(() => completionSnapshotFacts(forged, scope))
      .toThrow('invalid completion partial-text projection');
  });
});
