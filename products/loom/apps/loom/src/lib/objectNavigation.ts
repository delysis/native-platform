import { splitBlock } from 'prosemirror-commands';
import { NodeSelection, Plugin, Selection, TextSelection } from 'prosemirror-state';
import type { EditorView } from 'prosemirror-view';

function renderedShader(view: EditorView, position: number): boolean {
  const dom = view.nodeDOM(position);
  return dom instanceof HTMLElement && dom.dataset.shaderMode === 'render';
}

/** Object movement changes selection only. Enter explicitly creates writing space. */
export function objectNavigation(): Plugin {
  let editor: EditorView | undefined;
  return new Plugin({
    view(view) {
      editor = view;
      return {
        update(view, previous) {
          const prior = previous.selection;
          if (!(prior instanceof TextSelection) || prior.$head.parent.type.name !== 'code_block') return;
          const position = prior.$head.before();
          const current = view.state.selection;
          if (current instanceof TextSelection && current.$head.parent.type.name === 'code_block' &&
              current.$head.before() === position) return;
          const dom = view.nodeDOM(position);
          if (dom instanceof HTMLElement && dom.dataset.shaderMode === 'source') {
            dom.dispatchEvent(new Event('loom-leave-object'));
          }
        },
        destroy() { editor = undefined; }
      };
    },
    appendTransaction(_transactions, _old, state) {
      const { selection } = state;
      if (!editor || !(selection instanceof TextSelection) || !selection.empty ||
          selection.$head.parent.type.name !== 'code_block') return null;
      const position = selection.$head.before();
      return renderedShader(editor, position)
        ? state.tr.setSelection(NodeSelection.create(state.doc, position))
        : null;
    },
    props: {
      handleKeyDown(view, event) {
        if (event.altKey || event.metaKey || event.ctrlKey || view.composing) return false;
        const { state } = view;
        const { selection } = state;
        if (event.key === 'Escape' && selection instanceof TextSelection &&
            selection.$head.parent.type.name === 'code_block') {
          const dom = view.nodeDOM(selection.$head.before());
          if (dom instanceof HTMLElement && dom.dataset.shaderMode === 'source') {
            dom.dispatchEvent(new Event('loom-render-object'));
            return true;
          }
        }
        const direction = event.key === 'ArrowLeft' || event.key === 'ArrowUp' ? -1
          : event.key === 'ArrowRight' || event.key === 'ArrowDown' ? 1 : 0;
        if (selection instanceof NodeSelection &&
            (selection.node.type.name === 'image' || renderedShader(view, selection.from))) {
          if (event.key === 'Enter' && selection.node.isInline) {
            if (!view.editable) return true;
            view.dispatch(state.tr.setSelection(TextSelection.create(state.doc, selection.to)));
            splitBlock(view.state, view.dispatch);
            return true;
          }
          if (event.key === 'Enter' && selection.node.isBlock) {
            if (!view.editable) return true;
            const boundary = event.shiftKey ? selection.from : selection.to;
            const resolved = state.doc.resolve(boundary);
            const paragraph = state.schema.nodes.paragraph;
            if (!resolved.parent.canReplaceWith(resolved.index(), resolved.index(), paragraph)) return true;
            const transaction = state.tr.insert(boundary, paragraph.create());
            view.dispatch(transaction.setSelection(TextSelection.create(transaction.doc, boundary + 1)).scrollIntoView());
            return true;
          }
          if (!direction || event.shiftKey) return false;
          const boundary = direction < 0 ? selection.from : selection.to;
          const next = selection.node.isInline
            ? TextSelection.create(state.doc, boundary)
            : Selection.findFrom(state.doc.resolve(boundary), direction, true);
          if (next) view.dispatch(state.tr.setSelection(next).scrollIntoView());
          return true;
        }
        if (!direction || event.shiftKey || !(selection instanceof TextSelection) || !selection.empty) return false;
        const { $head } = selection;
        if (!$head.parent.isTextblock) return false;
        const vertical = event.key === 'ArrowUp' || event.key === 'ArrowDown';
        const atEdge = vertical
          ? view.endOfTextblock(direction < 0 ? 'up' : 'down')
          : $head.parentOffset === (direction < 0 ? 0 : $head.parent.content.size);
        if (!atEdge) return false;
        const boundary = direction < 0 ? $head.before() : $head.after();
        const resolved = state.doc.resolve(boundary);
        const neighbor = direction < 0 ? resolved.nodeBefore : resolved.nodeAfter;
        const position = direction < 0 ? boundary - (neighbor?.nodeSize ?? 0) : boundary;
        if (!neighbor || !renderedShader(view, position)) return false;
        view.dispatch(state.tr.setSelection(NodeSelection.create(state.doc, position)).scrollIntoView());
        return true;
      }
    }
  });
}
