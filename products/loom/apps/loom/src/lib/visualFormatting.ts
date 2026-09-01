import {
  lift,
  setBlockType,
  toggleMark,
  wrapIn
} from 'prosemirror-commands';
import { defaultMarkdownSerializer, schema } from 'prosemirror-markdown';
import type { Node as ProseMirrorNode } from 'prosemirror-model';
import { liftListItem, wrapInList } from 'prosemirror-schema-list';
import {
  AllSelection,
  Selection,
  TextSelection,
  type Command,
  type EditorState,
  type Transaction
} from 'prosemirror-state';
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

function structureIsActive(state: EditorState, nodeName: string): boolean {
  if (state.selection.empty) return ancestorIs(state, nodeName);

  let active = false;
  state.doc.nodesBetween(state.selection.from, state.selection.to, (node, position) => {
    if (!node.isTextblock) return true;
    const inside = state.doc.resolve(Math.min(position + 1, state.doc.content.size));
    for (let depth = inside.depth; depth >= 0; depth -= 1) {
      if (inside.node(depth).type.name === nodeName) {
        active = true;
        break;
      }
    }
    return !active;
  });
  return active;
}

interface SelectedStructure {
  node: ProseMirrorNode;
  position: number;
}

function selectedStructures(state: EditorState, nodeName: string): SelectedStructure[] {
  const selected: SelectedStructure[] = [];
  state.doc.nodesBetween(state.selection.from, state.selection.to, (node, position) => {
    if (node.type.name !== nodeName) return true;
    selected.push({ node, position });
    // The outer selected structure owns its nested content. Replacing nested
    // matches separately would make their recorded positions stale.
    return false;
  });
  return selected;
}

function unwrappedContent(node: ProseMirrorNode): readonly ProseMirrorNode[] {
  const blocks: ProseMirrorNode[] = [];
  if (node.type === schema.nodes.bullet_list || node.type === schema.nodes.ordered_list) {
    node.forEach((item) => item.forEach((block) => blocks.push(block)));
  } else {
    node.forEach((block) => blocks.push(block));
  }
  // Return nodes created by the document's own ProseMirror schema. Importing
  // and constructing a Fragment here can cross Vite's optimized dependency
  // instances and fail in WebKit even though the unit runner has one copy.
  return blocks;
}

function unwrapSelectedStructures(nodeName: string): Command {
  return (state, dispatch) => {
    const selected = selectedStructures(state, nodeName);
    if (selected.length === 0) return false;
    if (!dispatch) return true;

    let transaction = state.tr;
    // Descending positions keep every recorded range stable while earlier
    // replacements change document size.
    for (const { node, position } of selected.sort((left, right) => right.position - left.position)) {
      transaction = transaction.replaceWith(
        position,
        position + node.nodeSize,
        unwrappedContent(node)
      );
    }
    if (!transaction.docChanged) return false;
    dispatch(transaction);
    return true;
  };
}

function unwrapWithFallback(primary: Command, nodeName: string): Command {
  const fallback = unwrapSelectedStructures(nodeName);
  return (state, dispatch, view) => {
    // A shared ProseMirror lift range cannot represent disjoint containers or
    // a mixed plain/structured selection. Normalize those selections in one
    // transaction instead of treating a failed lift as a formatting no-op.
    if (state.selection instanceof AllSelection || selectedStructures(state, nodeName).length > 1) {
      return fallback(state, dispatch, view);
    }
    return primary(state, dispatch, view) || fallback(state, dispatch, view);
  };
}

function fullTextSelection(document: ProseMirrorNode): TextSelection | null {
  const start = Selection.findFrom(document.resolve(0), 1, true);
  const end = Selection.findFrom(document.resolve(document.content.size), -1, true);
  if (!(start instanceof TextSelection) || !(end instanceof TextSelection)) return null;
  // A text range cannot retain Select All semantics when a leaf block sits
  // outside either endpoint. Keep AllSelection in that case so the next
  // structural toggle still owns those nodes.
  if (
    !(Selection.atStart(document) instanceof TextSelection) ||
    !(Selection.atEnd(document) instanceof TextSelection)
  ) return null;
  return TextSelection.create(document, start.from, end.to);
}

