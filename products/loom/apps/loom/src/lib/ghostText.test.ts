import { defaultMarkdownParser, defaultMarkdownSerializer } from 'prosemirror-markdown';
import { EditorState, Selection, TextSelection } from 'prosemirror-state';
import { DecorationSet, type EditorView } from 'prosemirror-view';
import { describe, expect, it } from 'vitest';
import {
  createGhostTextPlugin,
  exactMarkdownByteOffsetAtSelection,
  ghostTextPluginKey,
  planGhostText,
  renderedGhostPresentationKey,
  setGhostText,
  VISUAL_TAB_INDENT,
  visualGhostInsertionIsVisible,
  visualGhostTextIsFaithfulAtSelection,
  visualGhostTextMayBePlainProse,
  type GhostTextPresentation
} from './ghostText';

function stateAtEnd(markdown = 'The sentence waits', withPlugin = false): EditorState {
  const initial = EditorState.create({
    doc: defaultMarkdownParser.parse(markdown),
    plugins: withPlugin ? [createGhostTextPlugin({
      accept: () => true, dismiss() {}, visible: () => true
    })] : []
  });
  return initial.apply(initial.tr.setSelection(Selection.atEnd(initial.doc)));
}

const suggestion: GhostTextPresentation = {
  active: true,
  candidateId: '01JTESTCANDIDATE00000000000',
  presentationKey: '01JTESTCANDIDATE00000000000:blob-1',
  surfaceKey: 'project:document:revision:visual',
  anchorByteOffset: 18,
  text: ' for the rain to answer.'
};

describe('planGhostText', () => {
  it('places the exact candidate bytes at an empty text selection', () => {
    const state = stateAtEnd();
    expect(planGhostText(state, suggestion)).toEqual({
      candidateId: suggestion.candidateId,
      position: state.selection.from,
      presentationKey: suggestion.presentationKey,
      surfaceKey: suggestion.surfaceKey,
      anchorByteOffset: suggestion.anchorByteOffset,
      text: suggestion.text
    });
  });

  it('refuses inactive, invisible, and ranged presentations', () => {
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
    expect(planGhostText(earlier, suggestion)?.position).toBe(earlier.selection.from);
  });
});

describe('visual ghost widget', () => {
  it('renders one zero-width widget without changing ProseMirror bytes', () => {
    const plugin = createGhostTextPlugin({
      accept: () => true, dismiss() {}, visible: () => true
    });
    const doc = defaultMarkdownParser.parse('A paragraph.');
    let state = EditorState.create({
      doc,
      selection: TextSelection.create(doc, 4),
      plugins: [plugin]
    });
    const before = defaultMarkdownSerializer.serialize(state.doc);
    state = state.apply(state.tr.setMeta(ghostTextPluginKey, {
      kind: 'set', presentation: suggestion
    }));
    const decorations = plugin.props.decorations?.call(plugin, state);
    expect(decorations).toBeInstanceOf(DecorationSet);
    const found = (decorations as DecorationSet).find();
    expect(found).toHaveLength(1);
    expect(found[0].from).toBe(state.selection.from);
    expect(found[0].to).toBe(state.selection.from);
    expect(defaultMarkdownSerializer.serialize(state.doc)).toBe(before);
  });
});

