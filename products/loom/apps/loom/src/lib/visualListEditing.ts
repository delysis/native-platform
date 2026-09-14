import { keymap } from 'prosemirror-keymap';
import { schema as markdownSchema } from 'prosemirror-markdown';
import type { Schema } from 'prosemirror-model';
import { liftListItem, sinkListItem, splitListItem } from 'prosemirror-schema-list';
import type { Command, EditorState, Plugin } from 'prosemirror-state';
import type { EditorView } from 'prosemirror-view';

export type VisualListTabReservation = (
  state: EditorState,
  view: EditorView | undefined
) => boolean;

export function visualListKeyBindings(schema: Schema = markdownSchema): Readonly<Record<string, Command>> {
  const listItem = schema.nodes.list_item;
  return {
    Enter: splitListItem(listItem),
    Tab: sinkListItem(listItem),
    'Shift-Tab': liftListItem(listItem)
  };
}

export function visualListKeymap(tabReserved: VisualListTabReservation, schema: Schema = markdownSchema): Plugin {
  const bindings = visualListKeyBindings(schema);
  return keymap({
    ...bindings,
    Tab: (state, dispatch, view) => tabReserved(state, view)
      ? false
      : bindings.Tab(state, dispatch, view)
  });
}
