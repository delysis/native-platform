import { describe, expect, it } from 'vitest';
import type {
  CuratedModelCatalogEntry,
  CuratedModelCatalogSnapshot,
  ModelCapabilitySummary
} from './types';
import {
  catalogDownloadRequest,
  legacyLocalCatalogMatch,
  validateCuratedModelCatalog
} from './modelCatalog';

const entry: CuratedModelCatalogEntry = {
  catalog_id: 'google.gemma-4-12b-it-qat-q4_0',
  display_name: 'Gemma 4 12B QAT Q4_0',
  publisher: 'Google',
  repository: 'google/gemma-4-12B-it-qat-q4_0-gguf',
  revision: '29d097773436b69ff9feafd636ab4cf873786537',
  artifact_name: 'gemma-4-12b-it-qat-q4_0.gguf',
  download_url: 'https://huggingface.co/google/gemma-4-12B-it-qat-q4_0-gguf/resolve/29d097773436b69ff9feafd636ab4cf873786537/gemma-4-12b-it-qat-q4_0.gguf?download=true',
  expected_sha256: '93567e57a8fe10b23569b9d9ec38cd005deedf71e29477c421a4b83f418a538b',
  expected_bytes: 6_975_879_296,
  max_bytes: 6_975_879_296,
  context_tokens: 262_144,
  license: {
    spdx_id: 'Apache-2.0',
    name: 'Apache License 2.0',
    url: 'https://ai.google.dev/gemma/docs/gemma_4_license'
  },
  memory_fit: {
    weight_bytes: 6_975_879_296,
    recommended_system_memory_bytes: 17_179_869_184,
    description: '16 GiB or more system memory recommended.'
  },
  compatibility: {
    local_only: true,
    hosted_fallback: false,
    prompt_mode: 'raw_completion',
    native_inspection_required: true,
    legacy_local_file_name: 'gemma-4-12b-it-qat-q4_0.gguf',
    legacy_local_file_bytes: 6_975_879_296
  }
};

function catalog(overrides: Partial<CuratedModelCatalogSnapshot> = {}): CuratedModelCatalogSnapshot {
  return { schema_version: 1, entries: [entry], ...overrides };
}

function localModel(overrides: Partial<ModelCapabilitySummary> = {}): ModelCapabilitySummary {
  return {
    model_id: 'discovered:gemma',
    display_name: entry.artifact_name,
    local: true,
    loaded: false,
    chat: false,
    completion: false,
    fill_in_middle: false,
    output_tokens: false,
    logprobs: false,
    model_path: `/models/${entry.artifact_name}`,
    file_bytes: entry.expected_bytes,
    header_verified: true,
    architecture: null,
    context_tokens: null,
    model_sha256: null,
    projector_present: null,
    media_kinds: [],
    policy_candidate: null,
    policy_verified: null,
    tested_profile: null,
    ...overrides
  };
}

describe('curated model catalog', () => {
  it('accepts the one immutable local-only artifact and builds its exact verified request', () => {
    expect(validateCuratedModelCatalog(catalog())).toEqual([entry]);
    expect(catalogDownloadRequest(entry)).toEqual({
      url: entry.download_url,
      fileName: entry.artifact_name,
      sha256: entry.expected_sha256,
      expectedBytes: entry.expected_bytes,
      maxBytes: entry.expected_bytes
    });
  });

  it.each([
    ['moving revision', { revision: 'main' }],
    ['mutable URL', { download_url: entry.download_url.replace(entry.revision, 'main') }],
    ['malformed checksum', { expected_sha256: 'z'.repeat(64) }],
    ['loose byte ceiling', { max_bytes: entry.expected_bytes + 1 }],
    ['hosted fallback', {
      compatibility: { ...entry.compatibility, hosted_fallback: true }
    }]
  ])('fails closed for %s', (_case, patch) => {
    expect(() => validateCuratedModelCatalog(catalog({
      entries: [{ ...entry, ...patch } as CuratedModelCatalogEntry]
    }))).toThrow('invalid curated model catalog entry');
  });

  it('does not let legacy local compatibility stand in for catalog integrity', () => {
    expect(legacyLocalCatalogMatch(entry, localModel())).toBe(true);
    expect(legacyLocalCatalogMatch(entry, localModel({
      file_bytes: entry.expected_bytes - 1
    }))).toBe(false);
    expect(legacyLocalCatalogMatch(entry, localModel({
      header_verified: false
    }))).toBe(false);
    expect(legacyLocalCatalogMatch(entry, localModel({
      display_name: `mmproj-${entry.artifact_name}`,
      model_path: `/models/mmproj-${entry.artifact_name}`
    }))).toBe(false);
    expect(localModel().model_sha256).toBeNull();
  });

  it('rejects any second curated identity in schema version one', () => {
    expect(() => validateCuratedModelCatalog(catalog({ entries: [entry, entry] })))
      .toThrow('unsupported curated model catalog');
  });
});
