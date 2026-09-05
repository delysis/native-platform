import { describe, expect, it } from 'vitest';
import {
  inlineSuggestionFamily,
  authoritativeInlineFamilyId,
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

const FAMILY_ONE = '01K00000000000000000000001';
const FAMILY_TWO = '01K00000000000000000000002';

function state(manuscriptText: string): InlineSuggestionState {
  const branch: BranchCard = {
    run_id: 'run-1',
    branch_id: 'branch-1',
    weave_command_id: FAMILY_ONE,
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
    projector_sha256: null,
    media_kinds: [],
    policy_candidate: null,
    policy_verified: null,
    tested_profile: null
  };
  return {
    branches: Array.from({ length: 4 }, (_, index) => ({
      ...branch,
      run_id: `run-${index + 1}`,
      branch_id: `branch-${index + 1}`,
      seed: String(index + 1)
    })),
    verifiedBodyByRun: {},
    liveTextByRun: { 'run-1': 'world continues' },
    liveTextSequenceByRun: { 'run-1': '7' },
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
      presentationKey: 'stream:run-1:7:prose-prefix:16',
      text: ' world continues',
      insertsOnAccept: true
    }]);
    expect(inlineSuggestionFamily(5, 'visual', state(''))).toEqual([]);
  });

  it('keys equal-byte-length live replacements by text-delta sequence', () => {
    const selection = state('hello');
    selection.liveTextByRun['run-1'] = 'cats arrive';
    selection.liveTextSequenceByRun!['run-1'] = '41';
    const first = inlineSuggestionFamily(5, 'visual', selection)[0];
    selection.liveTextByRun['run-1'] = 'dogs arrive';
    selection.liveTextSequenceByRun!['run-1'] = '42';
    const second = inlineSuggestionFamily(5, 'visual', selection)[0];

    expect(first).toMatchObject({
      text: ' cats arrive',
      presentationKey: 'stream:run-1:41:prose-prefix:12'
    });
    expect(second).toMatchObject({
      text: ' dogs arrive',
      presentationKey: 'stream:run-1:42:prose-prefix:12'
    });
  });

  it('fails closed when live text has no validated sequence partner', () => {
    const selection = state('hello');
    selection.liveTextSequenceByRun = {};
    expect(inlineSuggestionFamily(5, 'visual', selection)).toEqual([]);
  });

  it('projects multiline model output to one structurally faithful visual text block', () => {
    expect(projectInlineCandidateText(
      5,
      'visual',
      'hello',
      ' world.\n\nA new paragraph.',
      null
    )).toBe(' world.');
    expect(projectInlineCandidateText(
      5,
      'source',
      'hello',
      ' world.\n\nA new paragraph.',
      null
    )).toBe(' world.\n\nA new paragraph.');
    expect(projectInlineCandidateText(
      5,
      'visual',
      'hello',
      '\n\nA new paragraph.',
      null
    )).toBeNull();
  });

  it('surfaces only the exact live authoritative four-run family', () => {
    const selection = state('hello');
    const branches = Array.from({ length: 8 }, (_, index): BranchCard => ({
      ...selection.branches[0],
      run_id: `run-${index + 1}`,
      branch_id: `branch-${index + 1}`,
      weave_command_id: index < 4 ? FAMILY_ONE : FAMILY_TWO,
      created_at_unix_ms: index + 1
    }));
    selection.branches = [
      branches[0],
      branches[4],
      branches[1],
      branches[5],
      branches[2],
      branches[6],
      branches[3],
      branches[7]
    ];
    selection.liveTextByRun = Object.fromEntries(
      branches.map((branch) => [branch.run_id, `choice ${branch.run_id}`])
    );
    selection.liveTextSequenceByRun = Object.fromEntries(
      branches.map((branch, index) => [branch.run_id, String(index + 1)])
    );
    selection.authoritativeFamilyId = FAMILY_TWO;

    expect(inlineSuggestionFamily(5, 'visual', selection).map(({ runId }) => runId))
      .toEqual(['run-5', 'run-6', 'run-7', 'run-8']);
  });

  it('requires a fresh explicit family after completion context changes', () => {
    const selection = state('hello');
    selection.requireExplicitFamily = true;
    expect(inlineSuggestionFamily(5, 'visual', selection)).toEqual([]);
    selection.authoritativeFamilyId = FAMILY_ONE;
    expect(authoritativeInlineFamilyId(5, selection)).toBe(FAMILY_ONE);
  });

  it('fails closed on an incomplete or repeated live-family identity', () => {
    const selection = state('hello');
    selection.authoritativeFamilyId = FAMILY_ONE;
    selection.branches[3] = { ...selection.branches[3], run_id: 'run-1' };
    expect(inlineSuggestionFamily(5, 'visual', selection)).toEqual([]);
    selection.branches = selection.branches.slice(0, 3);
    expect(inlineSuggestionFamily(5, 'source', selection)).toEqual([]);
  });

  it('recovers the newest complete durable family by weave identity, not time or adjacency', () => {
    const selection = state('hello');
    const branches = Array.from({ length: 9 }, (_, index): BranchCard => ({
      ...selection.branches[0],
      run_id: `run-${index + 1}`,
      branch_id: `branch-${index + 1}`,
      weave_command_id: index < 4 ? FAMILY_ONE : FAMILY_TWO,
      created_at_unix_ms: index < 4 ? 9_000 + index : index
    }));
    selection.branches = [
      branches[4],
      branches[0],
      branches[5],
      branches[1],
      branches[8],
      branches[2],
      branches[6],
      branches[3],
      branches[7]
    ];
    selection.liveTextByRun = Object.fromEntries(
      branches.map((branch) => [branch.run_id, `choice ${branch.run_id}`])
    );
    selection.liveTextSequenceByRun = Object.fromEntries(
      branches.map((branch, index) => [branch.run_id, String(index + 1)])
    );

    expect(inlineSuggestionFamily(5, 'source', selection).map(({ runId }) => runId))
      .toEqual(['run-1', 'run-2', 'run-3', 'run-4']);

    selection.branches = selection.branches.filter((branch) => branch.run_id !== 'run-9');
    expect(inlineSuggestionFamily(5, 'source', selection).map(({ runId }) => runId))
      .toEqual(['run-5', 'run-6', 'run-7', 'run-8']);
  });

  it('preserves the frozen continuation and editor-owned separator after one word', () => {
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
      text: ' continues',
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
