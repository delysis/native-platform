<script lang="ts">
  import { baseKeymap, setBlockType, toggleMark, wrapIn } from 'prosemirror-commands';
  import { history, redo, undo } from 'prosemirror-history';
  import { keymap } from 'prosemirror-keymap';
  import { schema } from 'prosemirror-markdown';
  import type { Node as ProseMirrorNode } from 'prosemirror-model';
  import { EditorState, Selection } from 'prosemirror-state';
  import { EditorView } from 'prosemirror-view';
  import { onDestroy, onMount } from 'svelte';
  import {
    normalizeVisualMarkdownSource,
    parseVisualMarkdown,
    serializeVisualMarkdown
  } from './markdownSafety';
  import {
    clearGhostText,
    createGhostTextPlugin,
    currentGhostTextPlan,
    setGhostFanPinned,
    setGhostText,
    visualCaretBoundaryProof,
    visibleGhostWidgetPresentationKey,
    type GhostTextPresentation,
    type VisualCaretBoundaryFailure,
    visualGhostTextIsFaithfulAtSelection
  } from './ghostText';
  import {
    CLOSED_COMPLETION_LENS,
    reduceCompletionLens,
    type CompletionLensState
  } from './completionLens';
  import {
    nextVisualSuggestionWord,
    type CompletionInsertionAction,
    type SuggestionAlternative
  } from './suggestionInteraction';
  import {
    unavailableVisualSelectionWitness,
    type VisualCompletionAccessibilityWitness,
    type VisualSelectionAccessibilityWitness
  } from './completionAccessibility';
  import { allocateCompletionPopupDomIds } from './completionPopup';
  import {
    applyVisualFormat,
    visualFormatState,
    type VisualFormatAction,
    type VisualFormatState
  } from './visualFormatting';
  import { visualMarkdownInputRules } from './visualInputRules';
  import { visualListKeymap } from './visualListEditing';
  import {
    STALE_IMAGE_ATTACHMENT_ERROR,
    UNREADABLE_TRANSFER_IMAGE_ERROR,
    UNVERIFIED_DROP_FILE_ERROR,
    imageAttachmentErrorMessage,
    imageFilesFromTransfer,
    transferContainsEphemeralImage,
    transferMayContainImageFile
  } from './attachments';

  const formattingSelectionRestoreMeta = 'loomFormattingSelectionRestore';
  const completionPopupDomIds = allocateCompletionPopupDomIds('visual');

  interface GhostPresentationSnapshot {
    label: string;
    readonly: boolean;
    composing: boolean;
    text: string;
    candidateId: string;
    presentationKey: string;
    anchorByteOffset: number | null;
    insertsOnAccept: boolean;
    alternatives: readonly SuggestionAlternative[];
    hidden: boolean;
    unconsumeText: string;
    surfaceKey: string;
    suppressedKey: string;
    optionHeld: boolean;
    lensPinned: boolean;
  }

  export let value = '';
  export let label = 'Manuscript editor';
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
  /**
   * Claim browser image paste/drop events for the manuscript attachment flow.
   * Embedders with a different drop authority (such as the completion-context
   * pane's native path importer) must opt out so this editor does not swallow
   * an event it cannot commit.
   */
  export let acceptImageAttachments = true;
  export let onImageAttachments: (files: readonly File[]) => Promise<readonly string[]> =
    async () => [];
  export let onImageAttachmentsCommitted: (count: number) => void = () => {};
  export let onImageAttachmentError: (message: string) => void = () => {};
  export let resolveImageAssetUrl: (markdownPath: string) => string | null = () => null;
  export let onChange: (markdown: string) => void = () => {};
  export let onCompositionChange: (active: boolean) => void = () => {};
  export let onImmediateDocumentMutation: () => void = () => {};
  export let onGhostAccept: (candidateId: string, presentationKey: string) => boolean = () => false;
  export let onGhostInsert: (
    candidateId: string,
    presentationKey: string,
    text: string,
    action: CompletionInsertionAction
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
  export let onSelectionChange: (
    markdownByteOffset: number | null,
    failure: VisualCaretBoundaryFailure | 'selection_settling' | null,
    diagnostic: string | null
  ) => void = () => {};
  export let onCaretNavigation: () => void = () => {};
  export let onFormatStateChange: (state: VisualFormatState) => void = () => {};
  export let onSelectionAccessibilityChange: (
    witness: VisualSelectionAccessibilityWitness
  ) => void = () => {};
  export let onCompletionAccessibilityChange: (
    witness: VisualCompletionAccessibilityWitness
  ) => void = () => {};

  let mount: HTMLDivElement;
  let scrollViewport: HTMLElement | null = null;
  let view: EditorView | undefined;
  let lastEmitted = normalizeVisualMarkdownSource(value);
  let projectionTimer: number | undefined;
  let normalizationTimer: number | undefined;
  let localDocumentChanged = false;
  let completionMutationAuthorized = false;
  let composing = false;
  let suppressedGhostKey = '';
  let reportedGhostPresentationKey = '';
  let reportedRejectedPresentationIdentity = '';
  let optionHeld = false;
  let completionLens: CompletionLensState = CLOSED_COMPLETION_LENS;
  let visibilityFrame: number | undefined;
  let ghostSynchronizationFrame: number | undefined;
  let ghostSynchronizationRequiresRender = false;
  let selectionReportTimer: number | undefined;
  let boundaryCacheDocument: ProseMirrorNode | null = null;
  let boundaryCacheCanonical = '';
  let boundaryCacheFrom = -1;
  let boundaryCacheTo = -1;
  let boundaryCacheValue: number | null = null;
  let boundaryCacheFailure: VisualCaretBoundaryFailure | null = null;
  let boundaryCacheDiagnostic: string | null = null;
  let formattingSelectionDocument: ProseMirrorNode | null = null;
  let formattingSelection: Selection | null = null;
  let formattingRestoreFrame: number | undefined;
  let formattingRestoreTimer: number | undefined;
  let formattingRestoreIntentEpoch = 0;
  let formattingRestoreDocument: ProseMirrorNode | null = null;
  let formattingRestoreSelection: Selection | null = null;
  let formattingRestoreDeadline = 0;
  let reportedCompletionAccessibilityIdentity = '';
  let reportedSelectionAccessibilityIdentity = '';
  let selectionAccessibilityEpoch = 0;

  function reportCompletionAccessibility(): void {
    const plan = view ? currentGhostTextPlan(view.state) : null;
    const witness: VisualCompletionAccessibilityWitness = {
      available: Boolean(view),
      optionHeld,
      fanVisible: Boolean(plan?.fanVisible),
      lensPinned: completionLens.pinned,
      inlineHidden: Boolean(plan?.hidden),
      selectedCandidateId: plan?.candidateId ?? '',
      selectedPresentationKey: plan?.presentationKey ?? '',
      alternativeCandidateIds: plan?.alternatives.map((item) => item.candidateId) ?? [],
      alternativePresentationKeys: plan?.alternatives.map((item) => item.presentationKey) ?? [],
      alternativeRunIds: plan?.alternatives.map((item) => item.runId ?? '') ?? []
    };
    const identity = JSON.stringify(witness);
    if (identity === reportedCompletionAccessibilityIdentity) return;
    reportedCompletionAccessibilityIdentity = identity;
    onCompletionAccessibilityChange(witness);
  }

  function clearBoundaryCache(): void {
    boundaryCacheDocument = null;
    boundaryCacheCanonical = '';
    boundaryCacheFrom = -1;
    boundaryCacheTo = -1;
    boundaryCacheValue = null;
    boundaryCacheFailure = null;
    boundaryCacheDiagnostic = null;
  }

  function selectionBoundaryProof(state: EditorState): {
    byteOffset: number | null;
    failure: VisualCaretBoundaryFailure | null;
    diagnostic: string | null;
  } {
    if (
      boundaryCacheDocument === state.doc &&
      boundaryCacheCanonical === lastEmitted &&
      boundaryCacheFrom === state.selection.from &&
      boundaryCacheTo === state.selection.to
    ) return {
      byteOffset: boundaryCacheValue,
      failure: boundaryCacheFailure,
      diagnostic: boundaryCacheDiagnostic
    };
    const proof = visualCaretBoundaryProof(state, lastEmitted);
    boundaryCacheDocument = state.doc;
    boundaryCacheCanonical = lastEmitted;
    boundaryCacheFrom = state.selection.from;
    boundaryCacheTo = state.selection.to;
    boundaryCacheValue = proof.byteOffset;
    boundaryCacheFailure = proof.failure;
    boundaryCacheDiagnostic = proof.diagnostic;
    return proof;
  }

  function selectionBoundary(state: EditorState): number | null {
    return selectionBoundaryProof(state).byteOffset;
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

  function currentGhostPresentationSnapshot(): GhostPresentationSnapshot {
    return {
      label,
      readonly,
      composing,
      text: ghostText,
      candidateId: ghostCandidateId,
      presentationKey: ghostPresentationKey,
      anchorByteOffset: ghostAnchorByteOffset,
      insertsOnAccept: ghostInsertsOnAccept,
      alternatives: ghostAlternatives,
      hidden: ghostHidden,
      unconsumeText: ghostUnconsumeText,
      surfaceKey,
      suppressedKey: suppressedGhostKey,
      optionHeld,
      lensPinned: completionLens.pinned
    };
  }

  function synchronizeGhostPresentation(
    snapshot: GhostPresentationSnapshot,
    forceRender = false
  ): void {
    const editorView = view;
    if (!editorView || editorView.isDestroyed) return;
    editorView.setProps({
      editable: () => !snapshot.readonly,
      attributes: editorAttributes(snapshot.label)
    });
    const anchorByteOffset = snapshot.anchorByteOffset;
    const provenAnchorByteOffset = anchorByteOffset ?? -1;
    const exactAnchor = anchorByteOffset !== null &&
      selectionBoundary(editorView.state) === anchorByteOffset;
    const rollbackOnly = snapshot.text === '' && snapshot.unconsumeText !== '';
    const faithful = exactAnchor && (
      rollbackOnly || visualGhostTextIsFaithfulAtSelection(
        editorView.state,
        lastEmitted,
        provenAnchorByteOffset,
        snapshot.text
      )
    );
    const rejectionIdentity = anchorByteOffset === null
      ? ''
      : `${snapshot.presentationKey}\u0000${snapshot.surfaceKey}\u0000${anchorByteOffset}`;
    if (
      !snapshot.readonly &&
      !snapshot.composing &&
      snapshot.candidateId &&
      snapshot.presentationKey &&
      snapshot.surfaceKey &&
      exactAnchor &&
      !faithful &&
      reportedRejectedPresentationIdentity !== rejectionIdentity
    ) {
      reportedRejectedPresentationIdentity = rejectionIdentity;
      onGhostPresentationRejected(
        snapshot.candidateId,
        snapshot.presentationKey,
        snapshot.surfaceKey,
        provenAnchorByteOffset
      );
    }
    const presentation: GhostTextPresentation | null = snapshot.presentationKey &&
      snapshot.surfaceKey &&
      faithful &&
      snapshot.presentationKey !== snapshot.suppressedKey ? {
      active: !snapshot.readonly && !snapshot.composing,
      candidateId: snapshot.candidateId,
      presentationKey: snapshot.presentationKey,
      surfaceKey: snapshot.surfaceKey,
      anchorByteOffset: provenAnchorByteOffset,
      text: snapshot.text,
      insertsOnAccept: snapshot.insertsOnAccept,
      alternatives: snapshot.alternatives,
      hidden: snapshot.hidden || rollbackOnly,
      unconsumeText: snapshot.unconsumeText,
      // Once a word is consumed the session is locked to one candidate. Do
      // not hide its cached remainder behind a now-empty alternatives fan
      // while Option is still held.
      fanVisible: snapshot.optionHeld && snapshot.alternatives.length > 1,
      fanPinned: snapshot.lensPinned && snapshot.alternatives.length > 1
    } : null;
    setGhostText(editorView, presentation, forceRender);
    reportCompletionAccessibility();
    scheduleGhostVisibilityReport();
  }

  function scheduleGhostPresentationSynchronization(forceRender = false): void {
    ghostSynchronizationRequiresRender ||= forceRender;
    if (ghostSynchronizationFrame !== undefined) {
      window.cancelAnimationFrame(ghostSynchronizationFrame);
    }
    ghostSynchronizationFrame = window.requestAnimationFrame(() => {
      ghostSynchronizationFrame = undefined;
      const requiresRender = ghostSynchronizationRequiresRender;
      ghostSynchronizationRequiresRender = false;
      synchronizeGhostPresentation(currentGhostPresentationSnapshot(), requiresRender);
    });
  }

  /**
   * Rebuild an unchanged ProseMirror decoration when the native window wakes.
   * macOS can hide and reactivate a WKWebView without changing the browser
   * document's focus or visibility state, so App's native focus event is the
   * authoritative lifecycle edge for this repair.
   */
  export function refreshGhostPresentation(): boolean {
    if (!view || view.isDestroyed) return false;
    if (ghostSynchronizationFrame !== undefined) {
      window.cancelAnimationFrame(ghostSynchronizationFrame);
      ghostSynchronizationFrame = undefined;
    }
    ghostSynchronizationRequiresRender = false;
    synchronizeGhostPresentation(currentGhostPresentationSnapshot(), true);
    return true;
  }

  function projectDocument(): void {
    if (!view || composing || !localDocumentChanged) return;
    if (projectionTimer !== undefined) {
      window.clearTimeout(projectionTimer);
      projectionTimer = undefined;
    }
    localDocumentChanged = false;
    lastEmitted = serializeVisualMarkdown(view.state.doc);
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
    const currentDocumentLease = Boolean(
      formattingSelection && formattingSelectionDocument === view.state.doc
    );
    if (!view.hasFocus() && !currentDocumentLease) return false;
    if (
      currentDocumentLease &&
      formattingSelection &&
      !view.state.selection.eq(formattingSelection)
    ) {
      view.dispatch(view.state.tr.setSelection(formattingSelection));
    }
    view.focus();
    return view.hasFocus();
  }

  export function focusCurrentSelection(): boolean {
    if (!view || readonly) return false;
    view.focus();
    reportSelection(view.state);
    return view.hasFocus();
  }

  export function insertAttachmentMarkdown(
    markdown: string,
    clientX?: number,
    clientY?: number
  ): boolean {
    if (!view || readonly || composing || !markdown.trim()) return false;
    const position = clientX !== undefined && clientY !== undefined
      ? view.posAtCoords({ left: clientX, top: clientY })
      : null;
    const selection = position
      ? Selection.near(view.state.doc.resolve(position.pos))
      : view.state.selection;
    const attachmentDocument = parse(markdown);
    view.dispatch(view.state.tr.replaceRange(
      selection.from,
      selection.to,
      attachmentDocument.slice(0, attachmentDocument.content.size)
    ));
    projectDocument();
    view.focus();
    return true;
  }

  export function insertTextAtSelection(text: string): boolean {
    if (!view || readonly || composing || !text) return false;
    view.dispatch(view.state.tr.insertText(
      text,
      view.state.selection.from,
      view.state.selection.to
    ));
    projectDocument();
    view.focus();
    return true;
  }

  export function captureTextInsertionAnchor(): {
    surfaceKey: string;
    markdown: string;
    from: number;
    to: number;
  } | null {
    if (!view || readonly || composing) return null;
    return {
      surfaceKey,
      markdown: lastEmitted,
      from: view.state.selection.from,
      to: view.state.selection.to
    };
  }

  export function insertTextAtAnchor(
    anchor: { surfaceKey: string; markdown: string; from: number; to: number },
    text: string
  ): boolean {
    if (
      !view ||
      readonly ||
      composing ||
      !text ||
      anchor.surfaceKey !== surfaceKey ||
      anchor.markdown !== lastEmitted ||
      anchor.from < 0 ||
      anchor.to < anchor.from ||
      anchor.to > view.state.doc.content.size
    ) return false;
    view.dispatch(view.state.tr.insertText(text, anchor.from, anchor.to));
    projectDocument();
    view.focus();
    return true;
  }

  /** Reassert the current immutable selection without treating it as navigation. */
  export function reconcileCurrentSelection(): boolean {
    if (!view || view.isDestroyed) return false;
    view.dispatch(view.state.tr
      .setSelection(view.state.selection)
      .setMeta('addToHistory', false));
    return true;
  }

  export function captureFormattingSelection(
    focusTransitionFrom: EventTarget | null = null
  ): boolean {
    if (!view || readonly || composing) return false;
    const currentDocumentLease = Boolean(
      formattingSelection && formattingSelectionDocument === view.state.doc
    );
    const exactEditorFocusHandoff = focusTransitionFrom instanceof Node && (
      focusTransitionFrom === view.dom || view.dom.contains(focusTransitionFrom)
    );
    // A palette control may retain a lease after it takes focus, but it must
    // not manufacture one from an arbitrary stale editor selection. Keyboard
    // and macOS Accessibility activation can focus the Aa button before its
    // click handler runs, so admit only the browser-proven direct focus handoff
    // from this exact editor DOM.
    if (!view.hasFocus() && !exactEditorFocusHandoff) return currentDocumentLease;
    formattingSelectionDocument = view.state.doc;
    formattingSelection = view.state.selection;
    return true;
  }

  function cancelFormattingSelectionRestore(): void {
    formattingRestoreIntentEpoch += 1;
    formattingRestoreDocument = null;
    formattingRestoreSelection = null;
    formattingRestoreDeadline = 0;
    if (formattingRestoreFrame !== undefined) {
      window.cancelAnimationFrame(formattingRestoreFrame);
      formattingRestoreFrame = undefined;
    }
    if (formattingRestoreTimer !== undefined) {
      window.clearTimeout(formattingRestoreTimer);
      formattingRestoreTimer = undefined;
    }
  }

  function restoreProtectedFormattingSelection(intentEpoch: number): boolean {
    if (
      formattingRestoreIntentEpoch !== intentEpoch ||
      !view ||
      readonly ||
      composing ||
      performance.now() > formattingRestoreDeadline ||
      view.state.doc !== formattingRestoreDocument ||
      !formattingRestoreSelection
    ) return false;
    // Reassert even when ProseMirror's immutable state already matches.
    // WebKit can reconcile its DOM/Accessibility selection independently
    // after a contenteditable mutation.
    view.dispatch(view.state.tr
      .setSelection(formattingRestoreSelection)
      .setMeta(formattingSelectionRestoreMeta, true));
    view.focus();
    formattingSelectionDocument = view.state.doc;
    formattingSelection = view.state.selection;
    return true;
  }

  function scheduleFormattingSelectionRestore(intentEpoch: number, armLateRepair = false): void {
    if (
      formattingRestoreIntentEpoch !== intentEpoch ||
      formattingRestoreFrame !== undefined
    ) return;
    formattingRestoreFrame = window.requestAnimationFrame(() => {
      if (formattingRestoreIntentEpoch !== intentEpoch) return;
      // Give WebKit one render turn to publish any late contenteditable
      // selection reconciliation before installing the authoritative state.
      formattingRestoreFrame = window.requestAnimationFrame(() => {
        formattingRestoreFrame = undefined;
        if (!restoreProtectedFormattingSelection(intentEpoch) || !armLateRepair) return;
        formattingRestoreTimer = window.setTimeout(() => {
          formattingRestoreTimer = undefined;
          restoreProtectedFormattingSelection(intentEpoch);
        }, 900);
      });
    });
  }

  export function clearFormattingSelection(): void {
    cancelFormattingSelectionRestore();
    formattingSelectionDocument = null;
    formattingSelection = null;
  }

  export function applyFormatting(action: VisualFormatAction, href = ''): boolean {
    if (!view || readonly || composing) return false;
    const currentDocumentLease = Boolean(
      formattingSelection && formattingSelectionDocument === view.state.doc
    );
    if (!view.hasFocus() && !currentDocumentLease) return false;
    if (!focusPreservingSelection()) return false;
    const applied = applyVisualFormat(view.state, action, href, (transaction) => view?.dispatch(transaction));
    if (applied) {
      const formattedView = view;
      const formattedDocument = formattedView.state.doc;
      const formattedSelection = formattedView.state.selection;
      cancelFormattingSelectionRestore();
      formattingSelectionDocument = formattedDocument;
      formattingSelection = formattedSelection;
      formattingRestoreDocument = formattedDocument;
      formattingRestoreSelection = formattedSelection;
      // Keep the immutable command result authoritative through a bounded
      // WebKit/AX reconciliation window. An editor key or pointer intent
      // cancels this lease before its selection transaction is admitted.
      formattingRestoreDeadline = performance.now() + 3_000;
      formattedView.focus();
      onFormatStateChange(visualFormatState(formattedView.state));
      const restoreIntentEpoch = formattingRestoreIntentEpoch;
      scheduleFormattingSelectionRestore(restoreIntentEpoch, true);
    }
    return applied;
  }

  export function formattingDiagnostic(): string {
    if (!view) return 'editor_unavailable';
    const selection = view.state.selection;
    const ancestors: string[] = [];
    for (let depth = 0; depth <= selection.$from.depth; depth += 1) {
      ancestors.push(selection.$from.node(depth).type.name);
    }
    return `${selection.constructor.name}:${selection.from}:${selection.to}:${ancestors.join('>')}`;
  }

  export function acceptGhostWord(requireVisible = true): boolean {
    if (!view || readonly || composing || !view.hasFocus()) return false;
    const plan = currentGhostTextPlan(view.state);
    if (
      !plan ||
      (requireVisible && visibleGhostWidgetPresentationKey(view) !== plan.presentationKey) ||
      selectionBoundary(view.state) !== plan.anchorByteOffset
    ) return false;
    const word = nextVisualSuggestionWord(plan.text);
    if (!word || !authorizeCompletionInsertion(
      plan.candidateId,
      plan.presentationKey,
      word,
      'shuttle_word'
    )) return false;
    view.dispatch(view.state.tr.insertText(word));
    return true;
  }

  function authorizeCompletionInsertion(
    candidateId: string,
    presentationKey: string,
    text: string,
    action: CompletionInsertionAction
  ): boolean {
    const authorized = onGhostInsert(candidateId, presentationKey, text, action);
    if (authorized) completionMutationAuthorized = true;
    return authorized;
  }

  function authorizeCompletionReversal(
    candidateId: string,
    presentationKey: string,
    text: string
  ): boolean {
    const authorized = onGhostUnconsume(candidateId, presentationKey, text);
    if (authorized) completionMutationAuthorized = true;
    return authorized;
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
        // An exact visible completion reserves Tab for the following ghost
        // plugin. Otherwise list structure gets first refusal before Loom's
        // ordinary literal-tab fallback.
        visualListKeymap((state, editorView) => {
          const plan = currentGhostTextPlan(state);
          return Boolean(
            plan &&
            editorView &&
            visibleGhostWidgetPresentationKey(editorView) === plan.presentationKey
          );
        }),
        createGhostTextPlugin({
          accept: (candidateId, presentationKey) => onGhostAccept(candidateId, presentationKey),
          insert: authorizeCompletionInsertion,
          unconsume: authorizeCompletionReversal,
          cycle: onGhostCycle,
          modifier: setOptionHeld,
          pin: setLensPinned,
          dismiss: (candidateId, presentationKey) => onGhostDismiss(candidateId, presentationKey),
          visible: (presentationKey, expectedSurfaceKey, anchorByteOffset) =>
            Boolean(view) &&
            expectedSurfaceKey === surfaceKey &&
            anchorByteOffset === ghostAnchorByteOffset &&
            selectionBoundary(view!.state) === anchorByteOffset &&
            visibleGhostWidgetPresentationKey(view!) === presentationKey
        }, completionPopupDomIds),
        keymap(baseKeymap)
      ]
    });
  }

  function editorAttributes(currentLabel = label): Record<string, string> {
    const attributes: Record<string, string> = {
      'aria-label': currentLabel,
      class: 'loom-prosemirror',
      role: 'textbox',
      'aria-multiline': 'true',
      spellcheck: 'true'
    };
    const plan = view ? currentGhostTextPlan(view.state) : null;
    const selectedIndex = plan?.fanVisible
      ? plan.alternatives.findIndex(
          (alternative) => alternative.presentationKey === plan.presentationKey
        )
      : -1;
    if (view && plan?.fanVisible && plan.alternatives.length > 1 && selectedIndex >= 0) {
      const fan = view.dom.ownerDocument.getElementById(completionPopupDomIds.listboxId);
      const activeOptionId = completionPopupDomIds.optionId(selectedIndex);
      const activeOption = view.dom.ownerDocument.getElementById(activeOptionId);
      if (fan && activeOption && view.dom.contains(fan) && fan.contains(activeOption)) {
        attributes['aria-controls'] = completionPopupDomIds.listboxId;
        attributes['aria-activedescendant'] = activeOptionId;
      }
    }
    return attributes;
  }

  function imageNodeView(node: ProseMirrorNode): { dom: HTMLImageElement } {
    const dom = document.createElement('img');
    const markdownPath = typeof node.attrs.src === 'string' ? node.attrs.src : '';
    const resolved = resolveImageAssetUrl(markdownPath);
    if (resolved) dom.src = resolved;
    if (typeof node.attrs.alt === 'string') dom.alt = node.attrs.alt;
    if (typeof node.attrs.title === 'string' && node.attrs.title) dom.title = node.attrs.title;
    return { dom };
  }

  function attachmentSelection(event: DragEvent | ClipboardEvent): Selection | null {
    if (!view) return null;
    if (event instanceof DragEvent) {
      const position = view.posAtCoords({ left: event.clientX, top: event.clientY });
      if (position) return Selection.near(view.state.doc.resolve(position.pos));
    }
    return view.state.selection;
  }

  function handleImageTransfer(event: DragEvent | ClipboardEvent): boolean {
    const transfer = event instanceof ClipboardEvent ? event.clipboardData : event.dataTransfer;
    const files = imageFilesFromTransfer(transfer);
    const ephemeralImage = transferContainsEphemeralImage(transfer);
    const claimedFileDrop = event instanceof DragEvent && transferMayContainImageFile(transfer);
    // Returning true tells ProseMirror not to run its own drop/paste fallback;
    // deliberately leave the DOM event untouched for the embedding pane.
    if (!acceptImageAttachments) {
      return files.length > 0 || ephemeralImage || claimedFileDrop;
    }
    if (files.length === 0 && !ephemeralImage && !claimedFileDrop) return false;
    event.preventDefault();
    event.stopPropagation();
    if (files.length === 0) {
      onImageAttachmentError(
        claimedFileDrop && !ephemeralImage
          ? UNVERIFIED_DROP_FILE_ERROR
          : UNREADABLE_TRANSFER_IMAGE_ERROR
      );
      return true;
    }
    if (!view || readonly || composing) {
      onImageAttachmentError('Images cannot be attached while this editor is unavailable.');
      return true;
    }

    const capturedView = view;
    const capturedDocument = capturedView.state.doc;
    const capturedMarkdown = lastEmitted;
    const capturedSurfaceKey = surfaceKey;
    const capturedSelection = attachmentSelection(event);
    if (!capturedSelection) return true;
    void onImageAttachments(files).then((snippets) => {
      const committedSnippets = snippets.filter((snippet) => snippet.length > 0);
      if (
        !view ||
        view !== capturedView ||
        capturedView.isDestroyed ||
        readonly ||
        composing ||
        surfaceKey !== capturedSurfaceKey ||
        lastEmitted !== capturedMarkdown ||
        !capturedView.state.doc.eq(capturedDocument)
      ) {
        if (committedSnippets.length > 0) {
          onImageAttachmentError(STALE_IMAGE_ATTACHMENT_ERROR);
        }
        return;
      }
      const markdown = committedSnippets.join('\n\n');
      if (!markdown) return;
      const attachmentDocument = parse(markdown);
      capturedView.dispatch(capturedView.state.tr.replaceRange(
        capturedSelection.from,
        capturedSelection.to,
        attachmentDocument.slice(0, attachmentDocument.content.size)
      ));
      // Attachment insertion is already an explicit async user action. Project
      // it to the parent in this turn so the acknowledgement cannot precede the
      // editor's canonical Markdown commit.
      projectDocument();
      onImageAttachmentsCommitted(committedSnippets.length);
    }).catch((error: unknown) => onImageAttachmentError(imageAttachmentErrorMessage(error)));
    return true;
  }

  function handleImageDragOver(event: DragEvent): boolean {
    const claimed = transferMayContainImageFile(event.dataTransfer) ||
      transferContainsEphemeralImage(event.dataTransfer);
    if (!acceptImageAttachments) return claimed;
    if (claimed) event.preventDefault();
    return claimed;
  }

  function reportSelection(state: EditorState): void {
    const proof = selectionBoundaryProof(state);
    onSelectionChange(proof.byteOffset, proof.failure, proof.diagnostic);
    onFormatStateChange(visualFormatState(state));
    const firstVisibleSelection = Selection.atStart(state.doc);
    const lastVisibleSelection = Selection.atEnd(state.doc);
    const witness: VisualSelectionAccessibilityWitness = {
      available: true,
      epoch: selectionAccessibilityEpoch,
      selectionKind: state.selection.constructor.name,
      from: state.selection.from,
      to: state.selection.to,
      empty: state.selection.empty,
      allVisibleText: !state.selection.empty &&
        state.selection.from <= firstVisibleSelection.from &&
        state.selection.to >= lastVisibleSelection.to,
      caretAtEnd: state.selection.empty &&
        state.selection.from === lastVisibleSelection.from,
      caretByteOffset: proof.byteOffset
    };
    const witnessIdentity = JSON.stringify(witness);
    if (witnessIdentity !== reportedSelectionAccessibilityIdentity) {
      reportedSelectionAccessibilityIdentity = witnessIdentity;
      onSelectionAccessibilityChange(witness);
    }
  }

  function invalidateSelectionAccessibility(): void {
    selectionAccessibilityEpoch += 1;
    const witness = unavailableVisualSelectionWitness(selectionAccessibilityEpoch);
    const witnessIdentity = JSON.stringify(witness);
    if (witnessIdentity !== reportedSelectionAccessibilityIdentity) {
      reportedSelectionAccessibilityIdentity = witnessIdentity;
      onSelectionAccessibilityChange(witness);
    }
  }

  function scheduleSelectionReport(delay = 48): void {
    if (selectionReportTimer !== undefined) window.clearTimeout(selectionReportTimer);
    selectionReportTimer = window.setTimeout(() => {
      selectionReportTimer = undefined;
      if (view && !localDocumentChanged && !composing) reportSelection(view.state);
    }, delay);
  }

  function setOptionHeld(held: boolean): void {
    if (optionHeld !== held) optionHeld = held;
    const next = reduceCompletionLens(completionLens, held
      ? { kind: 'option_down', alternativeCount: ghostAlternatives.length }
      : { kind: 'release_option' });
    if (completionLens !== next) completionLens = next;
  }

  function setLensPinned(pinned: boolean): void {
    if (completionLens.pinned === pinned) return;
    completionLens = pinned
      ? { ...completionLens, pinned: ghostAlternatives.length > 1 }
      : { ...completionLens, pinned: false };
    if (view) setGhostFanPinned(view, completionLens.pinned);
  }

  function editorHasExactFocus(): boolean {
    return Boolean(view && !view.isDestroyed && view.hasFocus());
  }

  function handleWindowKeyDown(event: KeyboardEvent): void {
    if (!editorHasExactFocus()) {
      setOptionHeld(false);
      return;
    }
    // Every keydown is a fresh physical-state witness. This recovers from a
    // swallowed Option-up without guessing from the previous event sequence.
    setOptionHeld(
      (event.key === 'Alt' || event.altKey) &&
      !event.metaKey &&
      !event.ctrlKey
    );
  }

  function handleWindowKeyUp(event: KeyboardEvent): void {
    if (!editorHasExactFocus() || event.key === 'Alt' || !event.altKey) setOptionHeld(false);
  }

  function releaseOptionState(): void {
    setOptionHeld(false);
  }

  function handleWindowFocus(): void {
    scheduleGhostPresentationSynchronization(true);
  }

  function handleVisibilityChange(): void {
    releaseOptionState();
    if (document.visibilityState === 'visible') scheduleGhostPresentationSynchronization(true);
  }

  onMount(() => {
    const initialMarkdown = normalizeVisualMarkdownSource(value);
    lastEmitted = initialMarkdown;
    view = new EditorView(mount, {
      state: stateFor(initialMarkdown),
      nodeViews: { image: imageNodeView },
      editable: () => !readonly,
      attributes: editorAttributes(),
      dispatchTransaction(transaction) {
        if (!view) return;
        const previousSelection = view.state.selection;
        const selectionMoved = transaction.selectionSet &&
          !transaction.selection.eq(previousSelection);
        const completionMutation = transaction.docChanged && completionMutationAuthorized;
        if (transaction.docChanged) completionMutationAuthorized = false;
        const next = view.state.apply(transaction);
        view.updateState(next);
        if (transaction.docChanged) {
          invalidateSelectionAccessibility();
          clearFormattingSelection();
          clearBoundaryCache();
          if (selectionReportTimer !== undefined) {
            window.clearTimeout(selectionReportTimer);
            selectionReportTimer = undefined;
          }
          onSelectionChange(null, 'selection_settling', null);
        } else if (transaction.selectionSet) {
          const protectedSelectionDrift = Boolean(
            selectionMoved &&
            transaction.getMeta(formattingSelectionRestoreMeta) !== true &&
            formattingRestoreDocument === next.doc &&
            formattingRestoreSelection &&
            performance.now() <= formattingRestoreDeadline &&
            !next.selection.eq(formattingRestoreSelection)
          );
          // An open formatting palette owns a selection snapshot while its
          // controls have focus. Keep that snapshot synchronized when the
          // editor itself is still focused, including keyboard and AX-driven
          // selection changes that do not emit a pointerdown on the palette.
          if (
            formattingSelection &&
            formattingSelectionDocument === next.doc &&
            view.hasFocus() &&
            !protectedSelectionDrift
          ) {
            formattingSelection = next.selection;
          }
          if (protectedSelectionDrift) {
            scheduleFormattingSelectionRestore(formattingRestoreIntentEpoch);
          }
          if (
            view.hasFocus() &&
            selectionMoved &&
            !protectedSelectionDrift &&
            transaction.getMeta(formattingSelectionRestoreMeta) !== true
          ) onCaretNavigation();
          if (selectionMoved) {
            invalidateSelectionAccessibility();
            onSelectionChange(null, 'selection_settling', null);
            scheduleSelectionReport();
          } else {
            // A same-selection transaction is an explicit reconciliation, not
            // a transient unknown caret. Preserve the exact boundary so the
            // parent cannot briefly de-authorize an otherwise exact session.
            reportSelection(next);
          }
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
          if (!composing) {
            // Word stepping is already authorized against the exact visible
            // completion session. Project it into the parent immediately so
            // the next Option-Left/Right event observes the updated anchor and
            // cached remainder instead of falling into the ordinary 240 ms
            // typing debounce.
            if (completionMutation) projectDocument();
            else scheduleProjection();
          }
        }
      },
      handleDOMEvents: {
        keydown() {
          // A focused editor key event starts a new interaction epoch. Any
          // delayed WebKit repair belongs to the palette activation that came
          // before it and must never overwrite the resulting navigation.
          cancelFormattingSelectionRestore();
          return false;
        },
        pointerdown() {
          // Cancel before WebKit publishes the pointer-derived selection; the
          // later ProseMirror transaction will refresh the still-open lease.
          cancelFormattingSelectionRestore();
          return false;
        },
        paste(_view, event) {
          return handleImageTransfer(event);
        },
        dragover(_view, event) {
          return handleImageDragOver(event);
        },
        drop(_view, event) {
          return handleImageTransfer(event);
        },
        focus() {
          // Focus acquisition is a new interaction epoch. If Option is still
          // physically held, its next key event will re-establish that fact.
          releaseOptionState();
          scheduleGhostPresentationSynchronization(true);
          return false;
        },
        blur() {
          releaseOptionState();
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
    scheduleGhostPresentationSynchronization();
    window.addEventListener('resize', reportGhostVisibility);
    window.addEventListener('focus', handleWindowFocus);
    window.addEventListener('keydown', handleWindowKeyDown, true);
    window.addEventListener('keyup', handleWindowKeyUp, true);
    window.addEventListener('blur', releaseOptionState);
    window.addEventListener('pagehide', releaseOptionState);
    window.addEventListener('pointerdown', releaseOptionState, true);
    document.addEventListener('visibilitychange', handleVisibilityChange);
    scrollViewport?.addEventListener('scroll', reportGhostVisibility, { passive: true });
    if (autofocus) view.focus();
  });

  $: if (view && value !== lastEmitted && !composing && !localDocumentChanged) {
    const normalized = normalizeVisualMarkdownSource(value);
    // The parent replaced the document authority. Neither a captured
    // selection nor a delayed repair from the previous node tree can cross
    // this boundary.
    clearFormattingSelection();
    invalidateSelectionAccessibility();
    lastEmitted = normalized;
    const next = stateFor(normalized);
    view.updateState(next);
    clearBoundaryCache();
    reportSelection(next);
    scheduleGhostVisibilityReport();
    scheduleExternalNormalization(value, normalized);
  }

  $: if (view) {
    synchronizeGhostPresentation({
      label,
      readonly,
      composing,
      text: ghostText,
      candidateId: ghostCandidateId,
      presentationKey: ghostPresentationKey,
      anchorByteOffset: ghostAnchorByteOffset,
      insertsOnAccept: ghostInsertsOnAccept,
      alternatives: ghostAlternatives,
      hidden: ghostHidden,
      unconsumeText: ghostUnconsumeText,
      surfaceKey,
      suppressedKey: suppressedGhostKey,
      optionHeld,
      lensPinned: completionLens.pinned
    });
  }

  $: if (ghostAlternatives.length < 2 && completionLens !== CLOSED_COMPLETION_LENS) {
    completionLens = reduceCompletionLens(completionLens, {
      kind: 'alternatives_changed',
      alternativeCount: ghostAlternatives.length
    });
    if (view) setGhostFanPinned(view, false);
  }
  $: if (ghostAlternatives.length > 1 && optionHeld && !completionLens.momentary) {
    completionLens = reduceCompletionLens(completionLens, {
      kind: 'option_down',
      alternativeCount: ghostAlternatives.length
    });
  }

  onDestroy(() => {
    clearFormattingSelection();
    if (projectionTimer !== undefined) window.clearTimeout(projectionTimer);
    if (normalizationTimer !== undefined) window.clearTimeout(normalizationTimer);
    if (visibilityFrame !== undefined) window.cancelAnimationFrame(visibilityFrame);
    if (ghostSynchronizationFrame !== undefined) {
      window.cancelAnimationFrame(ghostSynchronizationFrame);
    }
    if (selectionReportTimer !== undefined) window.clearTimeout(selectionReportTimer);
    if (composing) onCompositionChange(false);
    onSelectionChange(null, 'selection_settling', null);
    if (reportedGhostPresentationKey) onGhostVisibilityChange('');
    onCompletionAccessibilityChange({
      available: false,
      optionHeld: false,
      fanVisible: false,
      lensPinned: false,
      inlineHidden: true,
      selectedCandidateId: '',
      selectedPresentationKey: '',
      alternativeCandidateIds: [],
      alternativePresentationKeys: [],
      alternativeRunIds: []
    });
    invalidateSelectionAccessibility();
    window.removeEventListener('resize', reportGhostVisibility);
    window.removeEventListener('focus', handleWindowFocus);
    window.removeEventListener('keydown', handleWindowKeyDown, true);
    window.removeEventListener('keyup', handleWindowKeyUp, true);
    window.removeEventListener('blur', releaseOptionState);
    window.removeEventListener('pagehide', releaseOptionState);
    window.removeEventListener('pointerdown', releaseOptionState, true);
    document.removeEventListener('visibilitychange', handleVisibilityChange);
    scrollViewport?.removeEventListener('scroll', reportGhostVisibility);
    view?.destroy();
  });
</script>

<div class="loom-editor-shell">
  <div class="editor-mount" bind:this={mount}></div>
</div>
