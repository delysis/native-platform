import { EditorState, NodeSelection, TextSelection } from 'prosemirror-state';
import { visualCaretBoundaryProof, visualInlineNodeBoundaryProof } from './ghostText';

export interface TerminalSourceRange { start: number; end: number }

/** Read the immutable selection through the same Markdown boundary proof as autocomplete. */
export function visualTerminalRange(state: EditorState, markdown: string): TerminalSourceRange | null {
  const selection = state.selection;
  if (selection instanceof NodeSelection) {
    const start = visualInlineNodeBoundaryProof(state, markdown, 'from').byteOffset;
    const end = visualInlineNodeBoundaryProof(state, markdown, 'to').byteOffset;
    return start === null || end === null ? null : { start, end };
  }
  if (!(selection instanceof TextSelection)) return null;
  const boundary = (position: number) => visualCaretBoundaryProof(
    EditorState.create({ doc: state.doc, selection: TextSelection.create(state.doc, position) }),
    markdown
  ).byteOffset;
  const start = boundary(state.selection.from);
  const end = state.selection.empty ? start : boundary(state.selection.to);
  return start === null || end === null ? null : { start, end };
}