describe('visual ghost visibility authority', () => {
  const clip = { left: 20, top: 10, right: 220, bottom: 110 };

  it('requires both the caret and first ghost fragment inside the viewport', () => {
    expect(visualGhostInsertionIsVisible(
      { left: 30, top: 20, right: 30, bottom: 40 },
      { left: 30, top: 20, right: 90, bottom: 40 },
      clip,
      'ltr'
    )).toBe(true);
    expect(visualGhostInsertionIsVisible(
      { left: 30, top: -30, right: 30, bottom: -10 },
      { left: 30, top: 20, right: 90, bottom: 40 },
      clip,
      'ltr'
    )).toBe(false);
    expect(visualGhostInsertionIsVisible(
      { left: 30, top: 20, right: 30, bottom: 40 },
      { left: 30, top: -30, right: 90, bottom: -10 },
      clip,
      'ltr'
    )).toBe(false);
  });

  it('does not let a later wrapped fragment authorize an offscreen start', () => {
    expect(visualGhostInsertionIsVisible(
      { left: 30, top: -30, right: 30, bottom: -10 },
      { left: 30, top: -30, right: 200, bottom: -10 },
      clip,
      'ltr'
    )).toBe(false);
  });

  it('uses the correct insertion edge for RTL text and rejects invalid geometry', () => {
    expect(visualGhostInsertionIsVisible(
      { left: 190, top: 20, right: 190, bottom: 40 },
      { left: 100, top: 20, right: 190, bottom: 40 },
      clip,
      'rtl'
    )).toBe(true);
    expect(visualGhostInsertionIsVisible(
      { left: 230, top: 20, right: 230, bottom: 40 },
      { left: 230, top: 20, right: 260, bottom: 40 },
      clip,
      'rtl'
    )).toBe(false);
    expect(visualGhostInsertionIsVisible(
      { left: Number.NaN, top: 20, right: 30, bottom: 40 },
      { left: 30, top: 20, right: 90, bottom: 40 },
      clip,
      'ltr'
    )).toBe(false);
  });
});

describe('exact visual caret to Markdown boundary', () => {
  function boundary(markdown: string, position: number): number | null {
    const doc = defaultMarkdownParser.parse(markdown);
    const state = EditorState.create({
      doc,
      selection: TextSelection.create(doc, position)
    });
    return exactMarkdownByteOffsetAtSelection(state, markdown);
  }

  it('maps interior prose, marked text, blocks, and list prefixes exactly', () => {
    expect(boundary('hello world', 6)).toBe(5);
    expect(boundary('hello **bold world** end', 12)).toBe(13);
    expect(boundary('hello\n\nworld', 8)).toBe(7);
    expect(boundary('* one\n* two', 10)).toBe(8);
    expect(boundary('# Head\n\ntext', 7)).toBe(8);
  });

  it('returns UTF-8 bytes rather than UTF-16 code units', () => {
    expect(boundary('A 🧵 waits', 5)).toBe(new TextEncoder().encode('A 🧵').byteLength);
    expect(boundary('A 🧵 waits', 4)).toBeNull();
  });

  it('fails closed inside extended grapheme clusters', () => {
    expect(boundary('A e\u0301 waits', 4)).toBeNull();
    expect(boundary('A e\u0301 waits', 5)).toBe(new TextEncoder().encode('A e\u0301').byteLength);

    const family = 'A 👩‍👩‍👧‍👦 waits';
    const familyDoc = defaultMarkdownParser.parse(family);
    const familyText = familyDoc.firstChild?.textContent ?? '';
    const familyStart = familyText.indexOf('👩');
    const familyEnd = familyStart + '👩‍👩‍👧‍👦'.length;
    expect(boundary(family, familyStart + 2)).toBeNull();
    expect(boundary(family, familyEnd + 1)).not.toBeNull();

    const flag = 'A 🇺🇳 waits';
    expect(boundary(flag, 5)).toBeNull();
    expect(boundary(flag, 7)).not.toBeNull();
  });

  it('flattens visible text across mark boundaries before proving a grapheme edge', () => {
    const markedCombining = '**e**\u0301';
    expect(defaultMarkdownParser.parse(markedCombining).toString()).toBe(
      'doc(paragraph(strong("e"), "\u0301"))'
    );
    expect(boundary(markedCombining, 2)).toBeNull();
    expect(boundary(markedCombining, 3)).not.toBeNull();
  });

  it('fails closed around inline atoms without an exact visible-text mapping', () => {
    const hardBreak = 'a  \nb';
    expect(defaultMarkdownParser.parse(hardBreak).toString()).toBe(
      'doc(paragraph("a", hard_break, "b"))'
    );
    expect(boundary(hardBreak, 2)).toBeNull();
    expect(boundary(hardBreak, 3)).toBeNull();
  });

  it('fails closed for stale Markdown, ranges, and witness collisions', () => {
    const doc = defaultMarkdownParser.parse('hello');
    const cursor = EditorState.create({ doc, selection: TextSelection.create(doc, 3) });
    const range = EditorState.create({ doc, selection: TextSelection.create(doc, 2, 4) });
    expect(exactMarkdownByteOffsetAtSelection(cursor, 'different')).toBeNull();
    expect(exactMarkdownByteOffsetAtSelection(range, 'hello')).toBeNull();
    expect(exactMarkdownByteOffsetAtSelection(
      cursor,
      'hello\uE000LOOM_CARET_BOUNDARY_7F3A9D2C\uE001'
    )).toBeNull();
  });
});

