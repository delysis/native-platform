import { defaultMarkdownParser, defaultMarkdownSerializer } from 'prosemirror-markdown';
import { EditorState, Selection, TextSelection } from 'prosemirror-state';
import { DecorationSet, type EditorView } from 'prosemirror-view';
import { describe, expect, it } from 'vitest';
import { parseVisualMarkdown } from './markdownSafety';
import {
  completionOptionAccessibleLabel,
  createGhostTextPlugin,
  exactMarkdownByteOffsetAtSelection,
  visualCaretBoundaryProof,
  ghostTextPluginKey,
  planGhostText,
  renderedGhostPresentationKey,
  setCompletionOptionAccessibility,
  setGhostFanVisible,
  setGhostText,
  VISUAL_TAB_INDENT,
  visualGhostInsertionIsVisible,
  visualGhostTextIsFaithfulAtSelection,
  visualGhostTextMayBePlainProse,
  visualGhostTextSafePrefix,
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
      text: suggestion.text,
      insertsOnAccept: false,
      alternatives: [],
      hidden: false,
      unconsumeText: '',
      fanVisible: false,
      renderEpoch: 0
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
  it('gives each fan option a human-readable accessible identity', () => {
    const attributes = new Map<string, string>();
    setCompletionOptionAccessibility(
      { setAttribute: (name, value) => attributes.set(name, value) },
      2,
      4,
      ' there,\n beyond the rain.',
      true
    );

    expect(completionOptionAccessibleLabel(2, 4, ' there,\n beyond the rain.'))
      .toBe('Suggestion 2 of 4: there, beyond the rain.');
    expect(Object.fromEntries(attributes)).toEqual({
      role: 'option',
      'aria-selected': 'true',
      'aria-label': 'Suggestion 2 of 4: there, beyond the rain.'
    });
    expect(attributes.has('aria-description')).toBe(false);
  });

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

  it('reconciles the alternatives palette to modifier state across streamed updates', () => {
    const plugin = createGhostTextPlugin({
      accept: () => true, dismiss() {}, visible: () => true
    });
    let state = EditorState.create({
      doc: defaultMarkdownParser.parse('A paragraph.'),
      selection: TextSelection.create(defaultMarkdownParser.parse('A paragraph.'), 4),
      plugins: [plugin]
    });
    const view = {
      get state() { return state; },
      dispatch(transaction: Parameters<EditorState['apply']>[0]) { state = state.apply(transaction); }
    } as unknown as EditorView;
    const withAlternatives = {
      ...suggestion,
      alternatives: [
        { candidateId: 'a', presentationKey: 'a:1', text: ' first' },
        { candidateId: 'b', presentationKey: 'b:1', text: ' second' }
      ]
    };
    setGhostText(view, withAlternatives);
    view.dispatch(state.tr.setMeta(ghostTextPluginKey, { kind: 'fan', visible: true }));
    setGhostText(view, {
      ...withAlternatives,
      presentationKey: 'a:2',
      text: ' first grows',
      fanVisible: true
    });
    expect(ghostTextPluginKey.getState(state)?.fanVisible).toBe(true);
    setGhostFanVisible(view, false);
    setGhostText(view, {
      ...withAlternatives,
      presentationKey: 'a:3',
      text: ' first grows again',
      fanVisible: true
    });
    // A real Option-down state arriving with a newly grown family must open
    // the fan even when the earlier one-candidate plan latched it closed.
    expect(ghostTextPluginKey.getState(state)?.fanVisible).toBe(true);
    setGhostFanVisible(view, true);
    setGhostText(view, {
      ...withAlternatives,
      presentationKey: 'a:4',
      text: ' first grows once more',
      fanVisible: false
    });
    expect(ghostTextPluginKey.getState(state)?.fanVisible).toBe(false);
  });

  it('dispatches only real fan transitions against a live editor view', () => {
    const plugin = createGhostTextPlugin({
      accept: () => true, dismiss() {}, visible: () => true
    });
    let state = EditorState.create({
      doc: defaultMarkdownParser.parse('A waits'),
      plugins: [plugin]
    });
    let destroyed = false;
    let dispatches = 0;
    const view = {
      get state() { return state; },
      get isDestroyed() { return destroyed; },
      dispatch(transaction: Parameters<EditorState['apply']>[0]) {
        dispatches += 1;
        state = state.apply(transaction);
      }
    } as unknown as EditorView;
    setGhostText(view, {
      ...suggestion,
      alternatives: [
        { candidateId: 'a', presentationKey: suggestion.presentationKey, text: ' first' },
        { candidateId: 'b', presentationKey: 'b:1', text: ' second' }
      ],
      fanVisible: false
    });
    dispatches = 0;

    setGhostFanVisible(view, false);
    setGhostFanVisible(view, true);
    setGhostFanVisible(view, true);
    destroyed = true;
    setGhostFanVisible(view, false);

    expect(dispatches).toBe(1);
    expect(ghostTextPluginKey.getState(state)?.fanVisible).toBe(true);
  });

  it('does not treat Command-Option or Control-Option as the fan modifier', () => {
    const modifierStates: boolean[] = [];
    const plugin = createGhostTextPlugin({
      accept: () => true,
      dismiss() {},
      visible: () => true,
      modifier: (held) => modifierStates.push(held)
    });
    const state = EditorState.create({
      doc: defaultMarkdownParser.parse('A waits'),
      plugins: [plugin]
    });
    const view = { state } as unknown as EditorView;
    const event = (metaKey: boolean, ctrlKey: boolean) => ({
      key: 'Alt',
      altKey: true,
      metaKey,
      ctrlKey,
      isComposing: false,
      keyCode: 18
    }) as KeyboardEvent;

    plugin.props.handleKeyDown?.call(plugin, view, event(true, false));
    plugin.props.handleKeyDown?.call(plugin, view, event(false, true));
    plugin.props.handleKeyDown?.call(plugin, view, event(false, false));

    expect(modifierStates).toEqual([false, false, true]);
  });

  it('reverses the exact last accepted word before falling back to macOS navigation', () => {
    const unconsumed: string[] = [];
    const modifierStates: boolean[] = [];
    const plugin = createGhostTextPlugin({
      accept: () => true,
      dismiss() {},
      visible: () => true,
      unconsume: (_candidateId, _presentationKey, text) => {
        unconsumed.push(text);
        return true;
      },
      modifier: (held) => modifierStates.push(held)
    });
    const doc = defaultMarkdownParser.parse('A one waits');
    let state = EditorState.create({ doc, selection: TextSelection.create(doc, 6), plugins: [plugin] });
    state = state.apply(state.tr.setMeta(ghostTextPluginKey, {
      kind: 'set',
      presentation: { ...suggestion, anchorByteOffset: 5, text: ' two', unconsumeText: ' one' }
    }));
    const view = {
      get state() { return state; },
      dispatch(transaction: Parameters<EditorState['apply']>[0]) { state = state.apply(transaction); }
    } as unknown as EditorView;
    const handled = plugin.props.handleKeyDown?.call(plugin, view, {
      key: 'ArrowLeft', altKey: true, metaKey: false, ctrlKey: false,
      isComposing: false, keyCode: 37, preventDefault() {}
    } as unknown as KeyboardEvent);
    expect(handled).toBe(true);
    expect(modifierStates).toEqual([true]);
    expect(unconsumed).toEqual([' one']);
    expect(state.doc.textContent).toBe('A waits');
    expect(ghostTextPluginKey.getState(state)).toMatchObject({
      anchorByteOffset: 1,
      text: ' one two'
    });
    const decorations = plugin.props.decorations?.call(plugin, state) as DecorationSet;
    expect(decorations.find()).toHaveLength(1);
    plugin.props.handleDOMEvents?.keyup?.call(plugin, view, {
      key: 'Alt', altKey: false
    } as KeyboardEvent);
    expect(modifierStates).toEqual([true, false]);
  });

  it('keeps Option held while an exact hidden rollback restores the fan family', () => {
    const modifierStates: boolean[] = [];
    const unconsumed: string[] = [];
    const plugin = createGhostTextPlugin({
      accept: () => true,
      dismiss() {},
      // An exhausted completion has no visible inline text. Its exact bytes
      // immediately before the caret, not a widget visibility witness, own
      // the one authorized rollback.
      visible: () => false,
      unconsume: (_candidateId, _presentationKey, text) => {
        unconsumed.push(text);
        return true;
      },
      modifier: (held) => modifierStates.push(held)
    });
    const doc = defaultMarkdownParser.parse('A waits for rain.');
    let state = EditorState.create({
      doc,
      selection: Selection.atEnd(doc),
      plugins: [plugin]
    });
    state = state.apply(state.tr.setMeta(ghostTextPluginKey, {
      kind: 'set',
      presentation: {
        ...suggestion,
        anchorByteOffset: 17,
        text: '',
        hidden: true,
        unconsumeText: ' for rain.',
        fanVisible: false,
        alternatives: [
          { candidateId: suggestion.candidateId, presentationKey: suggestion.presentationKey, text: ' for rain.' },
          { candidateId: 'b', presentationKey: 'b:1', text: ' until dawn.' }
        ]
      }
    }));
    const view = {
      get state() { return state; },
      dispatch(transaction: Parameters<EditorState['apply']>[0]) {
        state = state.apply(transaction);
      }
    } as unknown as EditorView;

    const handled = plugin.props.handleKeyDown?.call(plugin, view, {
      key: 'ArrowLeft', altKey: true, metaKey: false, ctrlKey: false,
      isComposing: false, keyCode: 37, preventDefault() {}
    } as unknown as KeyboardEvent);

    expect(handled).toBe(true);
    expect(modifierStates).toEqual([true]);
    expect(unconsumed).toEqual([' for rain.']);
    expect(state.doc.textContent).toBe('A waits');
  });

  it('chooses the highlighted alternative with Option-Return while the inline ghost is hidden', () => {
    const inserted: string[] = [];
    const modifierStates: boolean[] = [];
    const plugin = createGhostTextPlugin({
      accept: () => true,
      dismiss() {},
      visible: () => false,
      insert: (_candidateId, _presentationKey, text) => {
        inserted.push(text);
        return true;
      },
      modifier: (held) => modifierStates.push(held)
    });
    const doc = defaultMarkdownParser.parse('A waits');
    let state = EditorState.create({ doc, selection: Selection.atEnd(doc), plugins: [plugin] });
    state = state.apply(state.tr.setMeta(ghostTextPluginKey, {
      kind: 'set',
      presentation: {
        ...suggestion,
        text: ' for rain.',
        fanVisible: true,
        alternatives: [
          { candidateId: 'a', presentationKey: suggestion.presentationKey, text: ' for rain.' },
          { candidateId: 'b', presentationKey: 'b:1', text: ' until dawn.' }
        ]
      }
    }));
    const clip = { left: 0, top: 0, right: 500, bottom: 500 };
    let widget: Record<string, unknown>;
    const ownerDocument = {
      defaultView: {
        getComputedStyle: (element: unknown) => ({
          display: 'inline',
          visibility: element === widget ? 'hidden' : 'visible',
          opacity: '1',
          direction: 'ltr'
        })
      }
    };
    const dom = {
      isConnected: true,
      hidden: false,
      ownerDocument,
      parentElement: null,
      querySelectorAll: () => [widget],
      closest: () => ({ getBoundingClientRect: () => clip })
    };
    widget = {
      isConnected: true,
      hidden: false,
      ownerDocument,
      parentElement: dom,
      getAttribute: () => suggestion.presentationKey,
      getBoundingClientRect: () => ({ left: 40, top: 40, right: 120, bottom: 60 })
    };
    const view = {
      get state() { return state; },
      dom,
      coordsAtPos: () => ({ left: 40, top: 40, right: 40, bottom: 60 }),
      dispatch(transaction: Parameters<EditorState['apply']>[0]) { state = state.apply(transaction); }
    } as unknown as EditorView;
    const handled = plugin.props.handleKeyDown?.call(plugin, view, {
      key: 'Enter', altKey: true, metaKey: false, ctrlKey: false,
      isComposing: false, keyCode: 13, preventDefault() {}
    } as unknown as KeyboardEvent);
    expect(handled).toBe(true);
    expect(modifierStates).toEqual([true]);
    expect(inserted).toEqual([' for rain.']);
    expect(state.doc.textContent).toBe('A waits for rain.');
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

  it('reports the exact invariant that rejected a visual caret proof', () => {
    const doc = defaultMarkdownParser.parse('hello');
    const cursor = EditorState.create({ doc, selection: TextSelection.create(doc, 3) });
    const range = EditorState.create({ doc, selection: TextSelection.create(doc, 2, 4) });
    const end = EditorState.create({ doc, selection: Selection.atEnd(doc) });

    expect(visualCaretBoundaryProof(cursor, 'different')).toEqual({
      byteOffset: null,
      failure: 'canonical_mismatch',
      diagnostic: 'canonical_utf8=9,restored_utf8=5,first_utf16_difference=0'
    });
    expect(visualCaretBoundaryProof(range, 'hello')).toEqual({
      byteOffset: null,
      failure: 'selection_range',
      diagnostic: null
    });
    expect(visualCaretBoundaryProof(end, 'hello')).toEqual({
      byteOffset: 5,
      failure: null,
      diagnostic: null
    });
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

  it('anchors the next completion after an exact terminal prose separator', () => {
    const markdown = 'Something ';
    const doc = parseVisualMarkdown(markdown);
    const state = EditorState.create({ doc, selection: Selection.atEnd(doc) });
    const anchor = new TextEncoder().encode(markdown).byteLength;
    expect(exactMarkdownByteOffsetAtSelection(state, markdown)).toBe(anchor);
    expect(visualGhostTextIsFaithfulAtSelection(
      state,
      markdown,
      anchor,
      'lingers in the hallway.'
    )).toBe(true);
  });

  it('rejects multiline block structure, Markdown controls, and the wrong anchor', () => {
    const markdown = 'The rain waits.';
    const doc = defaultMarkdownParser.parse(markdown);
    const state = EditorState.create({ doc, selection: Selection.atEnd(doc) });
    const anchor = new TextEncoder().encode(markdown).byteLength;
    expect(visualGhostTextMayBePlainProse(' **boldly**')).toBe(false);
    expect(visualGhostTextMayBePlainProse('\n\nMorning came.\n\nThe bells answered.')).toBe(false);
    expect(visualGhostTextMayBePlainProse('\n\n# Morning came.')).toBe(false);
    expect(visualGhostTextMayBePlainProse('\n\n- Morning came.')).toBe(false);
    expect(visualGhostTextIsFaithfulAtSelection(
      state,
      markdown,
      anchor,
      '\n\nMorning came.\n\nThe bells answered.'
    )).toBe(false);
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

  it('surfaces the useful prose prefix when a completion later wanders into markup', () => {
    expect(visualGhostTextSafePrefix(' The door opened.\n# Notes')).toBe(' The door opened.');
    expect(visualGhostTextSafePrefix(' The door opened.\n\nMorning came.'))
      .toBe(' The door opened.');
    expect(visualGhostTextSafePrefix(' She waited **boldly**')).toBe(' She waited');
    expect(visualGhostTextSafePrefix(' **boldly**')).toBeNull();
    expect(visualGhostTextSafePrefix(' ordinary prose')).toBe(' ordinary prose');
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
  it('rebuilds an unchanged widget only for an explicit lifecycle refresh', () => {
    let state = stateAtEnd('The sentence waits', true);
    let dispatchCount = 0;
    const view = {
      get state() { return state; },
      dispatch(transaction: Parameters<EditorView['dispatch']>[0]) {
        dispatchCount += 1;
        state = state.apply(transaction);
      }
    } as unknown as EditorView;

    setGhostText(view, suggestion);
    expect(dispatchCount).toBe(1);
    expect(ghostTextPluginKey.getState(state)?.renderEpoch).toBe(0);

    setGhostText(view, suggestion);
    expect(dispatchCount).toBe(1);

    setGhostText(view, suggestion, true);
    expect(dispatchCount).toBe(2);
    expect(ghostTextPluginKey.getState(state)?.renderEpoch).toBe(1);
    expect(renderedGhostPresentationKey(state)).toBe(suggestion.presentationKey);

    setGhostText(view, suggestion, true);
    expect(dispatchCount).toBe(3);
    expect(ghostTextPluginKey.getState(state)?.renderEpoch).toBe(2);
  });

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

  it('survives an explicit same-selection reconciliation but clears when the caret moves', () => {
    let state = stateAtEnd('The sentence waits', true);
    const view = {
      get state() { return state; },
      dispatch(transaction: Parameters<EditorView['dispatch']>[0]) {
        state = state.apply(transaction);
      }
    } as unknown as EditorView;

    setGhostText(view, suggestion);
    const exactSelection = state.selection;
    const sameSelection = state.tr.setSelection(TextSelection.create(
      state.doc,
      exactSelection.from,
      exactSelection.to
    ));
    expect(sameSelection.selectionSet).toBe(true);
    state = state.apply(sameSelection);
    expect(ghostTextPluginKey.getState(state)?.presentationKey).toBe(suggestion.presentationKey);
    expect(renderedGhostPresentationKey(state)).toBe(suggestion.presentationKey);

    state = state.apply(state.tr.setSelection(TextSelection.create(
      state.doc,
      exactSelection.from - 1
    )));
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
