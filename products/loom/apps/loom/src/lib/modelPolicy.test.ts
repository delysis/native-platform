import { describe, expect, it } from 'vitest';
import type { ModelCapabilitySummary } from './types';
import { isVerifiedPolicyWriter, orderedLocalWriterCandidates } from './modelPolicy';

function model(overrides: Partial<ModelCapabilitySummary> = {}): ModelCapabilitySummary {
  return {
    model_id: 'discovered:1',
    display_name: 'writer.gguf',
    local: true,
    loaded: false,
    chat: false,
    completion: false,
    fill_in_middle: false,
    output_tokens: false,
    logprobs: false,
    model_path: '/models/writer.gguf',
    file_bytes: 42,
    header_verified: true,
    architecture: null,
    context_tokens: null,
    model_sha256: null,
    projector_present: null,
    media_kinds: [],
    policy_candidate: { profile_id: 'writer-v1', rank: 0 },
    policy_verified: null,
    tested_profile: null,
    ...overrides
  };
}

describe('orderedLocalWriterCandidates', () => {
  it('is permutation-independent and considers only local verified size hints', () => {
    const a = model({ model_path: '/z.gguf' });
    const b = model({ model_path: '/a.gguf' });
    const remote = model({ model_path: '/remote.gguf', local: false });
    const unverified = model({ model_path: '/bad.gguf', header_verified: false });
    const unrelated = model({ model_path: '/other.gguf', policy_candidate: null });

    expect(orderedLocalWriterCandidates([a, unrelated, b, remote, unverified])).toEqual([
      { modelPath: '/a.gguf', profileId: 'writer-v1', policyRank: 0 },
      { modelPath: '/z.gguf', profileId: 'writer-v1', policyRank: 0 }
    ]);
    expect(orderedLocalWriterCandidates([b, a])).toEqual(
      orderedLocalWriterCandidates([a, b])
    );
  });

  it('preserves policy rank ahead of path ordering', () => {
    const lowerPriority = model({
      model_path: '/a.gguf',
      policy_candidate: { profile_id: 'writer-later', rank: 4 }
    });
    const preferred = model({
      model_path: '/z.gguf',
      policy_candidate: { profile_id: 'writer-first', rank: 1 }
    });

    expect(orderedLocalWriterCandidates([lowerPriority, preferred])).toEqual([
      { modelPath: '/z.gguf', profileId: 'writer-first', policyRank: 1 },
      { modelPath: '/a.gguf', profileId: 'writer-later', policyRank: 4 }
    ]);
  });
});

describe('isVerifiedPolicyWriter', () => {
  it('does not mistake a size hint for exact native policy evidence', () => {
    expect(isVerifiedPolicyWriter(model(), 'writer-v1')).toBe(false);
    expect(isVerifiedPolicyWriter(model({
      loaded: true,
      completion: true,
      output_tokens: true,
      policy_verified: { profile_id: 'writer-v1', rank: 0 },
      tested_profile: 'writer-v1'
    }), 'writer-v1')).toBe(true);
  });

  it('fails closed when identity or required completion evidence is absent', () => {
    const loaded = model({ loaded: true, completion: true, output_tokens: true });
    expect(isVerifiedPolicyWriter(loaded, 'writer-v1')).toBe(false);
    expect(isVerifiedPolicyWriter({
      ...loaded,
      policy_verified: { profile_id: 'another', rank: 0 },
      tested_profile: 'another'
    }, 'writer-v1'))
      .toBe(false);
    expect(isVerifiedPolicyWriter({
      ...loaded,
      policy_verified: { profile_id: 'writer-v1', rank: 0 },
      tested_profile: 'writer-v1',
      output_tokens: false
    }, 'writer-v1'))
      .toBe(false);
  });
});
