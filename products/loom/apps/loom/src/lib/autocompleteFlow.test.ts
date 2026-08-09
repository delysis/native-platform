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
import type { BranchCard } from './types';

function readyBranch(
  runId: string,
  candidateId: string,
  blobId: string,
  text: string
): BranchCard {
  return {
    run_id: runId,
    branch_id: `branch-${runId}`,
    candidate_id: candidateId,
    source_revision_id: 'revision-1',
    target_start_byte: 61,
    target_end_byte: 61,
    text,
    output_blob_id: blobId,
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

describe('synthetic autocomplete disposition to acceptance flow', () => {
  it('retries the observed rejected family, then handles Tab only for a verified replacement', () => {
    const upperLoop = `.\n\n${'The platform smelled of wet iron. '.repeat(18)}`.trimEnd();
    const lowerLoop = `.\n\n${'the platform smelled of wet iron.\n\n'.repeat(16)}`.trimEnd();
    const rejected = [
      readyBranch('run-upper', 'candidate-upper', 'blob-upper', upperLoop),
      readyBranch('run-lower', 'candidate-lower', 'blob-lower', lowerLoop),
      readyBranch('run-period', 'candidate-period', 'blob-period', '.')
    ];
    const rejectedHydration = {
      'run-upper': 'blob-upper',
      'run-lower': 'blob-lower',
      'run-period': 'blob-period'
    };
    const exhausted = autocompleteDisposition({
      active: true,
      branches: rejected,
      hydratedBlobByRun: rejectedHydration,
      dismissedCandidateIds: [],
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

    const clean = readyBranch(
      'run-clean',
      'candidate-clean',
      'blob-clean',
      ', while somewhere beyond the rain a signal clicked from red to green.'
    );
    const available = autocompleteDisposition({
      active: true,
      branches: [...rejected, clean],
      hydratedBlobByRun: { ...rejectedHydration, 'run-clean': 'blob-clean' },
      dismissedCandidateIds: [],
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
      presentationKey: 'candidate-clean:blob-clean'
    });
    expect(defaultMarkdownSerializer.serialize(state.doc)).toBe(manuscriptBefore);
  });
});
