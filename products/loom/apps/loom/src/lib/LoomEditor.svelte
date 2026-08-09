<script lang="ts">
  import { baseKeymap, setBlockType, toggleMark, wrapIn } from 'prosemirror-commands';
  import { history, redo, undo } from 'prosemirror-history';
  import { keymap } from 'prosemirror-keymap';
  import { defaultMarkdownParser, defaultMarkdownSerializer, schema } from 'prosemirror-markdown';
  import type { Node as ProseMirrorNode } from 'prosemirror-model';
  import { EditorState, Selection } from 'prosemirror-state';
  import { EditorView } from 'prosemirror-view';
  import { onDestroy, onMount } from 'svelte';

  export let value = '';
  export let label = 'Manuscript editor';
  export let readonly = false;
  export let autofocus = false;
  export let onChange: (markdown: string) => void = () => {};
  export let onCompositionChange: (active: boolean) => void = () => {};
  export let onSelectionChange: (atDocumentEnd: boolean) => void = () => {};

  let mount: HTMLDivElement;
  let view: EditorView | undefined;
  let lastEmitted = value;
  let projectionTimer: number | undefined;
  let localDocumentChanged = false;
  let composing = false;

  function projectDocument(): void {
    if (!view || composing || !localDocumentChanged) return;
    if (projectionTimer !== undefined) {
      window.clearTimeout(projectionTimer);
      projectionTimer = undefined;
    }
    localDocumentChanged = false;
    lastEmitted = defaultMarkdownSerializer.serialize(view.state.doc);
    onChange(lastEmitted);
  }

  function scheduleProjection(delay = 240): void {
    if (projectionTimer !== undefined) return;
    projectionTimer = window.setTimeout(projectDocument, delay);
  }

  export function flushPending(): boolean {
    if (composing) return false;
    projectDocument();
    return true;
  }

  function parse(markdown: string): ProseMirrorNode {
    return defaultMarkdownParser.parse(markdown);
  }

  function stateFor(markdown: string): EditorState {
    const paragraph = schema.nodes.paragraph;
    const heading = schema.nodes.heading;
    const blockquote = schema.nodes.blockquote;
    const strong = schema.marks.strong;
    const em = schema.marks.em;
    return EditorState.create({
      doc: parse(markdown),
      plugins: [
        history(),
        keymap({
          'Mod-z': undo,
          'Shift-Mod-z': redo,
          'Mod-y': redo,
          'Mod-b': toggleMark(strong),
          'Mod-i': toggleMark(em),
          'Mod-Alt-0': setBlockType(paragraph),
          'Mod-Alt-1': setBlockType(heading, { level: 1 }),
          'Mod-Alt-2': setBlockType(heading, { level: 2 }),
          'Mod->': wrapIn(blockquote)
        }),
        keymap(baseKeymap)
      ]
    });
  }

  function reportSelection(state: EditorState): void {
    const documentEnd = Selection.atEnd(state.doc);
    onSelectionChange(
      state.selection.empty &&
      state.selection.from === documentEnd.from &&
      state.selection.to === documentEnd.to
    );
  }

  onMount(() => {
    view = new EditorView(mount, {
      state: stateFor(value),
      editable: () => !readonly,
      attributes: {
        'aria-label': label,
        class: 'loom-prosemirror',
        role: 'textbox',
        'aria-multiline': 'true',
        spellcheck: 'true'
      },
      dispatchTransaction(transaction) {
        if (!view) return;
        const next = view.state.apply(transaction);
        view.updateState(next);
        reportSelection(next);
        if (transaction.docChanged) {
          localDocumentChanged = true;
          if (!composing) scheduleProjection();
        }
      },
      handleDOMEvents: {
        compositionstart() {
          composing = true;
          onCompositionChange(true);
          return false;
        },
        compositionend() {
          composing = false;
          onCompositionChange(false);
          scheduleProjection(0);
          return false;
        }
      }
    });
    reportSelection(view.state);
    if (autofocus) view.focus();
  });

  $: if (view && value !== lastEmitted && !composing && !localDocumentChanged) {
    lastEmitted = value;
    const next = stateFor(value);
    view.updateState(next);
    reportSelection(next);
  }

  $: if (view) {
    view.setProps({ editable: () => !readonly });
  }

  onDestroy(() => {
    if (projectionTimer !== undefined) window.clearTimeout(projectionTimer);
    if (composing) onCompositionChange(false);
    onSelectionChange(false);
    view?.destroy();
  });
</script>

<div class="editor-mount" bind:this={mount}></div>
