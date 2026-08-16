<script lang="ts">
  import { baseKeymap, setBlockType, toggleMark, wrapIn } from 'prosemirror-commands';
  import { history, redo, undo } from 'prosemirror-history';
  import { keymap } from 'prosemirror-keymap';
  import { defaultMarkdownSerializer, schema } from 'prosemirror-markdown';
  import type { Node as ProseMirrorNode } from 'prosemirror-model';
  import { EditorState, Selection } from 'prosemirror-state';
  import { EditorView } from 'prosemirror-view';
  import { onDestroy, onMount } from 'svelte';
  import { normalizeVisualMarkdownSource, parseVisualMarkdown } from './markdownSafety';
  import {
    clearGhostText,
    createGhostTextPlugin,
    currentGhostTextPlan,
    exactMarkdownByteOffsetAtSelection,
    setGhostText,
    visibleGhostWidgetPresentationKey,
    visualGhostTextIsFaithfulAtSelection
  } from './ghostText';
  import { nextSuggestionWord, type SuggestionAlternative } from './suggestionInteraction';
  import {
    applyVisualFormat,
    visualFormatState,
    type VisualFormatAction,
    type VisualFormatState
  } from './visualFormatting';
  import { visualMarkdownInputRules } from './visualInputRules';

  export let value = '';
  export let label = 'Manuscript editor';
  export let placeholder = 'Start writing…';
  export let readonly = false;
  export let autofocus = false;
  export let ghostText = '';
  export let ghostCandidateId = '';
  export let ghostPresentationKey = '';
  export let ghostAnchorByteOffset: number | null = null;
  export let ghostInsertsOnAccept = false;
  export let ghostAlternatives: readonly SuggestionAlternative[] = [];
  export let ghostHidden = false;
  export let ghostUnconsumeText = '';
  export let surfaceKey = '';
  export let onChange: (markdown: string) => void = () => {};
  export let onCompositionChange: (active: boolean) => void = () => {};
  export let onImmediateDocumentMutation: () => void = () => {};
  export let onGhostAccept: (candidateId: string, presentationKey: string) => boolean = () => false;
  export let onGhostInsert: (
    candidateId: string,
    presentationKey: string,
    text: string
  ) => boolean = () => false;
  export let onGhostCycle: (offset: number) => void = () => {};
  export let onGhostUnconsume: (candidateId: string, presentationKey: string, text: string) => boolean = () => false;
  export let onGhostDismiss: (candidateId: string, presentationKey: string) => void = () => {};
  export let onGhostPresentationRejected: (
    candidateId: string,
    presentationKey: string,
    surfaceKey: string,
    anchorByteOffset: number
  ) => void;
  export let onGhostVisibilityChange: (presentationKey: string) => void = () => {};
  export let onSelectionChange: (markdownByteOffset: number | null) => void = () => {};
  export let onCaretNavigation: () => void = () => {};
  export let onFormatStateChange: (state: VisualFormatState) => void = () => {};

  let mount: HTMLDivElement;
  let scrollViewport: HTMLElement | null = null;
  let view: EditorView | undefined;
  let lastEmitted = normalizeVisualMarkdownSource(value);
  let editorEmpty = lastEmitted.length === 0;
  let projectionTimer: number | undefined;
  let normalizationTimer: number | undefined;
  let localDocumentChanged = false;
  let composing = false;
  let suppressedGhostKey = '';
  let reportedGhostPresentationKey = '';
  let reportedRejectedPresentationIdentity = '';
  let optionHeld = false;
  let visibilityFrame: number | undefined;
  let selectionReportTimer: number | undefined;
  let boundaryCacheDocument: ProseMirrorNode | null = null;
  let boundaryCacheCanonical = '';
  let boundaryCacheFrom = -1;
  let boundaryCacheTo = -1;
  let boundaryCacheValue: number | null = null;

  function clearBoundaryCache(): void {
    boundaryCacheDocument = null;
    boundaryCacheCanonical = '';
    boundaryCacheFrom = -1;
    boundaryCacheTo = -1;
    boundaryCacheValue = null;
  }

  function selectionBoundary(state: EditorState): number | null {
    if (
      boundaryCacheDocument === state.doc &&
      boundaryCacheCanonical === lastEmitted &&
      boundaryCacheFrom === state.selection.from &&
      boundaryCacheTo === state.selection.to
    ) return boundaryCacheValue;
    const boundary = exactMarkdownByteOffsetAtSelection(state, lastEmitted);
    boundaryCacheDocument = state.doc;
    boundaryCacheCanonical = lastEmitted;
    boundaryCacheFrom = state.selection.from;
    boundaryCacheTo = state.selection.to;
    boundaryCacheValue = boundary;
    return boundary;
  }

  function reportGhostVisibility(): void {
    const presentationKey = view ? visibleGhostWidgetPresentationKey(view) : '';
    if (reportedGhostPresentationKey === presentationKey) return;
    reportedGhostPresentationKey = presentationKey;
    onGhostVisibilityChange(presentationKey);
  }

  function scheduleGhostVisibilityReport(): void {
    if (visibilityFrame !== undefined) window.cancelAnimationFrame(visibilityFrame);
    visibilityFrame = window.requestAnimationFrame(() => {
      visibilityFrame = undefined;
      reportGhostVisibility();
    });
  }

  function projectDocument(): void {
    if (!view || composing || !localDocumentChanged) return;
    if (projectionTimer !== undefined) {
      window.clearTimeout(projectionTimer);
      projectionTimer = undefined;
    }
    localDocumentChanged = false;
    lastEmitted = defaultMarkdownSerializer.serialize(view.state.doc);
    clearBoundaryCache();
    onChange(lastEmitted);
    reportSelection(view.state);
  }

  function scheduleProjection(delay = 240): void {
    if (projectionTimer !== undefined) return;
    projectionTimer = window.setTimeout(projectDocument, delay);
  }

  function scheduleExternalNormalization(original: string, normalized: string): void {
    if (original === normalized) return;
    if (normalizationTimer !== undefined) window.clearTimeout(normalizationTimer);
    normalizationTimer = window.setTimeout(() => {
      normalizationTimer = undefined;
      if (view && value === original && lastEmitted === normalized) onChange(normalized);
    }, 0);
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

  export function focusPreservingSelection(): boolean {
    if (!view || readonly) return false;
    view.focus();
    return view.hasFocus();
  }

  export function applyFormatting(action: VisualFormatAction, href = ''): boolean {
    if (!view || readonly || composing) return false;
    const applied = applyVisualFormat(view.state, action, href, (transaction) => view?.dispatch(transaction));
    if (applied) {
      view.focus();
      onFormatStateChange(visualFormatState(view.state));
    }
    return applied;
  }

  export function acceptGhostWord(requireVisible = true): boolean {
    if (!view || readonly || composing || !view.hasFocus()) return false;
    const plan = currentGhostTextPlan(view.state);
    if (
      !plan ||
      (requireVisible && visibleGhostWidgetPresentationKey(view) !== plan.presentationKey) ||
      selectionBoundary(view.state) !== plan.anchorByteOffset
    ) return false;
    const word = nextSuggestionWord(plan.text);
    if (!word || !onGhostInsert(plan.candidateId, plan.presentationKey, word)) return false;
    view.dispatch(view.state.tr.insertText(word));
    return true;
  }

  function parse(markdown: string): ProseMirrorNode {
    return parseVisualMarkdown(markdown);
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
        visualMarkdownInputRules(),
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
          insert: (candidateId, presentationKey, text) =>
            onGhostInsert(candidateId, presentationKey, text),
          unconsume: (candidateId, presentationKey, text) =>
            onGhostUnconsume(candidateId, presentationKey, text),
          cycle: onGhostCycle,
          dismiss: (candidateId, presentationKey) => onGhostDismiss(candidateId, presentationKey),
          visible: (presentationKey, expectedSurfaceKey, anchorByteOffset) =>
            Boolean(view) &&
            expectedSurfaceKey === surfaceKey &&
            anchorByteOffset === ghostAnchorByteOffset &&
            selectionBoundary(view!.state) === anchorByteOffset &&
            visibleGhostWidgetPresentationKey(view!) === presentationKey
        })
      ]
    });
  }

  function editorAttributes(): Record<string, string> {
    return {
      'aria-label': label,
      'aria-placeholder': placeholder,
      'data-placeholder': placeholder,
      class: 'loom-prosemirror',
      role: 'textbox',
      'aria-multiline': 'true',
      spellcheck: 'true'
    };
  }

  function reportSelection(state: EditorState): void {
    onSelectionChange(selectionBoundary(state));
    onFormatStateChange(visualFormatState(state));
  }

  function scheduleSelectionReport(delay = 48): void {
    if (selectionReportTimer !== undefined) window.clearTimeout(selectionReportTimer);
    selectionReportTimer = window.setTimeout(() => {
      selectionReportTimer = undefined;
      if (view && !localDocumentChanged && !composing) reportSelection(view.state);
    }, delay);
  }

  function setOptionHeld(held: boolean): void {
    if (optionHeld === held) return;
    optionHeld = held;
    if (!view) return;
    const plan = currentGhostTextPlan(view.state);
    if (!plan || plan.alternatives.length < 2) return;
    setGhostText(view, { ...plan, active: true, fanVisible: held });
  }

  function handleWindowKeyDown(event: KeyboardEvent): void {
    if (event.key === 'Alt' || (event.altKey && !event.metaKey && !event.ctrlKey)) {
      setOptionHeld(true);
    }
  }

  function handleWindowKeyUp(event: KeyboardEvent): void {
    if (event.key === 'Alt' || !event.altKey) setOptionHeld(false);
  }

  function handleWindowBlur(): void {
    setOptionHeld(false);
  }

  onMount(() => {
    const initialMarkdown = normalizeVisualMarkdownSource(value);
    lastEmitted = initialMarkdown;
    view = new EditorView(mount, {
      state: stateFor(initialMarkdown),
      editable: () => !readonly,
      attributes: editorAttributes(),
      dispatchTransaction(transaction) {
        if (!view) return;
        const next = view.state.apply(transaction);
        view.updateState(next);
        editorEmpty = next.doc.textContent.length === 0;
        if (transaction.docChanged) {
          clearBoundaryCache();
          if (selectionReportTimer !== undefined) {
            window.clearTimeout(selectionReportTimer);
            selectionReportTimer = undefined;
          }
          onSelectionChange(null);
        } else if (transaction.selectionSet) {
          onCaretNavigation();
          onSelectionChange(null);
          scheduleSelectionReport();
        }
        if (transaction.docChanged || transaction.selectionSet) {
          reportGhostVisibility();
        } else {
          scheduleGhostVisibilityReport();
        }
        if (transaction.docChanged) {
          suppressedGhostKey = ghostPresentationKey;
          onImmediateDocumentMutation();
          localDocumentChanged = true;
          if (!composing) scheduleProjection();
        }
      },
      handleDOMEvents: {
        focus() {
          scheduleGhostVisibilityReport();
          return false;
        },
        blur() {
          scheduleGhostVisibilityReport();
          return false;
        },
        compositionstart() {
          composing = true;
          suppressedGhostKey = ghostPresentationKey;
          if (view) clearGhostText(view);
          onCompositionChange(true);
          return false;
        },
        compositionend() {
          composing = false;
          // A cancelled/no-op IME session must not permanently hide an
          // otherwise exact candidate. A real mutation already invalidated
          // its parent identity, and the exact-boundary proof still gates any
          // transient redisplay before that update arrives.
          suppressedGhostKey = '';
          onCompositionChange(false);
          scheduleProjection(0);
          return false;
        }
      }
    });
    reportSelection(view.state);
    scheduleExternalNormalization(value, initialMarkdown);
    scrollViewport = mount.closest<HTMLElement>('.editor-pane');
    scheduleGhostVisibilityReport();
    window.addEventListener('resize', reportGhostVisibility);
    window.addEventListener('keydown', handleWindowKeyDown, true);
    window.addEventListener('keyup', handleWindowKeyUp, true);
    window.addEventListener('blur', handleWindowBlur);
    scrollViewport?.addEventListener('scroll', reportGhostVisibility, { passive: true });
    if (autofocus) view.focus();
  });

  $: if (view && value !== lastEmitted && !composing && !localDocumentChanged) {
    const normalized = normalizeVisualMarkdownSource(value);
    lastEmitted = normalized;
    const next = stateFor(normalized);
    view.updateState(next);
    editorEmpty = next.doc.textContent.length === 0;
    clearBoundaryCache();
    reportSelection(next);
    scheduleGhostVisibilityReport();
    scheduleExternalNormalization(value, normalized);
  }

  $: if (view) {
    view.setProps({ editable: () => !readonly, attributes: editorAttributes() });
    const anchorByteOffset = ghostAnchorByteOffset;
    const exactAnchor = anchorByteOffset !== null &&
      selectionBoundary(view.state) === anchorByteOffset;
    const faithful = exactAnchor && visualGhostTextIsFaithfulAtSelection(
        view.state,
        lastEmitted,
        anchorByteOffset!,
        ghostText
      );
    const rejectionIdentity = anchorByteOffset === null
      ? ''
      : `${ghostPresentationKey}\u0000${surfaceKey}\u0000${anchorByteOffset}`;
    if (
      !readonly &&
      !composing &&
      ghostCandidateId &&
      ghostPresentationKey &&
      surfaceKey &&
      exactAnchor &&
      !faithful &&
      reportedRejectedPresentationIdentity !== rejectionIdentity
    ) {
      reportedRejectedPresentationIdentity = rejectionIdentity;
      onGhostPresentationRejected(
        ghostCandidateId,
        ghostPresentationKey,
        surfaceKey,
        anchorByteOffset!
      );
    }
    const presentation = ghostPresentationKey &&
      surfaceKey &&
      faithful &&
      ghostPresentationKey !== suppressedGhostKey ? {
      active: !readonly && !composing,
      candidateId: ghostCandidateId,
      presentationKey: ghostPresentationKey,
      surfaceKey,
      anchorByteOffset,
      text: ghostText,
      insertsOnAccept: ghostInsertsOnAccept,
      alternatives: ghostAlternatives,
      hidden: ghostHidden,
      unconsumeText: ghostUnconsumeText,
      fanVisible: optionHeld
    } : null;
    setGhostText(view, presentation);
    scheduleGhostVisibilityReport();
  }

  onDestroy(() => {
    if (projectionTimer !== undefined) window.clearTimeout(projectionTimer);
    if (normalizationTimer !== undefined) window.clearTimeout(normalizationTimer);
    if (visibilityFrame !== undefined) window.cancelAnimationFrame(visibilityFrame);
    if (selectionReportTimer !== undefined) window.clearTimeout(selectionReportTimer);
    if (composing) onCompositionChange(false);
    onSelectionChange(null);
    if (reportedGhostPresentationKey) onGhostVisibilityChange('');
    window.removeEventListener('resize', reportGhostVisibility);
    window.removeEventListener('keydown', handleWindowKeyDown, true);
    window.removeEventListener('keyup', handleWindowKeyUp, true);
    window.removeEventListener('blur', handleWindowBlur);
    scrollViewport?.removeEventListener('scroll', reportGhostVisibility);
    view?.destroy();
  });
</script>

<div class="loom-editor-shell">
  {#if editorEmpty}
    <div class="loom-editor-placeholder" aria-hidden="true">{placeholder}</div>
  {/if}
  <div class="editor-mount" bind:this={mount}></div>
</div>
