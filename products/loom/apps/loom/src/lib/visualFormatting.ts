import {
  lift,
  setBlockType,
  toggleMark,
  wrapIn
} from 'prosemirror-commands';
import { defaultMarkdownSerializer, schema } from 'prosemirror-markdown';
import { liftListItem, wrapInList } from 'prosemirror-schema-list';
import type { Command, EditorState, Transaction } from 'prosemirror-state';
import { parseVisualMarkdown } from './markdownSafety';

export type VisualBlockStyle = 'body' | 'title' | 'heading' | 'subheading';
export type VisualFormatAction =
  | VisualBlockStyle
  | 'bold'
  | 'italic'
  | 'blockquote'
  | 'bullet_list'
  | 'ordered_list'
  | 'link'
  | 'unlink';

export interface VisualFormatState {
  block: VisualBlockStyle;
  bold: boolean;
  italic: boolean;
  blockquote: boolean;
  bulletList: boolean;
  orderedList: boolean;
  linkHref: string;
  selectionEmpty: boolean;
}

function ancestorIs(state: EditorState, nodeName: string): boolean {
  for (let depth = state.selection.$from.depth; depth >= 0; depth -= 1) {
    if (state.selection.$from.node(depth).type.name === nodeName) return true;
  }
  return false;
}

function activeMark(state: EditorState, markName: 'strong' | 'em' | 'link') {
  const mark = schema.marks[markName];
  if (state.selection.empty) {
    return (state.storedMarks ?? state.selection.$from.marks())
      .find((candidate) => candidate.type === mark) ?? null;
  }
  let found = null;
  state.doc.nodesBetween(state.selection.from, state.selection.to, (node) => {
    found ??= node.marks.find((candidate) => candidate.type === mark) ?? null;
    return found === null;
  });
  return found;
}

export function visualFormatState(state: EditorState): VisualFormatState {
  const parent = state.selection.$from.parent;
  const level = parent.type === schema.nodes.heading ? Number(parent.attrs.level) : 0;
  const link = activeMark(state, 'link');
  return {
    block: level === 1 ? 'title' : level === 2 ? 'heading' : level === 3 ? 'subheading' : 'body',
    bold: Boolean(activeMark(state, 'strong')),
    italic: Boolean(activeMark(state, 'em')),
    blockquote: ancestorIs(state, 'blockquote'),
    bulletList: ancestorIs(state, 'bullet_list'),
    orderedList: ancestorIs(state, 'ordered_list'),
    linkHref: typeof link?.attrs.href === 'string' ? link.attrs.href : '',
    selectionEmpty: state.selection.empty
  };
}

function listCommand(state: EditorState, ordered: boolean): Command {
  const activeName = ordered ? 'ordered_list' : 'bullet_list';
  return ancestorIs(state, activeName)
    ? liftListItem(schema.nodes.list_item)
    : wrapInList(
        ordered ? schema.nodes.ordered_list : schema.nodes.bullet_list,
        ordered ? { order: 1, tight: true } : { tight: true }
      );
}

export function visualFormatCommand(
  state: EditorState,
  action: VisualFormatAction,
  href = ''
): Command | null {
  switch (action) {
    case 'body': return setBlockType(schema.nodes.paragraph);
    case 'title': return setBlockType(schema.nodes.heading, { level: 1 });
    case 'heading': return setBlockType(schema.nodes.heading, { level: 2 });
    case 'subheading': return setBlockType(schema.nodes.heading, { level: 3 });
    case 'bold': return toggleMark(schema.marks.strong);
    case 'italic': return toggleMark(schema.marks.em);
    case 'blockquote': return ancestorIs(state, 'blockquote') ? lift : wrapIn(schema.nodes.blockquote);
    case 'bullet_list': return listCommand(state, false);
    case 'ordered_list': return listCommand(state, true);
    case 'link': {
      const normalized = href.trim();
      if (!normalized || /[\u0000-\u001f\u007f\s]/u.test(normalized) || state.selection.empty) return null;
      return toggleMark(schema.marks.link, { href: normalized, title: null });
    }
    case 'unlink': {
      if (state.selection.empty) return null;
      return (_state, dispatch) => {
        dispatch?.(_state.tr.removeMark(_state.selection.from, _state.selection.to, schema.marks.link));
        return true;
      };
    }
  }
}

export function applyVisualFormat(
  state: EditorState,
  action: VisualFormatAction,
  href: string,
  dispatch: (transaction: Transaction) => void
): boolean {
  const command = visualFormatCommand(state, action, href);
  if (!command) return false;
  let transaction: Transaction | null = null;
  if (!command(state, (next) => { transaction = next; })) return false;
  if (!transaction) return false;
  const nextState = state.apply(transaction);
  const markdown = defaultMarkdownSerializer.serialize(nextState.doc);
  if (!parseVisualMarkdown(markdown).eq(nextState.doc)) return false;
  dispatch(transaction);
  return true;
}
