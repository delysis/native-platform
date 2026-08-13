import { describe, expect, it } from 'vitest';
import type { ModelCapabilitySummary } from './types';
import {
  automaticWriterForBuildPolicy,
  isVerifiedPolicyWriter,
  orderedLocalWriterCandidates,
  preferredWriterModelPath,
  writerProfileForBuildPolicy
} from './modelPolicy';

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

describe('preferredWriterModelPath', () => {
  it('never selects a merely parseable unrelated model', () => {
    const incompatible = model({
      model_path: '/models/instruct.gguf',
      policy_candidate: null
    });
    const writer = model({ model_path: '/models/base-writer.gguf' });

    expect(preferredWriterModelPath(
      [incompatible, writer],
      incompatible.model_path,
      incompatible.model_path
    )).toBe(writer.model_path);
    expect(preferredWriterModelPath([incompatible], incompatible.model_path, '')).toBe('');
  });

  it('prefers resident evidence, then a remembered compatible candidate', () => {
    const first = model({ model_path: '/models/first.gguf' });
    const remembered = model({ model_path: '/models/remembered.gguf' });
    const resident = model({
      model_path: '/models/resident.gguf',
      loaded: true,
      policy_candidate: null
    });

    expect(preferredWriterModelPath([first, remembered], remembered.model_path, ''))
      .toBe(remembered.model_path);
    expect(preferredWriterModelPath([first, resident], first.model_path, first.model_path))
      .toBe(resident.model_path);
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

describe('automaticWriterForBuildPolicy', () => {
  const quietPolicy = {
    name: 'writer-gemma4-base-v2',
    activation: 'quiet_default',
    canonical_sha256: '2d402d213b60ba65c4d018907e9eba67ccfbc1e97081cc0505f9713ae2dd89d2'
  } as const;

  it('ignores an arbitrary loaded completion model and selects exact policy evidence', () => {
    const arbitrary = model({
      model_id: 'arbitrary',
      loaded: true,
      completion: true,
      output_tokens: true,
      policy_candidate: null
    });
    const exact = model({
      model_id: 'exact',
      loaded: true,
      completion: true,
      output_tokens: true,
      policy_verified: { profile_id: 'gemma_4_e2b_base_q8_loom_v1', rank: 0 },
      tested_profile: 'gemma_4_e2b_base_q8_loom_v1'
    });
    expect(automaticWriterForBuildPolicy([arbitrary], quietPolicy)).toBeUndefined();
    expect(automaticWriterForBuildPolicy([arbitrary, exact], quietPolicy)?.model_id).toBe('exact');
  });

  it('cannot produce an automatic writer for a none or unverified build policy', () => {
    const exact = model({
      loaded: true,
      completion: true,
      output_tokens: true,
      policy_verified: { profile_id: 'gemma_4_e2b_base_q8_loom_v1', rank: 0 }
    });
    expect(automaticWriterForBuildPolicy([exact], null)).toBeUndefined();
    expect(automaticWriterForBuildPolicy([exact], {
      name: 'none-v1',
      activation: 'project_opt_in',
      canonical_sha256: 'ce3bdf5e3dbcac6f7bcc164ec4cc5c78b4a7b5bef7c49b3cd52c61e123b75fe0'
    })).toBeUndefined();
  });
});

describe('writerProfileForBuildPolicy', () => {
  it('binds both Gemma writer policies and rejects a build without a writer', () => {
    expect(writerProfileForBuildPolicy({
      name: 'writer-gemma4-base-v2',
      activation: 'quiet_default',
      canonical_sha256: '2d402d213b60ba65c4d018907e9eba67ccfbc1e97081cc0505f9713ae2dd89d2'
    })).toBe('gemma_4_e2b_base_q8_loom_v1');
    expect(writerProfileForBuildPolicy({
      name: 'none-v1',
      activation: 'project_opt_in',
      canonical_sha256: 'ce3bdf5e3dbcac6f7bcc164ec4cc5c78b4a7b5bef7c49b3cd52c61e123b75fe0'
    })).toBeNull();
    expect(writerProfileForBuildPolicy(null)).toBeNull();
  });
});
