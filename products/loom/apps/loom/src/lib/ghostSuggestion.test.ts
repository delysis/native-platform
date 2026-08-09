import { describe, expect, it } from 'vitest';
import type { BranchCard } from './types';
import { verifiedGhostSuggestion } from './ghostSuggestion';

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
