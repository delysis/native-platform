import { describe, expect, it } from 'vitest';
import {
  inlineSuggestionFamily,
  projectInlineCandidateText,
  type InlineSuggestionState
} from './inlineSuggestionFamily';
import {
  completionPresentation,
  consumeCompletionText,
  startCompletionSession,
  updateCompletionCandidate
} from './completionSession';
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

  it('preserves the same editor-owned separator through stream refresh after one word', () => {
    const family = inlineSuggestionFamily(5, 'visual', state('hello'));
    const session = startCompletionSession('context', family, 'run-1');
    expect(session).not.toBeNull();
    const consumed = consumeCompletionText(session!, ' world');
    expect(consumed).not.toBeNull();

    const projected = projectInlineCandidateText(
      5,
      'visual',
      'hello world',
      'world continues farther',
      null
    );
    expect(projected).toBe(' world continues farther');
    const refreshed = updateCompletionCandidate(
      consumed!.session,
      'run-1',
      projected!,
      'stream:run-1:23'
    );
    expect(refreshed).not.toBeNull();
    expect(completionPresentation(refreshed!)).toMatchObject({
      text: ' continues farther',
      targetByte: 11
    });
    expect(projectInlineCandidateText(
      5,
      'source',
      'hello world',
      'world continues farther',
      null
    )).toBe(' world continues farther');
  });
});
