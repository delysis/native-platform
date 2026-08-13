import { readFileSync } from 'node:fs';
import { defaultMarkdownParser, defaultMarkdownSerializer } from 'prosemirror-markdown';
import { EditorState, Selection } from 'prosemirror-state';
import { DecorationSet, type EditorView } from 'prosemirror-view';
import { describe, expect, it } from 'vitest';
import { verifyBranchBody } from './branchBodyProof';
import {
  createGhostTextPlugin,
  exactMarkdownByteOffsetAtSelection,
  ghostTextPluginKey,
  setGhostText,
  VISUAL_TAB_INDENT
} from './ghostText';
import {
  autocompleteDisposition,
  exactVerifiedSuggestionFamily
} from './ghostSuggestion';
import { sourceGhostKeyAction, sourceTabEdit } from './sourceGhostText';
import type { BranchCard } from './types';

interface SuggestionFixture {
  schema_version: number;
  fixture_id: string;
  source: {
    relative_path: string;
    revision_text: string;
    caret_byte: number;
  };
  candidate_family: Array<{
    text: string;
    sha256: string;
    seed: number;
  }>;
  expected: {
    primary_candidate_index: number;
    result_text: string;
    result_sha256: string;
    visible_ghost_count: number;
    hidden_candidate_count: number;
    explicit_review_candidate_count: number;
    ordinary_tab: string;
    forbidden_primary_chrome: string[];
  };
}

const fixture = JSON.parse(readFileSync(
  new URL('../../../../fixtures/w1/loom-suggestion-promotion-v1.json', import.meta.url),
  'utf8'
)) as SuggestionFixture;

async function digestText(text: string): Promise<string> {
  const bytes = new TextEncoder().encode(text);
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', bytes));
  return Array.from(digest, (byte) => byte.toString(16).padStart(2, '0')).join('');
}

function key(overrides: Partial<KeyboardEvent> = {}): KeyboardEvent {
  return {
    key: 'Tab',
    keyCode: 9,
    isComposing: false,
    shiftKey: false,
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    ...overrides
  } as KeyboardEvent;
}

