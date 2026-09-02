import { baseKeymap } from 'prosemirror-commands';
import { defaultMarkdownParser, defaultMarkdownSerializer } from 'prosemirror-markdown';
import type { Node as ProseMirrorNode } from 'prosemirror-model';
import { EditorState, TextSelection, type Command } from 'prosemirror-state';
import { describe, expect, it } from 'vitest';
import { visualListKeyBindings } from './visualListEditing';

function stateAtTextEnd(markdown: string, text: string): EditorState {
  const doc = defaultMarkdownParser.parse(markdown);
  let position: number | null = null;
  doc.descendants((node: ProseMirrorNode, offset: number) => {
    if (node.isText && node.text === text) position = offset + node.nodeSize;
  });
  if (position === null) throw new Error(`missing text: ${text}`);
  return EditorState.create({
    doc,
    selection: TextSelection.create(doc, position)
  });
}

function apply(state: EditorState, command: Command): { handled: boolean; state: EditorState } {
  let transaction: Parameters<EditorState['apply']>[0] | null = null;
  const handled = command(state, (next) => { transaction = next; });
  return {
    handled,
    state: transaction ? state.apply(transaction) : state
  };
}

function markdown(state: EditorState): string {
  return defaultMarkdownSerializer.serialize(state.doc);
}

describe('visual list key bindings', () => {
  const bindings = visualListKeyBindings();

  it.each([
    ['ordered', '1. One\n2. Two', '2. Two\n3. '],
    ['bullet', '* One\n* Two', '* Two\n* ']
  ])('splits an active %s list item on Enter', (_kind, source, suffix) => {
    const split = apply(stateAtTextEnd(source, 'Two'), bindings.Enter);
    expect(split.handled).toBe(true);
    expect(markdown(split.state)).toContain(suffix);
  });

  it('lets the generic Enter fallback exit an empty final list item', () => {
    const first = apply(stateAtTextEnd('1. One\n2. Two', 'Two'), bindings.Enter);
    const listDeclined = apply(first.state, bindings.Enter);
    expect(listDeclined.handled).toBe(false);

    const exited = apply(listDeclined.state, baseKeymap.Enter);
    expect(exited.handled).toBe(true);
    expect(exited.state.doc.lastChild?.type.name).toBe('paragraph');
    expect(markdown(exited.state)).toBe('1. One\n2. Two');
  });

  it('sinks with Tab and lifts with Shift-Tab', () => {
    const sunk = apply(stateAtTextEnd('1. One\n2. Two', 'Two'), bindings.Tab);
    expect(sunk.handled).toBe(true);
    expect(markdown(sunk.state)).toBe('1. One\n   1. Two');

    const lifted = apply(sunk.state, bindings['Shift-Tab']);
    expect(lifted.handled).toBe(true);
    expect(markdown(lifted.state)).toBe('1. One\n2. Two');
  });
});