describe('faithful visual ghost projection', () => {
  function stateAt(markdown: string, position: number): EditorState {
    const doc = defaultMarkdownParser.parse(markdown);
    return EditorState.create({ doc, selection: TextSelection.create(doc, position) });
  }

  it('admits literal inline prose only when promoted Markdown has the same document', () => {
    const markdown = 'The rain waits.';
    const state = stateAt(markdown, Selection.atEnd(defaultMarkdownParser.parse(markdown)).from);
    const anchor = new TextEncoder().encode(markdown).byteLength;
    expect(visualGhostTextMayBePlainProse(' for morning.')).toBe(true);
    expect(visualGhostTextIsFaithfulAtSelection(
      state,
      markdown,
      anchor,
      ' for morning.'
    )).toBe(true);
  });

  it('admits exact plain paragraph continuations but rejects Markdown controls and the wrong anchor', () => {
    const markdown = 'The rain waits.';
    const doc = defaultMarkdownParser.parse(markdown);
    const state = EditorState.create({ doc, selection: Selection.atEnd(doc) });
    const anchor = new TextEncoder().encode(markdown).byteLength;
    expect(visualGhostTextMayBePlainProse(' **boldly**')).toBe(false);
    expect(visualGhostTextMayBePlainProse('\n\nMorning came.\n\nThe bells answered.')).toBe(true);
    expect(visualGhostTextMayBePlainProse('\n\n# Morning came.')).toBe(false);
    expect(visualGhostTextMayBePlainProse('\n\n- Morning came.')).toBe(false);
    expect(visualGhostTextIsFaithfulAtSelection(
      state,
      markdown,
      anchor,
      '\n\nMorning came.\n\nThe bells answered.'
    )).toBe(true);
    expect(visualGhostTextIsFaithfulAtSelection(
      state,
      markdown,
      anchor,
      ' **boldly**'
    )).toBe(false);
    expect(visualGhostTextIsFaithfulAtSelection(
      state,
      markdown,
      anchor - 1,
      ' softly'
    )).toBe(false);
  });

  it('rejects candidate edges that join a human grapheme', () => {
    const cases = [
      { markdown: 'e', text: '\u0301 morning' },
      { markdown: '👩', text: '\u200d👩 together' },
      { markdown: '🇺', text: '🇳 together' }
    ];
    for (const { markdown, text } of cases) {
      const doc = defaultMarkdownParser.parse(markdown);
      const state = EditorState.create({ doc, selection: Selection.atEnd(doc) });
      const anchor = new TextEncoder().encode(markdown).byteLength;
      expect(visualGhostTextMayBePlainProse(text)).toBe(true);
      expect(visualGhostTextIsFaithfulAtSelection(
        state,
        markdown,
        anchor,
        text
      )).toBe(false);
    }
  });

  it('rejects a candidate whose trailing regional indicator joins the suffix', () => {
    const markdown = 'A 🇳 waits';
    const doc = defaultMarkdownParser.parse(markdown);
    const state = EditorState.create({ doc, selection: TextSelection.create(doc, 3) });
    const anchor = new TextEncoder().encode('A ').byteLength;
    const text = '🇺';
    expect(visualGhostTextMayBePlainProse(text)).toBe(true);
    expect(visualGhostTextIsFaithfulAtSelection(state, markdown, anchor, text)).toBe(false);
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

  it('refreshes surface and anchor identity even when candidate bytes are unchanged', () => {
    let state = stateAtEnd('The sentence waits', true);
    const view = {
      get state() { return state; },
      dispatch(transaction: Parameters<EditorView['dispatch']>[0]) {
        state = state.apply(transaction);
      }
    } as unknown as EditorView;

    setGhostText(view, suggestion);
    setGhostText(view, {
      ...suggestion,
      surfaceKey: 'project:document:new-epoch:visual',
      anchorByteOffset: suggestion.anchorByteOffset + 1
    });
    expect(ghostTextPluginKey.getState(state)).toMatchObject({
      presentationKey: suggestion.presentationKey,
      surfaceKey: 'project:document:new-epoch:visual',
      anchorByteOffset: suggestion.anchorByteOffset + 1
    });
  });

  it('accepts a rendered ghost and otherwise inserts visual indentation', () => {
    let accepted = '';
    let wasInstalledWhenClaimed = false;
    let visible = false;
    const plugin = createGhostTextPlugin({
      accept: (candidateId) => {
        accepted = candidateId;
        wasInstalledWhenClaimed = ghostTextPluginKey.getState(state) !== null;
        return true;
      },
      dismiss() {},
      visible: () => visible
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

    const tab = {
      key: 'Tab',
      keyCode: 9,
      isComposing: false,
      shiftKey: false,
      metaKey: false,
      ctrlKey: false,
      altKey: false
    } as KeyboardEvent;
    expect(plugin.props.handleKeyDown?.call(plugin, view, tab)).toBe(true);
    expect(accepted).toBe('');
    expect(state.doc.textContent).toBe(`The sentence waits${VISUAL_TAB_INDENT}`);
    expect(ghostTextPluginKey.getState(state)).toBeNull();

    const resetDoc = defaultMarkdownParser.parse('The sentence waits');
    state = EditorState.create({
      doc: resetDoc,
      selection: Selection.atEnd(resetDoc),
      plugins: [plugin]
    });
    setGhostText(view, suggestion);
    visible = true;
    const handled = plugin.props.handleKeyDown?.call(plugin, view, tab);
    expect(handled).toBe(true);
    expect(accepted).toBe(suggestion.candidateId);
    expect(wasInstalledWhenClaimed).toBe(true);
    expect(ghostTextPluginKey.getState(state)).toBeNull();
  });

  it('falls back to visual indentation when the parent rejects a stale ghost', () => {
    const plugin = createGhostTextPlugin({
      accept: () => false,
      dismiss() {},
      visible: () => true
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
    expect(state.doc.textContent).toBe(`The sentence waits${VISUAL_TAB_INDENT}`);
    expect(ghostTextPluginKey.getState(state)).toBeNull();
  });

  it('handles Tab without a ghost while modified Tab remains navigation', () => {
    const plugin = createGhostTextPlugin({
      accept: () => false,
      dismiss() {},
      visible: () => false
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
    const key = (overrides: Partial<KeyboardEvent> = {}) => ({
      key: 'Tab', keyCode: 9, isComposing: false,
      shiftKey: false, metaKey: false, ctrlKey: false, altKey: false,
      ...overrides
    } as KeyboardEvent);

    expect(plugin.props.handleKeyDown?.call(plugin, view, key({ shiftKey: true }))).toBe(false);
    expect(plugin.props.handleKeyDown?.call(plugin, view, key({ metaKey: true }))).toBe(false);
    expect(plugin.props.handleKeyDown?.call(plugin, view, key())).toBe(true);
    expect(state.doc.textContent).toBe(`The sentence waits${VISUAL_TAB_INDENT}`);
  });

  it('inserts the same literal tab byte at visual paragraph edges', () => {
    const positions = [1, Selection.atEnd(defaultMarkdownParser.parse('A paragraph.')).from];
    for (const position of positions) {
      const doc = defaultMarkdownParser.parse('A paragraph.');
      let state = EditorState.create({ doc, selection: TextSelection.create(doc, position) });
      state = state.apply(state.tr.insertText(VISUAL_TAB_INDENT));
      const markdown = defaultMarkdownSerializer.serialize(state.doc);
      expect(markdown).toContain('\t');
    }
    // Stock CommonMark loses this edge tab; Loom's guarded visual parser has
    // a separate regression in markdownSafety.test.ts.
    expect(defaultMarkdownParser.parse('A paragraph.\t').textContent).toBe('A paragraph.');
  });

  it('dismisses with Escape and ignores IME or modified acceptance keys', () => {
    let dismissed = '';
    let accepted = '';
    const plugin = createGhostTextPlugin({
      accept: (candidateId) => {
        accepted = candidateId;
        return true;
      },
      dismiss: (candidateId) => { dismissed = candidateId; },
      visible: () => true
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
