import { expect, it } from 'vitest';
import type { ModelCapabilitySummary } from './types';
import { workspaceWriterCandidates, workspaceWriterModel, type WorkspaceTemplateSnapshot } from './workspaceTemplate';

const model: ModelCapabilitySummary = {
  model_id: 'writer', display_name: 'writer.gguf', local: true, loaded: true,
  chat: false, completion: true, fill_in_middle: false, output_tokens: true,
  logprobs: false, model_path: '/models/writer.gguf', file_bytes: 42,
  header_verified: true, architecture: null, context_tokens: null,
  model_sha256: 'exact-native-digest', projector_present: false,
  projector_sha256: null, media_kinds: [],
  policy_candidate: { profile_id: 'writer-v1', rank: 0 },
  policy_verified: { profile_id: 'writer-v1', rank: 0 }, tested_profile: null
};
const template: WorkspaceTemplateSnapshot = {
  enabled: true, document_id: 'settings', revision_id: 'one',
  source_sha256: 'settings-source', suggestions: null, model_path: null, downloads: {}, google_client_configured: false,
  config: { model: { profile: 'writer-v1' }, panes: {} }, error: null
};

it('requires current workspace settings and exact verified identity before exposing a resident model', () => {
  expect(workspaceWriterModel([model], null, [], template, true)).toBe(model);
  expect(workspaceWriterModel([model], null, [], template, false)).toBeUndefined();
  expect(workspaceWriterModel([model], null, [], null, true)).toBeUndefined();
  expect(workspaceWriterModel([{ ...model, policy_verified: null }], null, [], template, true)).toBeUndefined();
  expect(workspaceWriterModel([model], null, [], { ...template, error: 'invalid' }, true)).toBeUndefined();
  expect(workspaceWriterModel([model], null, [], { ...template, config: { model: { profile: 'writer-v2' }, panes: {} } }, true)).toBeUndefined();
  expect(workspaceWriterModel([model], null, [], { ...template, config: { panes: {} } }, true)).toBe(model);
});

it('treats configured identity as mandatory and never falls back to a remembered arbitrary model', () => {
  const discovered = { ...model, loaded: false, policy_verified: null };
  expect(workspaceWriterCandidates({ profile: 'writer-v1' }, [discovered], [], '/models/other.gguf'))
    .toEqual([{ modelPath: model.model_path, profileId: 'writer-v1', policyRank: 0, remembered: false }]);
  expect(workspaceWriterCandidates({ profile: 'missing' }, [discovered], [], model.model_path)).toEqual([]);
  expect(workspaceWriterCandidates({ catalog: 'missing' }, [discovered], [], model.model_path)).toEqual([]);
  expect(workspaceWriterCandidates({ profile: 'writer-v1' }, [{ ...discovered, local: false }], [], model.model_path)).toEqual([]);
});

it('requires both an explicit model path and named identity when both are configured', () => {
  expect(workspaceWriterModel([model], null, [], { ...template, model_path: model.model_path }, true)).toBe(model);
  expect(workspaceWriterModel([model], null, [], { ...template, model_path: '/models/other.gguf' }, true)).toBeUndefined();
  expect(workspaceWriterModel([model], null, [], { ...template, model_path: '/models/other.gguf', config: { panes: {} } }, true)).toBeUndefined();
});
