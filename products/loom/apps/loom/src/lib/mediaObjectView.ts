import type { Node as ProseMirrorNode } from 'prosemirror-model';
import { NodeSelection, TextSelection } from 'prosemirror-state';
import type { EditorView, NodeView } from 'prosemirror-view';
import { parseVisualMarkdown, serializeVisualMarkdown } from './markdownSafety';
import { objectMenu } from './objectMenu';

const drafts = new WeakMap<EditorView, Set<() => boolean>>();

export function flushMediaObjectDrafts(editor: EditorView): boolean {
  for (const flush of [...(drafts.get(editor) ?? [])]) if (!flush()) return false;
  return true;
}

/** An atom's draft is local until Enter validates and replaces exactly that atom. */
export function mediaObjectView(
  node: ProseMirrorNode, editor: EditorView, getPos: () => number | undefined, media: HTMLElement
): NodeView {
  const dom = document.createElement('span');
  dom.className = 'loom-media-object';
  dom.contentEditable = 'false';
  dom.append(media);
  let input: HTMLInputElement | undefined;
  let error: HTMLElement | undefined;
  let composing = false;
  const markdown = () => serializeVisualMarkdown(node.type.schema.nodes.doc.create(null,
    node.type.schema.nodes.paragraph.create(null, node)));

  function select(): void {
    const position = getPos();
    if (position !== undefined) editor.dispatch(editor.state.tr.setSelection(NodeSelection.create(editor.state.doc, position)));
  }
  function cancel(focus = true): void {
    input?.remove(); input = undefined;
    error?.remove(); error = undefined;
    media.style.display = '';
    if (focus) { select(); editor.focus(); }
  }
  function apply(focus = true): boolean {
    if (!input) return true;
    if (composing) return false;
    if (input.value === markdown()) { cancel(focus); return true; }
    if (!editor.editable) return false;
    try {
      const parsed = parseVisualMarkdown(input.value);
      const replacement = parsed.firstChild?.firstChild;
      if (parsed.childCount !== 1 || parsed.firstChild?.type.name !== 'paragraph' ||
          parsed.firstChild.childCount !== 1 || replacement?.type !== node.type ||
          serializeVisualMarkdown(parsed) !== input.value) throw new Error('Enter one image or audio Markdown reference.');
      const position = getPos();
      if (position === undefined) return false;
      if (replacement.eq(node)) { cancel(focus); return true; }
      const transaction = editor.state.tr.replaceWith(position, position + node.nodeSize, replacement);
      editor.dispatch(transaction.setSelection(TextSelection.create(transaction.doc, position + replacement.nodeSize)).scrollIntoView());
      if (focus) editor.focus();
      return true;
    } catch (failure) {
      if (!error) {
        error = document.createElement('small');
        error.className = 'loom-object-error';
        error.setAttribute('role', 'status');
        dom.append(error);
      }
      error.textContent = failure instanceof Error ? failure.message : 'This reference could not be read.';
      return false;
    }
  }
  function edit(): void {
    if (input) { apply(); return; }
    select();
    media.style.display = 'none';
    input = document.createElement('input');
    input.type = 'text';
    input.className = 'loom-object-source';
    input.setAttribute('aria-label', 'Object Markdown');
    input.value = markdown();
    input.readOnly = !editor.editable;
    input.addEventListener('compositionstart', () => { composing = true; });
    input.addEventListener('compositionend', () => { composing = false; });
    input.addEventListener('keydown', (event) => {
      if (event.isComposing) return;
      if (event.key !== 'Enter' && event.key !== 'Escape') return;
      event.preventDefault(); event.stopPropagation();
      if (event.key === 'Escape') cancel(); else apply();
    });
    dom.append(input);
    input.focus();
    input.select();
  }
  dom.addEventListener('mousedown', (event) => { if (!input && event.button === 0) select(); });
  const disposeMenu = objectMenu(dom, () => input ? 'Apply' : 'Edit', edit, () => input ? input.focus() : editor.focus());
  const flush = () => apply(false);
  const editorDrafts = drafts.get(editor) ?? new Set<() => boolean>();
  drafts.set(editor, editorDrafts);
  editorDrafts.add(flush);
  return {
    dom,
    selectNode() { dom.classList.add('ProseMirror-selectednode'); },
    deselectNode() { dom.classList.remove('ProseMirror-selectednode'); },
    stopEvent(event) { return event.type === 'contextmenu' || Boolean(input) || event.target instanceof HTMLAudioElement; },
    ignoreMutation() { return true; },
    destroy() { disposeMenu(); editorDrafts.delete(flush); if (!editorDrafts.size) drafts.delete(editor); }
  };
}
