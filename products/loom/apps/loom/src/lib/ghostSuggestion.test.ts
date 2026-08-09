import { describe, expect, it } from 'vitest';
import type { BranchCard } from './types';
import {
  ghostReviewAffordance,
  selectVerifiedGhostSuggestion,
  verifiedGhostSuggestion,
  visibleVerifiedGhostSuggestion
} from './ghostSuggestion';

describe('ghostReviewAffordance', () => {
  it('keeps a compact readable review action for one active suggestion', () => {
    expect(ghostReviewAffordance(true, 1)).toEqual({
      visible: true,
      label: 'Review',
      ariaLabel: 'Review the current writing suggestion'
    });
  });

  it('describes additional and non-inline alternatives without empty controls', () => {
    expect(ghostReviewAffordance(true, 3)).toEqual({
      visible: true,
      label: '2 more',
      ariaLabel: 'Review the current writing suggestion and 2 more alternatives'
    });
    expect(ghostReviewAffordance(false, 1)).toEqual({
      visible: true,
      label: '1 alternative',
      ariaLabel: 'Review 1 writing alternative'
    });
    expect(ghostReviewAffordance(false, 0)).toEqual({
      visible: false,
      label: '',
      ariaLabel: ''
    });
  });
});

function branch(overrides: Partial<BranchCard> = {}): BranchCard {
  const text = ' rain.\n\nThen light.';
  return {
    run_id: 'run-1',
    branch_id: 'branch-1',
    candidate_id: 'candidate-1',
    source_revision_id: 'revision-1',
    target_start_byte: 9,
    target_end_byte: 9,
    text,
    output_blob_id: 'blob-1',
    output_byte_len: new TextEncoder().encode(text).byteLength,
    status: 'ready',
    seed: '7',
    model_id: 'model-1',
    selection: null,
    error: null,
    error_truncated: false,
    created_at_unix_ms: 1,
    ...overrides
  };
}

describe('verifiedGhostSuggestion', () => {
  it('returns exact text only after immutable branch-body hydration', () => {
    expect(verifiedGhostSuggestion(branch(), 'blob-1')).toEqual({
      candidateId: 'candidate-1',
      presentationKey: 'candidate-1:blob-1',
      text: ' rain.\n\nThen light.'
    });
  });

  it('fails closed for live text, identity mismatch, length mismatch, or non-ready state', () => {
    expect(verifiedGhostSuggestion(branch(), undefined)).toBeNull();
    expect(verifiedGhostSuggestion(branch(), 'another-blob')).toBeNull();
    expect(verifiedGhostSuggestion(branch({ output_byte_len: 1 }), 'blob-1')).toBeNull();
    expect(verifiedGhostSuggestion(branch({ status: 'generating' }), 'blob-1')).toBeNull();
  });

  it('measures UTF-8 bytes rather than JavaScript code units', () => {
    const text = ' 🌧️';
    const correctLength = new TextEncoder().encode(text).byteLength;
    expect(verifiedGhostSuggestion(branch({ text, output_byte_len: correctLength }), 'blob-1')?.text)
      .toBe(text);
    expect(verifiedGhostSuggestion(branch({ text, output_byte_len: text.length }), 'blob-1'))
      .toBeNull();
  });
});

describe('visibleVerifiedGhostSuggestion', () => {
  it('exposes menu and announcement state only for the child-rendered identity', () => {
    const suggestion = verifiedGhostSuggestion(branch(), 'blob-1');
    expect(visibleVerifiedGhostSuggestion(suggestion, '')).toBeNull();
    expect(visibleVerifiedGhostSuggestion(suggestion, 'candidate-1:another-blob')).toBeNull();
    expect(visibleVerifiedGhostSuggestion(suggestion, 'candidate-1:blob-1')).toBe(suggestion);
  });
});

describe('selectVerifiedGhostSuggestion', () => {
  it('reacts to an explicit immutable-body and caret snapshot', () => {
    const waiting = {
      active: true,
      branches: [branch()],
      hydratedBlobByRun: {},
      dismissedCandidateIds: [],
      targetByte: 9
    };
    expect(selectVerifiedGhostSuggestion(waiting)).toBeNull();
    expect(selectVerifiedGhostSuggestion({
      ...waiting,
      hydratedBlobByRun: { 'run-1': 'blob-1' }
    })).toEqual({
      candidateId: 'candidate-1',
      presentationKey: 'candidate-1:blob-1',
      text: ' rain.\n\nThen light.'
    });
  });

  it('fails closed away from the exact boundary or after dismissal', () => {
    const selection = {
      active: true,
      branches: [branch()],
      hydratedBlobByRun: { 'run-1': 'blob-1' },
      dismissedCandidateIds: [] as string[],
      targetByte: 9
    };
    expect(selectVerifiedGhostSuggestion({ ...selection, targetByte: 8 })).toBeNull();
    expect(selectVerifiedGhostSuggestion({
      ...selection,
      dismissedCandidateIds: ['candidate-1']
    })).toBeNull();
    expect(selectVerifiedGhostSuggestion({ ...selection, active: false })).toBeNull();
  });

  it('keeps degenerate model loops in provenance without presenting them', () => {
    const loop = ` She ${'her '.repeat(24)}`;
    expect(selectVerifiedGhostSuggestion({
      active: true,
      branches: [branch({
        text: loop,
        output_byte_len: new TextEncoder().encode(loop).byteLength
      })],
      hydratedBlobByRun: { 'run-1': 'blob-1' },
      dismissedCandidateIds: [],
      targetByte: 9
    })).toBeNull();
  });

  it('skips a hydrated c630-shaped loop and selects the next exact branch', () => {
    const loop = ` ${'Be'.repeat(180)}[image]\n\nS`;
    const clean = ' Beyond the wet glass, a bicycle bell answered.';
    const loopBranch = branch({
      text: loop,
      output_byte_len: new TextEncoder().encode(loop).byteLength
    });
    const cleanBranch = branch({
      run_id: 'run-2',
      branch_id: 'branch-2',
      candidate_id: 'candidate-2',
      text: clean,
      output_blob_id: 'blob-2',
      output_byte_len: new TextEncoder().encode(clean).byteLength
    });

    expect(selectVerifiedGhostSuggestion({
      active: true,
      branches: [loopBranch, cleanBranch],
      hydratedBlobByRun: { 'run-1': 'blob-1', 'run-2': 'blob-2' },
      dismissedCandidateIds: [],
      targetByte: 9
    })).toEqual({
      candidateId: 'candidate-2',
      presentationKey: 'candidate-2:blob-2',
      text: clean
    });
    expect(loopBranch.text).toBe(loop);
    expect(loopBranch.output_blob_id).toBe('blob-1');
  });
});
