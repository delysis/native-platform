import { defaultMarkdownParser, defaultMarkdownSerializer } from 'prosemirror-markdown';
import { EditorState, Selection } from 'prosemirror-state';
import type { EditorView } from 'prosemirror-view';
import { describe, expect, it } from 'vitest';
import {
  emptyAutocompleteRetryLedger,
  planAutocompleteRetry
} from './autocompleteRetry';
import { autocompleteDisposition } from './ghostSuggestion';
import { createGhostTextPlugin, setGhostText } from './ghostText';
import { verifyBranchBody, type VerifiedBranchBody } from './branchBodyProof';
import type { BranchCard } from './types';

function readyBranch(
  runId: string,
  candidateId: string,
  text: string
): BranchCard {
  return {
    run_id: runId,
    branch_id: `branch-${runId}`,
    document_id: 'document-1',
    candidate_id: candidateId,
    source_revision_id: 'revision-1',
    target_start_byte: 61,
    target_end_byte: 61,
    text,
    output_blob_id: null,
    output_byte_len: new TextEncoder().encode(text).byteLength,
    status: 'ready',
    seed: '7',
    model_id: 'gemma-4-base',
    selection: null,
    error: null,
    error_truncated: false,
    created_at_unix_ms: 1
  };
}

async function hydratedBranch(
  runId: string,
  candidateId: string,
  text: string
): Promise<{ branch: BranchCard; body: VerifiedBranchBody }> {
  const branch = readyBranch(runId, candidateId, text);
  const bytes = new TextEncoder().encode(text);
  const owned = new Uint8Array(bytes.byteLength);
  owned.set(bytes);
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', owned.buffer));
  const blobId = Array.from(digest, (byte) => byte.toString(16).padStart(2, '0')).join('');
  branch.output_blob_id = blobId;
  const body = await verifyBranchBody({
    run_id: runId,
    branch_id: branch.branch_id,
    document_id: 'document-1',
    candidate_id: branch.candidate_id!,
    source_revision_id: branch.source_revision_id,
    target_start_byte: branch.target_start_byte,
    target_end_byte: branch.target_end_byte,
    seed: branch.seed!,
    model_id: branch.model_id!,
    created_at_unix_ms: branch.created_at_unix_ms,
    output_blob_id: blobId,
    byte_len: bytes.byteLength,
    text
  }, branch);
  if (!body) throw new Error('test branch body did not verify');
  return { branch, body };
}

describe('synthetic autocomplete disposition to acceptance flow', () => {
  it('schedules one bounded replacement when the editor rejects the final presentation', async () => {
    const candidate = await hydratedBranch(
      'run-context',
      'candidate-context',
      ' morning'
    );
    const disposition = autocompleteDisposition({
      active: true,
      branches: [candidate.branch],
      verifiedBodyByRun: { 'run-context': candidate.body },
      dismissedCandidateIds: [],
      unpresentablePresentationKeys: [`candidate-context:${candidate.body.blobId}`],
      targetByte: 61
    });
    expect(disposition).toEqual({
      kind: 'exhausted',
      candidates: [{ candidateId: 'candidate-context', reason: 'unpresentable' }]
    });
    expect(planAutocompleteRetry(emptyAutocompleteRetryLedger(), {
      disposition,
      budgetKey: 'document:revision:gemma',
      activeBranchCount: 0,
      weaveStarting: false,
      maximumRetries: 1
    }).kind).toBe('schedule');
  });

  it('retries the observed rejected family, then handles Tab only for a verified replacement', async () => {
    const upperLoop = `.\n\n${'The platform smelled of wet iron. '.repeat(18)}`.trimEnd();
    const lowerLoop = `.\n\n${'the platform smelled of wet iron.\n\n'.repeat(16)}`.trimEnd();
    const rejectedHydrated = await Promise.all([
      hydratedBranch('run-upper', 'candidate-upper', upperLoop),
      hydratedBranch('run-lower', 'candidate-lower', lowerLoop),
      hydratedBranch('run-period', 'candidate-period', '.')
    ]);
    const rejected = rejectedHydrated.map((item) => item.branch);
    const rejectedHydration = {
      'run-upper': rejectedHydrated[0].body,
      'run-lower': rejectedHydrated[1].body,
      'run-period': rejectedHydrated[2].body
    };
    const exhausted = autocompleteDisposition({
      active: true,
      branches: rejected,
      verifiedBodyByRun: rejectedHydration,
      dismissedCandidateIds: [],
      unpresentablePresentationKeys: [],
      targetByte: 61
    });
    expect(exhausted.kind).toBe('exhausted');
    expect(planAutocompleteRetry(emptyAutocompleteRetryLedger(), {
      disposition: exhausted,
      budgetKey: 'document:revision:gemma',
      activeBranchCount: 0,
      weaveStarting: false,
      maximumRetries: 1
    }).kind).toBe('schedule');

    const clean = await hydratedBranch(
      'run-clean',
      'candidate-clean',
      ', while somewhere beyond the rain a signal clicked from red to green.'
    );
    const available = autocompleteDisposition({
      active: true,
      branches: [...rejected, clean.branch],
      verifiedBodyByRun: { ...rejectedHydration, 'run-clean': clean.body },
      dismissedCandidateIds: [],
      unpresentablePresentationKeys: [],
      targetByte: 61
    });
    expect(available.kind).toBe('available');
    if (available.kind !== 'available') throw new Error('expected verified replacement');

    let accepted: { candidateId: string; presentationKey: string } | null = null;
    const plugin = createGhostTextPlugin({
      accept: (candidateId, presentationKey) => {
        accepted = { candidateId, presentationKey };
        return true;
      },
      dismiss() {},
      visible: () => true
    });
    const doc = defaultMarkdownParser.parse(
      'The last train was gone, and the platform smelled of wet iron'
    );
    let state = EditorState.create({
      doc,
      selection: Selection.atEnd(doc),
      plugins: [plugin]
    });
    const manuscriptBefore = defaultMarkdownSerializer.serialize(state.doc);
    const view = {
      get state() { return state; },
      dispatch(transaction: Parameters<EditorView['dispatch']>[0]) {
        state = state.apply(transaction);
      }
    } as unknown as EditorView;
    setGhostText(view, {
      active: true,
      candidateId: available.suggestion.candidateId,
      presentationKey: available.suggestion.presentationKey,
      surfaceKey: 'project:document:revision:visual',
      anchorByteOffset: available.suggestion.targetByte,
      text: available.suggestion.text
    });

    const handled = plugin.props.handleKeyDown?.call(plugin, view, {
      key: 'Tab',
      keyCode: 9,
      isComposing: false,
      shiftKey: false,
      metaKey: false,
      ctrlKey: false,
      altKey: false
    } as KeyboardEvent);
    expect(handled).toBe(true);
    expect(accepted).toEqual({
      candidateId: 'candidate-clean',
      presentationKey: `candidate-clean:${clean.body.blobId}`
    });
    expect(defaultMarkdownSerializer.serialize(state.doc)).toBe(manuscriptBefore);
  });
});