function withVisibleTextSelectionResult(command: Command): Command {
  return (state, dispatch, view) => {
    const mapAllSelection = state.selection instanceof AllSelection && dispatch;
    return command(state, mapAllSelection ? (transaction) => {
      // A structural command may replace wrappers in descending order and map
      // browser Select All to wrapper boundaries. Publish one atomic
      // transaction whose final selection spans the writer-visible text, but
      // retain AllSelection when leaf boundaries make a total text range
      // impossible without dropping part of the selected document.
      const mappedText = fullTextSelection(transaction.doc);
      if (mappedText) transaction.setSelection(mappedText);
      dispatch(transaction);
    } : dispatch, view);
  };
}

function trimmedInlineRange(state: EditorState): { from: number; to: number } | null {
  const selection = state.selection;
  if (selection.empty) return null;
  let from: number | null = null;
  let to: number | null = null;
  state.doc.nodesBetween(selection.from, selection.to, (node, position) => {
    if (!node.isText || !node.text) return true;
    const selectedFrom = Math.max(selection.from, position);
    const selectedTo = Math.min(selection.to, position + node.nodeSize);
    const selectedText = node.text.slice(selectedFrom - position, selectedTo - position);
    const first = selectedText.search(/\S/u);
    if (first < 0) return false;
    const trailing = selectedText.match(/\s*$/u)?.[0].length ?? 0;
    const last = selectedText.length - trailing;
    from ??= selectedFrom + first;
    to = selectedFrom + last;
    return false;
  });
  return from !== null && to !== null && from < to ? { from, to } : null;
}

/**
 * Markdown delimiters cannot faithfully own leading/trailing whitespace.
 * Apply an inline command to the writer-visible non-whitespace range, then
 * restore the exact original selection (including browser Select All).
 */
function withTrimmedInlineSelection(command: Command): Command {
  return (state, dispatch, view) => {
    if (state.selection.empty) return command(state, dispatch, view);
    const range = trimmedInlineRange(state);
    if (!range) return false;
    const selected = state.apply(state.tr
      .setSelection(TextSelection.create(state.doc, range.from, range.to))
      .setMeta('addToHistory', false));
    return command(selected, dispatch ? (transaction) => {
      transaction.setSelection(state.selection.map(transaction.doc, transaction.mapping));
      dispatch(transaction);
    } : undefined, view);
  };
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
    blockquote: structureIsActive(state, 'blockquote'),
    bulletList: structureIsActive(state, 'bullet_list'),
    orderedList: structureIsActive(state, 'ordered_list'),
    linkHref: typeof link?.attrs.href === 'string' ? link.attrs.href : '',
    selectionEmpty: state.selection.empty
  };
}

function listCommand(state: EditorState, ordered: boolean): Command {
  const activeName = ordered ? 'ordered_list' : 'bullet_list';
  const command = structureIsActive(state, activeName)
    ? unwrapWithFallback(liftListItem(schema.nodes.list_item), activeName)
    : wrapInList(
        ordered ? schema.nodes.ordered_list : schema.nodes.bullet_list,
        ordered ? { order: 1, tight: true } : { tight: true }
      );
  return withVisibleTextSelectionResult(command);
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
    case 'bold': return withTrimmedInlineSelection(toggleMark(schema.marks.strong));
    case 'italic': return withTrimmedInlineSelection(toggleMark(schema.marks.em));
    case 'blockquote': return withVisibleTextSelectionResult(
      structureIsActive(state, 'blockquote')
        ? unwrapWithFallback(lift, 'blockquote')
        : wrapIn(schema.nodes.blockquote)
    );
    case 'bullet_list': return listCommand(state, false);
    case 'ordered_list': return listCommand(state, true);
    case 'link': {
      const normalized = href.trim();
      if (!normalized || /[\u0000-\u001f\u007f\s]/u.test(normalized) || state.selection.empty) return null;
      return (_state, dispatch) => {
        const range = trimmedInlineRange(_state);
        if (!range) return false;
        const mark = schema.marks.link.create({ href: normalized, title: null });
        dispatch?.(_state.tr
          .removeMark(range.from, range.to, schema.marks.link)
          .addMark(range.from, range.to, mark));
        return true;
      };
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
