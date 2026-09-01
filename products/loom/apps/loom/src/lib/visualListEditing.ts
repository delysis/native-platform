import { keymap } from 'prosemirror-keymap';
import { schema } from 'prosemirror-markdown';
import { liftListItem, sinkListItem, splitListItem } from 'prosemirror-schema-list';
import type { Command, EditorState, Plugin } from 'prosemirror-state';
import type { EditorView } from 'prosemirror-view';

export type VisualListTabReservation = (
  state: EditorState,
  view: EditorView | undefined
) => boolean;

export function visualListKeyBindings(): Readonly<Record<string, Command>> {
  const listItem = schema.nodes.list_item;
  return {
    Enter: splitListItem(listItem),
    Tab: sinkListItem(listItem),
    'Shift-Tab': liftListItem(listItem)
  };
}

export function visualListKeymap(tabReserved: VisualListTabReservation): Plugin {
  const bindings = visualListKeyBindings();
  return keymap({
    ...bindings,
    Tab: (state, dispatch, view) => tabReserved(state, view)
      ? false
      : bindings.Tab(state, dispatch, view)
  });
}
