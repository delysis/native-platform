import { describe, expect, it } from 'vitest';
import {
  appendUniquePage,
  branchBodyDisposition,
  mergeNewestPage
} from './branchPaging';
import type { BranchSummary } from './types';

function summary(overrides: Partial<BranchSummary> = {}): BranchSummary {
  return {
    run_id: 'run-1',
    branch_id: 'branch-1',
    document_id: 'document-1',
    candidate_id: 'candidate-1',
    source_revision_id: 'revision-1',
    target_start_byte: 5,
    target_end_byte: 5,
    output_blob_id: 'blob-1',
    output_byte_len: 12,
    status: 'ready',
    seed: '7',
    model_id: 'test/model',
    selection: null,
    error: null,
    error_truncated: false,
    created_at_unix_ms: 1,
    ...overrides
  };
}

describe('bounded branch paging', () => {
  it('refreshes newest metadata without dropping explicitly loaded older rows', () => {
    const refreshed = summary({ status: 'ready' });
    const stale = summary({ status: 'generating' });
    const older = summary({ run_id: 'run-older', branch_id: 'branch-older' });
    expect(mergeNewestPage([refreshed], [stale, older])).toEqual([refreshed, older]);
  });

  it('deduplicates a repeated cursor boundary while appending older rows', () => {
    const first = summary();
    const older = summary({ run_id: 'run-older', branch_id: 'branch-older' });
    expect(appendUniquePage([first], [first, older])).toEqual([first, older]);
  });

  it('fetches only changed in-budget bodies and rejects oversized previews locally', () => {
    const branch = summary();
    expect(branchBodyDisposition(branch, undefined, 1024)).toBe('fetch');
    expect(branchBodyDisposition(branch, 'blob-1', 1024)).toBe('cached');
    expect(branchBodyDisposition(summary({ output_byte_len: 2048 }), undefined, 1024))
      .toBe('too_large');
    expect(branchBodyDisposition(summary({ output_blob_id: null, output_byte_len: null }), undefined, 1024))
      .toBe('absent');
  });
});
