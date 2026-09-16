import { describe, expect, it } from 'vitest';
import { defaultMarkdownParser } from 'prosemirror-markdown';
import { EditorState, TextSelection } from 'prosemirror-state';
import { history, undo } from 'prosemirror-history';
import { applyRemoteDocument, mapRemoteOffset } from './remoteEditor';

describe('remote document updates', () => {
  it('preserves local undo and a selection between two remote edits in one paragraph', () => {
    let state = EditorState.create({ doc: defaultMarkdownParser.parse('One middle three'), plugins: [history()] });
    state = state.apply(state.tr.insertText('local ', 5));
    state = state.apply(state.tr.setSelection(TextSelection.create(state.doc, 11, 17)));
    state = applyRemoteDocument(state, defaultMarkdownParser.parse('First One local middle three last'));
    expect(state.doc.textBetween(state.selection.from, state.selection.to)).toBe('middle');
    expect(undo(state, transaction => state = state.apply(transaction))).toBe(true);
    expect(state.doc.textContent).toBe('First One middle three last');
    expect(mapRemoteOffset('One middle three', 'First One middle three last', 4)).toBe(10);
  });

  it('keeps remote text when undoing a local insertion', () => {
    let state = EditorState.create({ doc: defaultMarkdownParser.parse('Hello world'), plugins: [history()] });
    state = state.apply(state.tr.insertText('local ', 7));
    state = state.apply(state.tr.setSelection(TextSelection.create(state.doc, 13)));
    state = applyRemoteDocument(state, defaultMarkdownParser.parse('Hello local world, remotely'));
    expect(state.selection.from).toBe(13);
    expect(undo(state, transaction => state = state.apply(transaction))).toBe(true);
    expect(state.doc.textContent).toBe('Hello world, remotely');
  });

  it('moves a selection with text inserted before it', () => {
    let state = EditorState.create({ doc: defaultMarkdownParser.parse('Hello world') });
    state = state.apply(state.tr.setSelection(TextSelection.create(state.doc, 7, 12)));
    state = applyRemoteDocument(state, defaultMarkdownParser.parse('Hello lovely world'));
    expect(state.doc.textBetween(state.selection.from, state.selection.to)).toBe('world');
  });

  it('maps textarea positions in UTF-16 without dropping an emoji', () => {
    expect(mapRemoteOffset('🌱 garden', 'Our 🌱 garden', 3)).toBe(7);
    expect(mapRemoteOffset('before xxx after', 'before new after', 8)).toBe(10);
  });
});
