import { describe, expect, it } from 'vitest';
import { branchIsActionableOnShelf } from './branchShelf';
import type { BranchCard } from './types';

function branch(overrides: Partial<BranchCard> = {}): BranchCard {
  return {
    run_id: 'run-1',
    branch_id: 'branch-1',
    document_id: 'document-1',
    candidate_id: 'candidate-1',
    source_revision_id: 'revision-current',
    target_start_byte: 4,
    target_end_byte: 4,
    text: ' possible',
    output_blob_id: 'blob-1',
    output_byte_len: 9,
    status: 'ready',
    seed: '7',
    model_id: 'writer-1',
    selection: null,
    error: null,
    error_truncated: false,
    created_at_unix_ms: 1,
    ...overrides
  };
}

describe('branchIsActionableOnShelf', () => {
  it('shows only promotable current-revision candidates in steady state', () => {
    expect(branchIsActionableOnShelf(branch(), 'revision-current')).toBe(true);
    expect(branchIsActionableOnShelf(branch({ selection: 'keep_alternative' }), 'revision-current'))
      .toBe(true);
    expect(branchIsActionableOnShelf(branch({ candidate_id: null }), 'revision-current'))
      .toBe(false);
    expect(branchIsActionableOnShelf(branch({ selection: 'promote' }), 'revision-current'))
      .toBe(false);
    expect(branchIsActionableOnShelf(branch({ selection: 'reject' }), 'revision-current'))
      .toBe(false);
  });

  it('keeps current failures inspectable without surfacing active or historical runs', () => {
    expect(branchIsActionableOnShelf(branch({ status: 'failed', candidate_id: null }), 'revision-current'))
      .toBe(true);
    expect(branchIsActionableOnShelf(branch({ status: 'interrupted', candidate_id: null }), 'revision-current'))
      .toBe(true);
    for (const status of ['queued', 'generating', 'cancelled', 'pruned', 'rejected'] as const) {
      expect(branchIsActionableOnShelf(branch({ status }), 'revision-current')).toBe(false);
    }
    expect(branchIsActionableOnShelf(
      branch({ source_revision_id: 'revision-old' }),
      'revision-current'
    )).toBe(false);
  });
});
