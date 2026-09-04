<script lang="ts">
  import { onDestroy, onMount, tick } from 'svelte';
  import { completionOptionAccessibleLabel } from './ghostText';
  import { allocateCompletionPopupDomIds, placeCompletionPopup } from './completionPopup';
  import {
    CLOSED_COMPLETION_LENS,
    completionLensVisible,
    reduceCompletionLens,
    type CompletionLensState
  } from './completionLens';
  import type { VerseNewlineKind } from './verseCodec';
  import {
    nextSuggestionWord,
    type CompletionInsertionAction,
    type SuggestionAlternative
  } from './suggestionInteraction';
  import {
    planSourceGhostText,
    renderedSourceGhostPresentationKey,
    sourceGhostAnchorMatches,
    sourceGhostKeyAction,
    sourceGhostRectIntersectsViewport,
    sourceTabEdit,
    sourceGhostVisibilityWitnessMatches,
    sourceMirrorDirectionIsSupported,
    sourceMirrorGeometry,
    sourceShiftTabEdit,
    sourceTextHasStrongRtl,
    type SourceGhostAnchor,
    type SourceGhostPlan,
    type SourceGhostPresentation
  } from './sourceGhostText';
  import {
    STALE_IMAGE_ATTACHMENT_ERROR,
    UNREADABLE_TRANSFER_IMAGE_ERROR,
    UNVERIFIED_DROP_FILE_ERROR,
    imageAttachmentErrorMessage,
    imageFilesFromTransfer,
    transferContainsEphemeralImage,
    transferMayContainImageFile
  } from './attachments';

  export let element: HTMLTextAreaElement | undefined;
  export let value = '';
  export let readonly = false;
  export let verse = false;
  export let verseNewline: VerseNewlineKind | null = null;
  export let surfaceKey = '';
  export let label = 'Markdown source editor';
  export let ghostText = '';
  export let ghostCandidateId = '';
  export let ghostPresentationKey = '';
  export let ghostInsertsOnAccept = false;
  export let ghostAlternatives: readonly SuggestionAlternative[] = [];
  export let ghostHidden = false;
  export let ghostUnconsumeText = '';
  export let onImageAttachments: (files: readonly File[]) => Promise<readonly string[]> =
    async () => [];
  export let onImageAttachmentsCommitted: (count: number) => void = () => {};
  export let onImageAttachmentError: (message: string) => void = () => {};
  export let onValueInput: (textarea: HTMLTextAreaElement) => void = () => {};
  export let onSelectionChange: (textarea: HTMLTextAreaElement) => void = () => {};
  export let onCompositionStart: () => void = () => {};
  export let onCompositionEnd: (textarea: HTMLTextAreaElement) => void = () => {};
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
  export let onGhostVisibilityChange: (presentationKey: string) => void = () => {};

  let shell: HTMLDivElement;
  let viewport: HTMLDivElement;
  let mirror: HTMLDivElement;
  let ghostSpan: HTMLSpanElement;
  let focused = false;
  let composing = false;
  let exactGeometry = false;
  let selectionStart = 0;
  let selectionEnd = 0;
  let suppressedPresentationKey = '';
  let observedPresentationKey = '';
  let observedValue = value;
  let observedSurfaceKey = surfaceKey;
  let resizeObserver: ResizeObserver | undefined;
  let geometryFrame: number | undefined;
  let reportedVisiblePresentationKey = '';
  let ltrContent = true;
  let plan: SourceGhostPlan | null = null;
  let presentationAnchor: SourceGhostAnchor | null = null;
  let completionLens: CompletionLensState = CLOSED_COMPLETION_LENS;
  let optionHeld = false;
  let optionFanVisible = false;
  let lensVisible = false;
  let lensTrigger: HTMLButtonElement;
  let suggestionFan: HTMLDivElement;
  let fanPlacementFrame: number | undefined;
  let selectedFanOptionIndex = -1;
  let controlledFanId: string | undefined;
  let activeFanOptionId: string | undefined;
  const completionPopupDomIds = allocateCompletionPopupDomIds('source');

  const mirroredProperties = [
    'direction',
    'font-family',
    'font-feature-settings',
    'font-kerning',
    'font-size',
    'font-stretch',
    'font-style',
    'font-variant',
    'font-variation-settings',
    'font-weight',
    'letter-spacing',
    'line-height',
    'overflow-wrap',
    'tab-size',
    'text-align',
    'text-indent',
    'text-rendering',
    'text-transform',
    'white-space',
    'word-break',
    'word-spacing',
    'writing-mode'
  ] as const;

  function presentation(): SourceGhostPresentation | null {
    const currentValue = element?.value ?? value;
    if (
      !ghostPresentationKey ||
      ghostPresentationKey === suppressedPresentationKey ||
      !sourceGhostAnchorMatches(
        presentationAnchor,
        currentValue,
        surfaceKey,
        selectionStart,
        selectionEnd
      )
    ) return null;
    return {
      active: true,
      candidateId: ghostCandidateId,
      presentationKey: ghostPresentationKey,
      text: ghostText,
      rollbackOnly: ghostText === '' && ghostUnconsumeText !== ''
    };
  }

  function currentPlan(): SourceGhostPlan | null {
    return planSourceGhostText({
      presentation: presentation(),
      value: element?.value ?? value,
      selectionStart,
      selectionEnd,
      focused,
      composing,
      readonly,
      exactGeometry,
      ltrContent,
      verseNewline: verse ? verseNewline : null
    });
  }

  function reportVisiblePresentationKey(presentationKey: string): void {
    if (reportedVisiblePresentationKey === presentationKey) return;
    reportedVisiblePresentationKey = presentationKey;
    onGhostVisibilityChange(presentationKey);
  }

  function placeSourceFan(): void {
    fanPlacementFrame = undefined;
    if (
      !plan ||
      !ghostSpan?.isConnected
    ) return;
    if (!visibleSourceGhostPlan(plan, true)) {
      // Viewport clamping cannot supply the missing insertion witness. Close
      // the fixed fan when its mirrored caret has scrolled out of view.
      optionHeld = false;
      completionLens = CLOSED_COMPLETION_LENS;
      return;
    }
    const ghostRect = ghostSpan.getClientRects().item(0) ?? ghostSpan.getBoundingClientRect();
    if (lensTrigger?.isConnected) {
      const surfaceRect = shell.getBoundingClientRect();
      const triggerWidth = Math.max(lensTrigger.offsetWidth, 34);
      const triggerHeight = Math.max(lensTrigger.offsetHeight, 22);
      lensTrigger.style.left = `${Math.max(
        12,
        Math.min(window.innerWidth - triggerWidth - 12, surfaceRect.right - triggerWidth - 18)
      )}px`;
      lensTrigger.style.top = `${Math.max(
        12,
        Math.min(window.innerHeight - triggerHeight - 12, ghostRect.top)
      )}px`;
    }
    if (!lensVisible || !suggestionFan?.isConnected) return;
    suggestionFan.style.maxHeight = '';
    placeCompletionPopup(suggestionFan, {
      left: ghostRect.left,
      right: ghostRect.left,
      top: ghostRect.top,
      bottom: ghostRect.bottom,
      width: 0,
      height: ghostRect.height
    });
  }

  function requestFanPlacement(): void {
    if (fanPlacementFrame !== undefined) return;
    fanPlacementFrame = window.requestAnimationFrame(placeSourceFan);
  }

  function setOptionFanVisible(visible: boolean): void {
    optionHeld = visible;
    const next = reduceCompletionLens(completionLens, visible
      ? { kind: 'option_down', alternativeCount: ghostAlternatives.length }
      : { kind: 'release_option' });
    if (completionLens === next) {
      if (lensVisible) requestFanPlacement();
      return;
    }
    completionLens = next;
    if (completionLensVisible(completionLens)) void tick().then(requestFanPlacement);
  }

  function setCompletionLensPinned(pinned: boolean): void {
    if (completionLens.pinned === pinned) return;
    completionLens = pinned
      ? { ...completionLens, pinned: ghostAlternatives.length > 1 }
      : { ...completionLens, pinned: false };
    if (completionLensVisible(completionLens)) void tick().then(requestFanPlacement);
    element?.focus({ preventScroll: true });
  }

  function toggleCompletionLensPin(): void {
    setCompletionLensPinned(!completionLens.pinned);
  }

  function renderedGhostPresentationKey(
    candidate: SourceGhostPlan | null,
    allowFanHiddenGhost = false
  ): string {
    if (
      !candidate ||
      !viewport ||
      !ghostSpan ||
      ghostHidden ||
      viewport.hidden ||
      ghostSpan.hidden ||
      !viewport.isConnected ||
      !ghostSpan.isConnected
    ) return '';

    const viewportStyle = getComputedStyle(viewport);
    const ghostStyle = getComputedStyle(ghostSpan);
    const ignoreFanVisibility = allowFanHiddenGhost && lensVisible;
    if (
      viewportStyle.display === 'none' ||
      viewportStyle.visibility === 'hidden' ||
      viewportStyle.visibility === 'collapse' ||
      Number.parseFloat(viewportStyle.opacity) === 0 ||
      ghostStyle.display === 'none' ||
      (!ignoreFanVisibility && (
        ghostStyle.visibility === 'hidden' ||
        ghostStyle.visibility === 'collapse'
      )) ||
      Number.parseFloat(ghostStyle.opacity) === 0
    ) return '';

    // The first client rect is the continuation's insertion edge. Using the
    // union bounding box would let a long completion reach back into view and
    // falsely authorize an offscreen caret.
    const firstGhostRect = ghostSpan.getClientRects().item(0);
    if (
      !firstGhostRect ||
      !sourceGhostRectIntersectsViewport(firstGhostRect, viewport.getBoundingClientRect())
    ) return '';
    return candidate.presentationKey;
  }

  function visibleSourceGhostPlan(
    candidate: SourceGhostPlan | null,
    allowFanHiddenGhost = false
  ): SourceGhostPlan | null {
    const livePresentationKey = renderedGhostPresentationKey(
      candidate,
      allowFanHiddenGhost
    );
    return candidate &&
      renderedSourceGhostPresentationKey(candidate, viewport ? Boolean(viewport.hidden) : true) ===
        candidate.presentationKey &&
      sourceGhostVisibilityWitnessMatches(candidate.presentationKey, livePresentationKey)
      ? candidate
      : null;
  }

  function installPlan(next: SourceGhostPlan | null): void {
    const previousKey = plan?.presentationKey ?? '';
    plan = next;
    if (!next) {
      if (viewport) viewport.hidden = true;
      shell?.classList.remove('ghost-active');
      reportVisiblePresentationKey('');
      return;
    }
    if (previousKey === next.presentationKey && viewport && !viewport.hidden) {
      reportVisiblePresentationKey(renderedGhostPresentationKey(next));
      return;
    }
    if (viewport) viewport.hidden = true;
    reportVisiblePresentationKey('');
    const expectedKey = next.presentationKey;
    void tick().then(() => {
      if (!viewport || plan?.presentationKey !== expectedKey) return;
      viewport.hidden = false;
      reportVisiblePresentationKey(renderedGhostPresentationKey(plan));
      requestFanPlacement();
    });
  }

  function hideCurrentGhost(): void {
    installPlan(null);
  }

  function suppressCurrentGhost(): void {
    if (ghostPresentationKey) suppressedPresentationKey = ghostPresentationKey;
    installPlan(null);
    // Do not wait for Svelte's next DOM flush: an input/selection/IME event
    // must make stale completion pixels and their acceptance target disappear
    // inside the same event turn.
  }

  function readSelection(notify: boolean, suppressWhenMoved: boolean): void {
    if (!element) return;
    const nextStart = element.selectionStart;
    const nextEnd = element.selectionEnd;
    if (
      suppressWhenMoved &&
      (nextStart !== selectionStart || nextEnd !== selectionEnd)
    ) hideCurrentGhost();
    selectionStart = nextStart;
    selectionEnd = nextEnd;
    installPlan(currentPlan());
    if (notify) onSelectionChange(element);
  }

  function syncGeometry(): void {
    if (!element || !shell || !viewport || !mirror || element.offsetParent !== shell) {
      exactGeometry = false;
      installPlan(null);
      return;
    }
    const computed = getComputedStyle(element);
    const expectedWhitespace = verse ? 'pre' : 'pre-wrap';
    if (
      computed.transform !== 'none' ||
      !sourceMirrorDirectionIsSupported(computed.direction) ||
      computed.writingMode !== 'horizontal-tb' ||
      computed.whiteSpace !== expectedWhitespace
    ) {
      exactGeometry = false;
      installPlan(null);
      return;
    }
    const geometry = sourceMirrorGeometry({
      clientWidth: element.clientWidth,
      clientHeight: element.clientHeight,
      scrollWidth: element.scrollWidth,
      scrollHeight: element.scrollHeight,
      scrollLeft: element.scrollLeft,
      scrollTop: element.scrollTop,
      offsetLeft: element.offsetLeft,
      offsetTop: element.offsetTop,
      clientLeft: element.clientLeft,
      clientTop: element.clientTop,
      wraps: !verse
    });
    if (!geometry) {
      exactGeometry = false;
      installPlan(null);
      return;
    }

    for (const property of mirroredProperties) {
      mirror.style.setProperty(property, computed.getPropertyValue(property));
    }
    mirror.style.setProperty('box-sizing', 'border-box');
    mirror.style.setProperty('padding-top', computed.paddingTop);
    mirror.style.setProperty('padding-right', computed.paddingRight);
    mirror.style.setProperty('padding-bottom', computed.paddingBottom);
    mirror.style.setProperty('padding-left', computed.paddingLeft);
    mirror.style.width = `${geometry.canvasWidth}px`;
    mirror.style.minHeight = `${geometry.canvasHeight}px`;
    mirror.style.transform = `translate(${geometry.translateX}px, ${geometry.translateY}px)`;
    viewport.style.left = `${geometry.viewportLeft}px`;
    viewport.style.top = `${geometry.viewportTop}px`;
    viewport.style.width = `${geometry.viewportWidth}px`;
    viewport.style.height = `${geometry.viewportHeight}px`;
    exactGeometry = true;
    installPlan(currentPlan());
    requestFanPlacement();
  }

  function requestGeometrySync(): void {
    if (geometryFrame !== undefined) return;
    geometryFrame = window.requestAnimationFrame(() => {
      geometryFrame = undefined;
      syncGeometry();
    });
  }

  function invalidateAndRequestGeometry(): void {
    installPlan(null);
    requestGeometrySync();
  }

  function handleFocus(): void {
    focused = true;
    readSelection(true, false);
    syncGeometry();
  }

  function handleBlur(): void {
    focused = false;
    setOptionFanVisible(false);
    hideCurrentGhost();
  }

  function handleBeforeInput(): void {
    suppressCurrentGhost();
  }

  function handleInput(): void {
    suppressCurrentGhost();
    readSelection(false, false);
    if (element) onValueInput(element);
  }

  function handleImageTransfer(event: DragEvent | ClipboardEvent): void {
    const transfer = event instanceof ClipboardEvent ? event.clipboardData : event.dataTransfer;
    const files = imageFilesFromTransfer(transfer);
    const ephemeralImage = transferContainsEphemeralImage(transfer);
    const claimedFileDrop = event instanceof DragEvent && transferMayContainImageFile(transfer);
    if (files.length === 0 && !ephemeralImage && !claimedFileDrop) return;
    event.preventDefault();
    event.stopPropagation();
    if (files.length === 0) {
      onImageAttachmentError(
        claimedFileDrop && !ephemeralImage
          ? UNVERIFIED_DROP_FILE_ERROR
          : UNREADABLE_TRANSFER_IMAGE_ERROR
      );
      return;
    }
    if (!element || readonly || composing) {
      onImageAttachmentError('Images cannot be attached while this editor is unavailable.');
      return;
    }

    const capturedElement = element;
    const capturedValue = capturedElement.value;
    const capturedSurfaceKey = surfaceKey;
    const capturedStart = capturedElement.selectionStart;
    const capturedEnd = capturedElement.selectionEnd;
    void onImageAttachments(files).then((snippets) => {
      const committedSnippets = snippets.filter((snippet) => snippet.length > 0);
      if (
        !element ||
        element !== capturedElement ||
        !capturedElement.isConnected ||
        readonly ||
        composing ||
        surfaceKey !== capturedSurfaceKey ||
        capturedElement.value !== capturedValue
      ) {
        if (committedSnippets.length > 0) {
          onImageAttachmentError(STALE_IMAGE_ATTACHMENT_ERROR);
        }
        return;
      }
      const markdown = committedSnippets.join('\n\n');
      if (!markdown) return;
      const capturedSelectionIsCurrent =
        capturedElement.selectionStart === capturedStart &&
        capturedElement.selectionEnd === capturedEnd;
      capturedElement.setRangeText(
        markdown,
        capturedStart,
        capturedEnd,
        capturedSelectionIsCurrent ? 'end' : 'preserve'
      );
      observedValue = capturedElement.value;
      readSelection(false, false);
      onValueInput(capturedElement);
      onImageAttachmentsCommitted(committedSnippets.length);
    }).catch((error: unknown) => onImageAttachmentError(imageAttachmentErrorMessage(error)));
  }

  function handleImageDragOver(event: DragEvent): void {
    if (
      transferMayContainImageFile(event.dataTransfer) ||
      transferContainsEphemeralImage(event.dataTransfer)
    ) event.preventDefault();
  }

  function handleSelection(): void {
    readSelection(true, true);
  }

  function handleCompositionStart(): void {
    composing = true;
    suppressCurrentGhost();
    onCompositionStart();
  }

  function handleCompositionEnd(): void {
    composing = false;
    suppressCurrentGhost();
    readSelection(false, false);
    if (element) onCompositionEnd(element);
  }

  function handleKeydown(event: KeyboardEvent): void {
    const candidate = currentPlan();
    const visible = visibleSourceGhostPlan(candidate, lensVisible);
    if (lensVisible && !visible) completionLens = CLOSED_COMPLETION_LENS;
    if (
      (event.key === 'Alt' || event.altKey) &&
      visible &&
      ghostAlternatives.length > 1
    ) {
      setOptionFanVisible(true);
      if (event.key === 'Alt') return;
    }
    if (
      visible &&
      lensVisible &&
      !event.metaKey &&
      !event.ctrlKey &&
      !event.altKey &&
      event.key === 'Escape'
    ) {
      event.preventDefault();
      event.stopPropagation();
      completionLens = CLOSED_COMPLETION_LENS;
      return;
    }
    if (
      visible &&
      completionLens.pinned &&
      !event.metaKey &&
      !event.ctrlKey &&
      !event.altKey &&
      (event.key === 'ArrowUp' || event.key === 'ArrowDown')
    ) {
      event.preventDefault();
      event.stopPropagation();
      onGhostCycle(event.key === 'ArrowDown' ? 1 : -1);
      return;
    }
    if (
      candidate &&
      ghostUnconsumeText &&
      event.altKey &&
      !event.metaKey &&
      !event.ctrlKey &&
      event.key === 'ArrowLeft' &&
      element &&
      element.selectionStart === element.selectionEnd &&
      element.value.slice(0, element.selectionStart).endsWith(ghostUnconsumeText) &&
      onGhostUnconsume(candidate.candidateId, candidate.presentationKey, ghostUnconsumeText)
    ) {
      event.preventDefault();
      event.stopPropagation();
      const end = element.selectionStart;
      element.setRangeText('', end - ghostUnconsumeText.length, end, 'end');
      // This exact DOM mutation was authorized by the parent completion
      // session. Record its value before Svelte echoes it back through the
      // `value` prop so the reactive external-edit guard cannot suppress the
      // newly restored remainder presentation.
      observedValue = element.value;
      suppressCurrentGhost();
      readSelection(false, false);
      onValueInput(element);
      return;
    }
    if (
      candidate &&
      ghostUnconsumeText &&
      event.key === 'Escape' &&
      !event.altKey &&
      !event.metaKey &&
      !event.ctrlKey
    ) {
      event.preventDefault();
      event.stopPropagation();
      suppressCurrentGhost();
      onGhostDismiss(candidate.candidateId, candidate.presentationKey);
      return;
    }
    if (
      visible &&
      lensVisible &&
      (event.altKey || completionLens.pinned) &&
      (event.key === 'Enter' || event.key === 'Tab') &&
      insertVisibleGhostText(
        visible,
        visible.text,
        event.key === 'Enter' ? 'fan_return' : 'fan_tab'
      )
    ) {
      event.preventDefault();
      event.stopPropagation();
      suppressCurrentGhost();
      return;
    }
    const action = sourceGhostKeyAction(event, Boolean(visible), lensVisible && Boolean(visible));
    if (!action) return;
    if ((action === 'cycle_next' || action === 'cycle_previous') && visible) {
      event.preventDefault();
      event.stopPropagation();
      onGhostCycle(action === 'cycle_next' ? 1 : -1);
      return;
    }
    if (action === 'dismiss') {
      if (!visible) return;
      event.preventDefault();
      event.stopPropagation();
      suppressCurrentGhost();
      onGhostDismiss(visible.candidateId, visible.presentationKey);
      return;
    }
    if (action === 'accept' && visible) {
      // The parent must consume the exact visibility witness before this
      // component clears it. Its boolean result is the authority to promote.
      const accepted = ghostInsertsOnAccept
        ? insertVisibleGhostText(visible, visible.text, 'inline_tab')
        : onGhostAccept(visible.candidateId, visible.presentationKey);
      suppressCurrentGhost();
      if (accepted) {
        event.preventDefault();
        event.stopPropagation();
        return;
      }
    }
    if (action === 'accept_word' && visible) {
      const word = nextSuggestionWord(visible.text);
      if (word && insertVisibleGhostText(visible, word, 'option_word')) {
        event.preventDefault();
        event.stopPropagation();
      }
      return;
    }
    if (!element || readonly) return;
    if (action === 'remove_tab_indent') {
      const edit = sourceShiftTabEdit(
        element.value,
        element.selectionStart,
        element.selectionEnd
      );
      // On an unindented line Shift-Tab remains ordinary keyboard navigation
      // out of the textarea instead of becoming a focus trap.
      if (!edit) return;
      event.preventDefault();
      event.stopPropagation();
      suppressCurrentGhost();
      element.value = edit.value;
      element.setSelectionRange(edit.selectionStart, edit.selectionEnd);
      readSelection(false, false);
      onValueInput(element);
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    const edit = sourceTabEdit(
      element.value,
      element.selectionStart,
      element.selectionEnd
    );
    if (!edit) return;
    // Preserve source bytes exactly. setRangeText also keeps the native caret
    // and selection semantics of an ordinary textarea edit.
    suppressCurrentGhost();
    element.setRangeText('\t', element.selectionStart, element.selectionEnd, 'end');
    if (element.value !== edit.value || element.selectionStart !== edit.caret) return;
    readSelection(false, false);
    onValueInput(element);
  }

  function handleKeyup(event: KeyboardEvent): void {
    if (event.key === 'Alt' || !event.altKey) setOptionFanVisible(false);
    handleSelection();
  }

  function handleWindowOptionDown(event: KeyboardEvent): void {
    const candidate = currentPlan();
    const visible = visibleSourceGhostPlan(candidate, optionFanVisible);
    if (
      (event.key === 'Alt' || (event.altKey && !event.metaKey && !event.ctrlKey)) &&
      visible &&
      ghostAlternatives.length > 1
    ) {
      setOptionFanVisible(true);
    } else if (optionFanVisible && !visible) {
      setOptionFanVisible(false);
    }
  }

  function handleWindowOptionUp(event: KeyboardEvent): void {
    if (event.key === 'Alt' || !event.altKey) setOptionFanVisible(false);
  }

  function handleWindowBlur(): void {
    setOptionFanVisible(false);
  }

  function handleWindowGeometryChange(): void {
    requestFanPlacement();
  }

  function insertVisibleGhostText(
    candidate: SourceGhostPlan,
    text: string,
    action: CompletionInsertionAction
  ): boolean {
    if (
      !element ||
      readonly ||
      !text ||
      !onGhostInsert(candidate.candidateId, candidate.presentationKey, text, action)
    ) return false;
    const start = element.selectionStart;
    const end = element.selectionEnd;
    element.setRangeText(text, start, end, 'end');
    if (element.selectionStart !== start + text.length) return false;
    // The parent has already consumed these exact bytes. Distinguish its
    // ensuing value-prop echo from a manual/external edit; otherwise the echo
    // suppresses the new session remainder merely because it is new text.
    observedValue = element.value;
    readSelection(false, false);
    onValueInput(element);
    return true;
  }

  function handleDocumentSelectionChange(): void {
    if (document.activeElement === element) readSelection(true, true);
  }

  export function focusAtDocumentEnd(): boolean {
    if (!element || readonly) return false;
    hideCurrentGhost();
    element.focus({ preventScroll: true });
    const end = element.value.length;
    element.setSelectionRange(end, end);
    selectionStart = end;
    selectionEnd = end;
    focused = document.activeElement === element;
    onSelectionChange(element);
    syncGeometry();
    return focused;
  }

  export function focusCurrentSelection(): boolean {
    if (!element || readonly) return false;
    element.focus({ preventScroll: true });
    focused = document.activeElement === element;
    if (focused) onSelectionChange(element);
    syncGeometry();
    return focused;
  }

  export function insertAttachmentMarkdown(markdown: string): boolean {
    if (!element || readonly || composing || !markdown.trim()) return false;
    const start = element.selectionStart;
    const end = element.selectionEnd;
    const before = value.slice(0, start);
    const after = value.slice(end);
    const prefix = before && !before.endsWith('\n') ? '\n\n' : '';
    const suffix = after && !after.startsWith('\n') ? '\n\n' : '';
    const inserted = `${prefix}${markdown}${suffix}`;
    element.value = `${before}${inserted}${after}`;
    const caret = start + inserted.length;
    element.setSelectionRange(caret, caret);
    onValueInput(element);
    onSelectionChange(element);
    element.focus();
    return true;
  }

  export function acceptGhostWord(requireVisible = true): boolean {
    const candidate = currentPlan();
    if (
      !focused ||
      !candidate ||
      (requireVisible && renderedGhostPresentationKey(candidate) !== candidate.presentationKey)
    ) return false;
    const word = nextSuggestionWord(candidate.text);
    if (!word) return false;
    const accepted = insertVisibleGhostText(candidate, word, 'shuttle_word');
    if (accepted) suppressCurrentGhost();
    return accepted;
  }

  onMount(() => {
    if (!element) return;
    selectionStart = element.selectionStart;
    selectionEnd = element.selectionEnd;
    focused = document.activeElement === element;
    resizeObserver = new ResizeObserver(invalidateAndRequestGeometry);
    resizeObserver.observe(element);
    document.addEventListener('selectionchange', handleDocumentSelectionChange);
    window.addEventListener('keydown', handleWindowOptionDown, true);
    window.addEventListener('keyup', handleWindowOptionUp, true);
    window.addEventListener('blur', handleWindowBlur);
    window.addEventListener('resize', handleWindowGeometryChange);
    window.addEventListener('scroll', handleWindowGeometryChange, true);
    syncGeometry();
  });

  $: if (ghostPresentationKey !== observedPresentationKey) {
    ltrContent;
    observedPresentationKey = ghostPresentationKey;
    suppressedPresentationKey = '';
    presentationAnchor = ghostPresentationKey
      ? {
          value: element?.value ?? value,
          surfaceKey,
          selectionStart,
          selectionEnd
        }
      : null;
    installPlan(currentPlan());
  }

  $: selectedFanOptionIndex = plan && ghostAlternatives.length > 1
    ? ghostAlternatives.findIndex(
        (alternative) => alternative.presentationKey === plan?.presentationKey
      )
    : -1;
  $: controlledFanId = lensVisible && selectedFanOptionIndex >= 0
    ? completionPopupDomIds.listboxId
    : undefined;

  $: lensVisible = completionLensVisible(completionLens);
  $: optionFanVisible = completionLens.momentary;
  $: if (ghostAlternatives.length < 2 && completionLens !== CLOSED_COMPLETION_LENS) {
    completionLens = reduceCompletionLens(completionLens, {
      kind: 'alternatives_changed',
      alternativeCount: ghostAlternatives.length
    });
  }
  $: if (ghostAlternatives.length > 1 && optionHeld && !completionLens.momentary) {
    completionLens = reduceCompletionLens(completionLens, {
      kind: 'option_down',
      alternativeCount: ghostAlternatives.length
    });
  }
  $: activeFanOptionId = lensVisible && selectedFanOptionIndex >= 0
    ? completionPopupDomIds.optionId(selectedFanOptionIndex)
    : undefined;

  $: ltrContent = !sourceTextHasStrongRtl(value) && !sourceTextHasStrongRtl(ghostText);

  $: if (value !== observedValue) {
    observedValue = value;
    suppressCurrentGhost();
    void tick().then(() => {
      if (!element) return;
      selectionStart = element.selectionStart;
      selectionEnd = element.selectionEnd;
      requestGeometrySync();
    });
  }

  $: if (surfaceKey !== observedSurfaceKey) {
    observedSurfaceKey = surfaceKey;
    suppressCurrentGhost();
    exactGeometry = false;
    void tick().then(requestGeometrySync);
  }

  $: if (element && mirror && viewport) {
    ghostText;
    ltrContent;
    readonly;
    verse;
    verseNewline;
    void tick().then(requestGeometrySync);
  }

  onDestroy(() => {
    resizeObserver?.disconnect();
    if (geometryFrame !== undefined) window.cancelAnimationFrame(geometryFrame);
    if (fanPlacementFrame !== undefined) window.cancelAnimationFrame(fanPlacementFrame);
    reportVisiblePresentationKey('');
    document.removeEventListener('selectionchange', handleDocumentSelectionChange);
    window.removeEventListener('keydown', handleWindowOptionDown, true);
    window.removeEventListener('keyup', handleWindowOptionUp, true);
    window.removeEventListener('blur', handleWindowBlur);
    window.removeEventListener('resize', handleWindowGeometryChange);
    window.removeEventListener('scroll', handleWindowGeometryChange, true);
  });
</script>

<div
  class:verse
  class:ghost-active={Boolean(plan)}
  class="source-editor-shell"
  bind:this={shell}
>
  <div class="source-ghost-viewport" aria-hidden="true" hidden={!plan} bind:this={viewport}>
    <div class="source-ghost-mirror" bind:this={mirror}>
      {#if plan}
        <span>{plan.prefix}</span><span class:ghost-text-hidden={ghostHidden} class="loom-source-ghost-text" bind:this={ghostSpan}>{plan.text}</span><span>{plan.suffix}</span><span class="source-ghost-sentinel">&#8203;</span>
      {/if}
    </div>
  </div>
  <textarea
    bind:this={element}
    class:verse
    {value}
    {readonly}
    on:focus={handleFocus}
    on:blur={handleBlur}
    on:beforeinput={handleBeforeInput}
    on:paste={handleImageTransfer}
    on:dragover={handleImageDragOver}
    on:drop={handleImageTransfer}
    on:input={handleInput}
    on:select={handleSelection}
    on:click={handleSelection}
    on:keyup={handleKeyup}
    on:keydown={handleKeydown}
    on:scroll={invalidateAndRequestGeometry}
    on:compositionstart={handleCompositionStart}
    on:compositionend={handleCompositionEnd}
    aria-label={label}
    aria-controls={controlledFanId}
    aria-activedescendant={activeFanOptionId}
    spellcheck="true"
    wrap={verse ? 'off' : 'soft'}
  ></textarea>
  {#if plan && ghostAlternatives.length > 1}
    <button
      bind:this={lensTrigger}
      class="loom-completion-lens-trigger source-completion-lens-trigger"
      class:pinned={completionLens.pinned}
      type="button"
      tabindex="-1"
      aria-label={completionLens.pinned ? 'Unpin completion alternatives' : 'Pin completion alternatives'}
      aria-expanded={lensVisible}
      aria-controls={completionPopupDomIds.listboxId}
      on:mousedown|preventDefault|stopPropagation
      on:click|preventDefault|stopPropagation={toggleCompletionLensPin}
    >{Math.max(1, selectedFanOptionIndex + 1)}/{ghostAlternatives.length}</button>
  {/if}
  {#if plan && lensVisible && ghostAlternatives.length > 1}
    <div
      class="source-suggestion-fan"
      bind:this={suggestionFan}
      id={completionPopupDomIds.listboxId}
      role="listbox"
      aria-label="Completion suggestions"
      aria-keyshortcuts="Alt+ArrowUp Alt+ArrowDown Alt+Enter Alt+Tab"
    >
      {#each ghostAlternatives as alternative, index (alternative.runId ?? alternative.candidateId)}
        <button
          type="button"
          tabindex="-1"
          class:active={alternative.presentationKey === plan.presentationKey}
          class="loom-ghost-fan-row"
          id={completionPopupDomIds.optionId(index)}
          role="option"
          aria-selected={alternative.presentationKey === plan.presentationKey}
          aria-label={completionOptionAccessibleLabel(
            index + 1,
            ghostAlternatives.length,
            alternative.text
          )}
          on:mousedown|preventDefault|stopPropagation
          on:click|preventDefault|stopPropagation={() => {
            const offset = index - selectedFanOptionIndex;
            if (offset !== 0) onGhostCycle(offset);
            setCompletionLensPinned(true);
          }}
        >
          <span class="loom-ghost-fan-index">{index + 1}</span><span>{alternative.text}</span>
        </button>
      {/each}
      <div class="loom-ghost-fan-hint">↑↓ choose&nbsp; · &nbsp;Tab insert&nbsp; · &nbsp;click counter to pin</div>
    </div>
  {/if}
</div>
