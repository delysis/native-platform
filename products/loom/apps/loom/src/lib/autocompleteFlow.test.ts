import { defaultMarkdownParser, defaultMarkdownSerializer } from 'prosemirror-markdown';
import { EditorState, Selection } from 'prosemirror-state';
import type { EditorView } from 'prosemirror-view';
import { describe, expect, it } from 'vitest';
import { verifiedGhostSuggestion } from './ghostSuggestion';
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
    weave_command_id: null,
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
  it('does not score or replace a verified autocomplete generation after completion', async () => {
    const repeated = await hydratedBranch(
      'run-repeated',
      'candidate-repeated',
      ` ${'rain '.repeat(32)}`
    );
    expect(verifiedGhostSuggestion(repeated.branch, repeated.body)).toEqual({
      candidateId: 'candidate-repeated',
      presentationKey: `candidate-repeated:${repeated.body.blobId}`,
      targetByte: 61,
      text: repeated.body.text
    });
  });

  it('handles Tab only for the first verified candidate and never shops for a replacement', async () => {
    const clean = await hydratedBranch(
      'run-clean',
      'candidate-clean',
      ', while somewhere beyond the rain a signal clicked from red to green.'
    );
    const available = verifiedGhostSuggestion(clean.branch, clean.body);
    if (!available) throw new Error('expected verified candidate');

    let accepted: { candidateId: string; presentationKey: string; text: string } | null = null;
    const plugin = createGhostTextPlugin({
      accept: () => false,
      insert: (candidateId, presentationKey, text, action) => {
        expect(action).toBe('inline_tab');
        accepted = { candidateId, presentationKey, text };
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
      candidateId: available.candidateId,
      presentationKey: available.presentationKey,
      surfaceKey: 'project:document:revision:visual',
      anchorByteOffset: available.targetByte,
      text: available.text,
      insertsOnAccept: true
    });

    const handled = plugin.props.handleKeyDown?.call(plugin, view, {
      key: 'Tab', keyCode: 9, isComposing: false, shiftKey: false,
      metaKey: false, ctrlKey: false, altKey: false
    } as KeyboardEvent);
    expect(handled).toBe(true);
    expect(accepted).toEqual({
      candidateId: 'candidate-clean',
      presentationKey: `candidate-clean:${clean.body.blobId}`,
      text: ','
    });
    expect(defaultMarkdownSerializer.serialize(state.doc)).toBe(`${manuscriptBefore},`);
  });
});
