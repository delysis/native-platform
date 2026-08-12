import { defaultMarkdownParser } from 'prosemirror-markdown';
import { EditorState, Selection } from 'prosemirror-state';
import type { EditorView } from 'prosemirror-view';
import { describe, expect, it } from 'vitest';
import {
  createGhostTextPlugin,
  setGhostText,
  VISUAL_TAB_INDENT
} from '../src/lib/ghostText';

describe('W1 stale Tab evidence', () => {
  it('does not promote the stale suggestion and inserts an ordinary tab', () => {
    let accepted = 0;
    const plugin = createGhostTextPlugin({
      accept: () => {
        accepted += 1;
        return true;
      },
      dismiss: () => {},
      visible: () => false
    });
    const doc = defaultMarkdownParser.parse('The exact manuscript');
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

    setGhostText(view, {
      active: true,
      candidateId: 'stale-candidate',
      presentationKey: 'stale-presentation',
      surfaceKey: 'stale-surface',
      anchorByteOffset: 20,
      text: ' that must not promote'
    });
    expect(plugin.props.handleKeyDown?.call(plugin, view, {
      key: 'Tab',
      keyCode: 9,
      isComposing: false,
      shiftKey: false,
      metaKey: false,
      ctrlKey: false,
      altKey: false
    } as KeyboardEvent)).toBe(true);
    expect(accepted).toBe(0);
    expect(state.doc.textContent).toBe(`The exact manuscript${VISUAL_TAB_INDENT}`);
  });
});
