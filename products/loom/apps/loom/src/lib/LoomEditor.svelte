<script lang="ts">
  import { baseKeymap, setBlockType, toggleMark, wrapIn } from 'prosemirror-commands';
  import { history, redo, undo } from 'prosemirror-history';
  import { keymap } from 'prosemirror-keymap';
  import { defaultMarkdownParser, defaultMarkdownSerializer, schema } from 'prosemirror-markdown';
  import type { Node as ProseMirrorNode } from 'prosemirror-model';
  import { EditorState, Selection } from 'prosemirror-state';
  import { EditorView } from 'prosemirror-view';
  import { onDestroy, onMount } from 'svelte';
  import {
    clearGhostText,
    createGhostTextPlugin,
    currentGhostTextPlan,
    planGhostOverlayGeometry,
    setGhostText
  } from './ghostText';

  export let value = '';
  export let label = 'Manuscript editor';
  export let readonly = false;
  export let autofocus = false;
  export let ghostText = '';
  export let ghostCandidateId = '';
  export let ghostPresentationKey = '';
  export let onChange: (markdown: string) => void = () => {};
  export let onCompositionChange: (active: boolean) => void = () => {};
  export let onImmediateDocumentMutation: () => void = () => {};
  export let onGhostAccept: (candidateId: string, presentationKey: string) => void = () => {};
  export let onGhostDismiss: (candidateId: string, presentationKey: string) => void = () => {};
  export let onGhostVisibilityChange: (presentationKey: string) => void = () => {};
  export let onSelectionChange: (atDocumentEnd: boolean) => void = () => {};

  let shell: HTMLDivElement;
  let mount: HTMLDivElement;
  let view: EditorView | undefined;
  let lastEmitted = value;
  let projectionTimer: number | undefined;
  let localDocumentChanged = false;
  let composing = false;
  let suppressedGhostKey = '';
  let reportedGhostPresentationKey = '';
  let ghostOverlay: {
    presentationKey: string;
    text: string;
    left: number;
    top: number;
    maxWidth: number;
  } | null = null;

  function syncGhostOverlay(): string {
    const plan = view ? currentGhostTextPlan(view.state) : null;
    if (!view || !plan || !shell) {
      ghostOverlay = null;
      return '';
    }
    try {
      const caret = view.coordsAtPos(plan.position, 1);
      const shellBounds = shell.getBoundingClientRect();
      const editorBounds = view.dom.getBoundingClientRect();
      const editorStyle = window.getComputedStyle(view.dom);
      const paddingLeft = Number.parseFloat(editorStyle.paddingLeft) || 0;
      const paddingRight = Number.parseFloat(editorStyle.paddingRight) || 0;
      const geometry = planGhostOverlayGeometry({
        caret,
        shell: shellBounds,
        text: {
          left: editorBounds.left + paddingLeft,
          right: editorBounds.right - paddingRight
        }
      });
      if (!geometry) {
        ghostOverlay = null;
        return '';
      }
      ghostOverlay = {
        presentationKey: plan.presentationKey,
        text: plan.text,
        ...geometry
      };
      return plan.presentationKey;
    } catch {
      ghostOverlay = null;
      return '';
    }
  }

  function reportGhostVisibility(): void {
    const presentationKey = syncGhostOverlay();
    if (reportedGhostPresentationKey === presentationKey) return;
    reportedGhostPresentationKey = presentationKey;
    onGhostVisibilityChange(presentationKey);
  }

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

  export function focusAtDocumentEnd(): boolean {
    if (!view || readonly) return false;
    const end = Selection.atEnd(view.state.doc);
    if (!view.state.selection.eq(end)) {
      view.dispatch(view.state.tr.setSelection(end));
    }
    view.focus();
    return view.hasFocus();
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
        keymap(baseKeymap),
        createGhostTextPlugin({
          accept: (candidateId, presentationKey) => onGhostAccept(candidateId, presentationKey),
          dismiss: (candidateId, presentationKey) => onGhostDismiss(candidateId, presentationKey)
        })
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
        reportGhostVisibility();
        if (transaction.docChanged) {
          suppressedGhostKey = ghostPresentationKey;
          onImmediateDocumentMutation();
          localDocumentChanged = true;
          if (!composing) scheduleProjection();
        }
      },
      handleDOMEvents: {
        compositionstart() {
          composing = true;
          suppressedGhostKey = ghostPresentationKey;
          if (view) clearGhostText(view);
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
    reportGhostVisibility();
    window.addEventListener('resize', reportGhostVisibility);
    if (autofocus) view.focus();
  });

  $: if (view && value !== lastEmitted && !composing && !localDocumentChanged) {
    lastEmitted = value;
    const next = stateFor(value);
    view.updateState(next);
    reportSelection(next);
    reportGhostVisibility();
  }

  $: if (view) {
    view.setProps({ editable: () => !readonly });
    const presentation = ghostPresentationKey && ghostPresentationKey !== suppressedGhostKey ? {
      active: !readonly && !composing,
      candidateId: ghostCandidateId,
      presentationKey: ghostPresentationKey,
      text: ghostText
    } : null;
    setGhostText(view, presentation);
    reportGhostVisibility();
  }

  onDestroy(() => {
    if (projectionTimer !== undefined) window.clearTimeout(projectionTimer);
    if (composing) onCompositionChange(false);
    onSelectionChange(false);
    if (reportedGhostPresentationKey) onGhostVisibilityChange('');
    window.removeEventListener('resize', reportGhostVisibility);
    view?.destroy();
  });
</script>

<div class="loom-editor-shell" bind:this={shell}>
  <div class="editor-mount" bind:this={mount}></div>
  {#if ghostOverlay}
    <span
      class="loom-visual-ghost"
      aria-hidden="true"
      style={`left:${ghostOverlay.left}px;top:${ghostOverlay.top}px;max-width:${ghostOverlay.maxWidth}px`}
    >{ghostOverlay.text}</span>
  {/if}
</div>
