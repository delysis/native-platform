import { describe, expect, it } from 'vitest';
import { inlineSuggestionFamily, type InlineSuggestionState } from './inlineSuggestionFamily';
import type { BranchCard, ModelCapabilitySummary, OpenDocument } from './types';

function state(manuscriptText: string): InlineSuggestionState {
  const branch: BranchCard = {
    run_id: 'run-1',
    branch_id: 'branch-1',
    document_id: 'document-1',
    candidate_id: null,
    source_revision_id: 'revision-1',
    target_start_byte: 5,
    target_end_byte: 5,
    text: '',
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
  const document: OpenDocument = {
    summary: {
      document_id: 'document-1',
      relative_path: 'hello.md',
      title: 'Hello',
      kind: 'prose',
      revision_id: 'revision-1',
      active_blob_id: 'blob-1',
      word_count: 1,
      externally_modified: false
    },
    visible_blob_id: 'blob-1',
    text: manuscriptText,
    transient_draft: null
  };
  const currentModel: ModelCapabilitySummary = {
    model_id: 'model-1',
    display_name: 'Gemma 4 12B QAT',
    local: true,
    loaded: true,
    chat: true,
    completion: true,
    fill_in_middle: false,
    output_tokens: true,
    logprobs: false,
    model_path: '/models/gemma.gguf',
    file_bytes: 1,
    header_verified: true,
    architecture: 'gemma4',
    context_tokens: 4096,
    model_sha256: null,
    projector_present: false,
    media_kinds: [],
    policy_candidate: null,
    policy_verified: null,
    tested_profile: null
  };
  return {
    branches: [branch],
    verifiedBodyByRun: {},
    liveTextByRun: { 'run-1': 'world continues' },
    currentModel,
    document,
    suggestionsEnabled: true,
    promotionReady: true,
    dismissedCandidateIds: [],
    unpresentableVisualKeys: [],
    manuscriptText,
    sourceNewline: null
  };
}

describe('inline suggestion family', () => {
  it('projects a streamed visual candidate against the current canonical manuscript', () => {
    expect(inlineSuggestionFamily(5, 'visual', state('hello'))).toMatchObject([{
      runId: 'run-1',
      targetByte: 5,
      text: ' world continues',
      insertsOnAccept: true
    }]);
    expect(inlineSuggestionFamily(5, 'visual', state(''))).toEqual([]);
  });
});
