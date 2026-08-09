import { defaultMarkdownParser, defaultMarkdownSerializer } from 'prosemirror-markdown';
import { EditorState, Selection, TextSelection } from 'prosemirror-state';
import type { EditorView } from 'prosemirror-view';
import { describe, expect, it } from 'vitest';
import {
  createGhostTextPlugin,
  ghostTextPluginKey,
  planGhostText,
  planGhostOverlayGeometry,
  renderedGhostPresentationKey,
  setGhostText,
  type GhostTextPresentation
} from './ghostText';

function stateAtEnd(markdown = 'The sentence waits', withPlugin = false): EditorState {
  const initial = EditorState.create({
    doc: defaultMarkdownParser.parse(markdown),
    plugins: withPlugin ? [createGhostTextPlugin({ accept() {}, dismiss() {} })] : []
  });
  return initial.apply(initial.tr.setSelection(Selection.atEnd(initial.doc)));
}

const suggestion: GhostTextPresentation = {
  active: true,
  candidateId: '01JTESTCANDIDATE00000000000',
  presentationKey: '01JTESTCANDIDATE00000000000:blob-1',
  text: ' for the rain to answer.\n\nThen it does.'
};

describe('planGhostText', () => {
  it('places the exact candidate bytes at an empty document-end selection', () => {
    const state = stateAtEnd();
    expect(planGhostText(state, suggestion)).toEqual({
      candidateId: suggestion.candidateId,
      position: state.selection.from,
      presentationKey: suggestion.presentationKey,
      text: suggestion.text
    });
  });

  it('refuses inactive, invisible, ranged, and non-end presentations', () => {
    const end = stateAtEnd();
    expect(planGhostText(end, { ...suggestion, active: false })).toBeNull();
    expect(planGhostText(end, { ...suggestion, text: ' \n\t' })).toBeNull();

    const ranged = end.apply(end.tr.setSelection(
      TextSelection.create(end.doc, end.selection.from - 2, end.selection.from)
    ));
    expect(planGhostText(ranged, suggestion)).toBeNull();

    const earlier = end.apply(end.tr.setSelection(
      TextSelection.create(end.doc, Math.max(1, end.selection.from - 2))
    ));
    expect(planGhostText(earlier, suggestion)).toBeNull();
  });
});

describe('visual ghost overlay', () => {
  it('keeps generated text out of ProseMirror DOM decorations', () => {
    const plugin = createGhostTextPlugin({ accept() {}, dismiss() {} });
    expect(plugin.props.decorations).toBeUndefined();
  });

  it('continues on the caret line when space remains', () => {
    expect(planGhostOverlayGeometry({
      caret: { left: 220, top: 80, bottom: 110 },
      shell: { left: 100, top: 20 },
      text: { left: 150, right: 600 }
    })).toEqual({ left: 120, top: 60, maxWidth: 380 });
  });

  it('starts a new visual line near the writing measure edge', () => {
    expect(planGhostOverlayGeometry({
      caret: { left: 570, top: 80, bottom: 110 },
      shell: { left: 100, top: 20 },
      text: { left: 150, right: 600 }
    })).toEqual({ left: 50, top: 90, maxWidth: 450 });
  });

  it('refuses non-finite or inverted layout evidence', () => {
    expect(planGhostOverlayGeometry({
      caret: { left: Number.NaN, top: 0, bottom: 1 },
      shell: { left: 0, top: 0 },
      text: { left: 0, right: 1 }
    })).toBeNull();
    expect(planGhostOverlayGeometry({
      caret: { left: 1, top: 0, bottom: 1 },
      shell: { left: 0, top: 0 },
      text: { left: 2, right: 1 }
    })).toBeNull();
  });
});

describe('ghost-text plugin state', () => {
  it('clears synchronously on the first document-changing transaction', () => {
    let state = stateAtEnd('The sentence waits', true);
    const view = {
      get state() { return state; },
      dispatch(transaction: Parameters<EditorView['dispatch']>[0]) {
        state = state.apply(transaction);
      }
    } as unknown as EditorView;

    setGhostText(view, suggestion);
    expect(ghostTextPluginKey.getState(state)?.presentationKey).toBe(suggestion.presentationKey);
    expect(renderedGhostPresentationKey(state)).toBe(suggestion.presentationKey);
    state = state.apply(state.tr.insertText('!'));
    expect(ghostTextPluginKey.getState(state)).toBeNull();
    expect(renderedGhostPresentationKey(state)).toBe('');
  });

  it('sets and clears presentation state without changing manuscript bytes', () => {
    let state = stateAtEnd('The sentence waits', true);
    const before = defaultMarkdownSerializer.serialize(state.doc);
    const view = {
      get state() { return state; },
      dispatch(transaction: Parameters<EditorView['dispatch']>[0]) {
        state = state.apply(transaction);
      }
    } as unknown as EditorView;

    setGhostText(view, suggestion);
    setGhostText(view, null);
    expect(defaultMarkdownSerializer.serialize(state.doc)).toBe(before);
    expect(ghostTextPluginKey.getState(state)).toBeNull();
  });

  it('accepts with unmodified Tab only while the exact ghost is rendered', () => {
    let accepted = '';
    const plugin = createGhostTextPlugin({
      accept: (candidateId) => { accepted = candidateId; },
      dismiss() {}
    });
    const doc = defaultMarkdownParser.parse('The sentence waits');
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
    setGhostText(view, suggestion);

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
    expect(accepted).toBe(suggestion.candidateId);
    expect(ghostTextPluginKey.getState(state)).toBeNull();
  });

  it('dismisses with Escape and ignores IME or modified acceptance keys', () => {
    let dismissed = '';
    let accepted = '';
    const plugin = createGhostTextPlugin({
      accept: (candidateId) => { accepted = candidateId; },
      dismiss: (candidateId) => { dismissed = candidateId; }
    });
    const doc = defaultMarkdownParser.parse('The sentence waits');
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
    setGhostText(view, suggestion);
    const key = (overrides: Partial<KeyboardEvent>) => ({
      key: 'Tab', keyCode: 9, isComposing: false,
      shiftKey: false, metaKey: false, ctrlKey: false, altKey: false,
      ...overrides
    } as KeyboardEvent);

    expect(plugin.props.handleKeyDown?.call(plugin, view, key({ isComposing: true }))).toBe(false);
    expect(plugin.props.handleKeyDown?.call(plugin, view, key({ metaKey: true }))).toBe(false);
    expect(accepted).toBe('');
    expect(ghostTextPluginKey.getState(state)).not.toBeNull();
    expect(plugin.props.handleKeyDown?.call(plugin, view, key({ key: 'Escape', keyCode: 27 })))
      .toBe(true);
    expect(dismissed).toBe(suggestion.candidateId);
    expect(ghostTextPluginKey.getState(state)).toBeNull();
  });
});