describe('W1 model-free suggestion vertical', () => {
  it('shows one exact-boundary ghost and gives unmodified Tab only to that ghost', async () => {
    expect(fixture.schema_version).toBe(1);
    for (const candidate of fixture.candidate_family) {
      expect(await digestText(candidate.text)).toBe(candidate.sha256);
    }
    expect(await digestText(fixture.expected.result_text)).toBe(fixture.expected.result_sha256);

    const branches: BranchCard[] = fixture.candidate_family.map((candidate, index) => ({
      run_id: `w1-run-${index}`,
      branch_id: `w1-branch-${index}`,
      document_id: 'w1-document',
      candidate_id: `w1-candidate-${index}`,
      source_revision_id: 'w1-source-revision',
      target_start_byte: fixture.source.caret_byte,
      target_end_byte: fixture.source.caret_byte,
      text: candidate.text,
      output_blob_id: candidate.sha256,
      output_byte_len: new TextEncoder().encode(candidate.text).byteLength,
      status: 'ready',
      seed: String(candidate.seed),
      model_id: 'w1-model-free-fixture',
      selection: null,
      error: null,
      error_truncated: false,
      created_at_unix_ms: index + 1
    }));
    const bodies = await Promise.all(branches.map(async (branch) => {
      const body = await verifyBranchBody({
        run_id: branch.run_id,
        branch_id: branch.branch_id,
        document_id: branch.document_id,
        candidate_id: branch.candidate_id!,
        source_revision_id: branch.source_revision_id,
        target_start_byte: branch.target_start_byte,
        target_end_byte: branch.target_end_byte,
        seed: branch.seed!,
        model_id: branch.model_id!,
        created_at_unix_ms: branch.created_at_unix_ms,
        output_blob_id: branch.output_blob_id!,
        byte_len: branch.output_byte_len!,
        text: branch.text
      }, branch);
      expect(body).not.toBeNull();
      return body!;
    }));
    const verifiedBodyByRun = Object.fromEntries(
      branches.map((branch, index) => [branch.run_id, bodies[index]])
    );

    const disposition = autocompleteDisposition({
      active: true,
      branches,
      verifiedBodyByRun,
      dismissedCandidateIds: [],
      unpresentablePresentationKeys: [],
      targetByte: fixture.source.caret_byte
    });
    expect(disposition.kind).toBe('available');
    if (disposition.kind !== 'available') throw new Error('fixture suggestion unavailable');
    const primary = branches[fixture.expected.primary_candidate_index];
    expect(disposition.suggestion.candidateId).toBe(primary.candidate_id);
    const reviewFamily = exactVerifiedSuggestionFamily({
      active: true,
      branches,
      verifiedBodyByRun,
      targetByte: fixture.source.caret_byte
    });
    expect(reviewFamily).toHaveLength(fixture.expected.explicit_review_candidate_count);
    expect(reviewFamily.length - fixture.expected.visible_ghost_count)
      .toBe(fixture.expected.hidden_candidate_count);

    let accepted = '';
    let acceptedPresentation = '';
    let dismissed = '';
    let visible = true;
    const plugin = createGhostTextPlugin({
      accept: (candidateId, presentationKey) => {
        accepted = candidateId;
        acceptedPresentation = presentationKey;
        return true;
      },
      dismiss: (candidateId) => { dismissed = candidateId; },
      visible: (presentationKey, surfaceKey, anchorByteOffset) =>
        visible &&
        presentationKey === disposition.suggestion.presentationKey &&
        surfaceKey === 'w1-project:w1-document:w1-source-revision:visual' &&
        anchorByteOffset === fixture.source.caret_byte
    });
    const doc = defaultMarkdownParser.parse(fixture.source.revision_text);
    let state = EditorState.create({
      doc,
      selection: Selection.atEnd(doc),
      plugins: [plugin]
    });
    const view = {
      get state() { return state; },
      dispatch(transaction: Parameters<EditorView['dispatch']>[0]) {
        state = state.apply(transaction);
      }
    } as unknown as EditorView;
    const manuscriptBefore = defaultMarkdownSerializer.serialize(state.doc);
    expect(exactMarkdownByteOffsetAtSelection(state, fixture.source.revision_text))
      .toBe(fixture.source.caret_byte);

    setGhostText(view, {
      active: true,
      candidateId: disposition.suggestion.candidateId,
      presentationKey: disposition.suggestion.presentationKey,
      surfaceKey: 'w1-project:w1-document:w1-source-revision:visual',
      anchorByteOffset: disposition.suggestion.targetByte,
      text: disposition.suggestion.text
    });
    const decorations = plugin.props.decorations?.call(plugin, state);
    expect(decorations).toBeInstanceOf(DecorationSet);
    expect((decorations as DecorationSet).find()).toHaveLength(
      fixture.expected.visible_ghost_count
    );
    expect(plugin.props.handleKeyDown?.call(plugin, view, key())).toBe(true);
    expect(accepted).toBe(primary.candidate_id);
    expect(acceptedPresentation).toBe(disposition.suggestion.presentationKey);
    expect(defaultMarkdownSerializer.serialize(state.doc)).toBe(manuscriptBefore);
    expect(ghostTextPluginKey.getState(state)).toBeNull();

    state = EditorState.create({
      doc,
      selection: Selection.atEnd(doc),
      plugins: [plugin]
    });
    setGhostText(view, {
      active: true,
      candidateId: disposition.suggestion.candidateId,
      presentationKey: disposition.suggestion.presentationKey,
      surfaceKey: 'w1-project:w1-document:w1-source-revision:visual',
      anchorByteOffset: disposition.suggestion.targetByte,
      text: disposition.suggestion.text
    });
    expect(plugin.props.handleKeyDown?.call(plugin, view, key({ key: 'Escape', keyCode: 27 })))
      .toBe(true);
    expect(dismissed).toBe(primary.candidate_id);
    expect(defaultMarkdownSerializer.serialize(state.doc)).toBe(manuscriptBefore);

    state = EditorState.create({
      doc,
      selection: Selection.atEnd(doc),
      plugins: [plugin]
    });
    visible = false;
    setGhostText(view, {
      active: true,
      candidateId: disposition.suggestion.candidateId,
      presentationKey: disposition.suggestion.presentationKey,
      surfaceKey: 'w1-project:w1-document:stale-revision:visual',
      anchorByteOffset: disposition.suggestion.targetByte,
      text: disposition.suggestion.text
    });
    expect(plugin.props.handleKeyDown?.call(plugin, view, key())).toBe(true);
    expect(state.doc.textContent).toBe(`${fixture.source.revision_text}${VISUAL_TAB_INDENT}`);
    expect(VISUAL_TAB_INDENT).toBe(fixture.expected.ordinary_tab);
  });

  it('keeps source-editor Tab ordinary unless the exact rendered ghost owns it', () => {
    expect(sourceGhostKeyAction(key(), false)).toBe('insert_tab');
    expect(sourceTabEdit(fixture.source.revision_text, fixture.source.caret_byte,
      fixture.source.caret_byte)).toEqual({
      value: `${fixture.source.revision_text}${fixture.expected.ordinary_tab}`,
      caret: fixture.source.caret_byte + 1
    });
    expect(sourceGhostKeyAction(key(), true)).toBe('accept');
    expect(sourceGhostKeyAction(key({ key: 'Escape', keyCode: 27 }), true)).toBe('dismiss');
  });

  it('contains none of the rejected persistent primary chrome', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    for (const forbidden of fixture.expected.forbidden_primary_chrome) {
      expect(source).not.toContain(forbidden);
    }
    const menuStart = source.indexOf('<div class="project-menu-popover"');
    const menuEnd = source.indexOf('</details>', menuStart);
    const explicitReview = source.indexOf('<span>Review suggestions</span>');
    expect(explicitReview).toBeGreaterThan(menuStart);
    expect(explicitReview).toBeLessThan(menuEnd);
  });
});
