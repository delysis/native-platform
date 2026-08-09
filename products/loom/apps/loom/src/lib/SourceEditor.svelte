<script lang="ts">
  import { onDestroy, onMount, tick } from 'svelte';
  import type { VerseNewlineKind } from './verseCodec';
  import {
    planSourceGhostText,
    renderedSourceGhostPresentationKey,
    sourceGhostKeyAction,
    sourceMirrorDirectionIsSupported,
    sourceMirrorGeometry,
    sourceTextHasStrongRtl,
    type SourceGhostPlan,
    type SourceGhostPresentation
  } from './sourceGhostText';

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
  export let onValueInput: (textarea: HTMLTextAreaElement) => void = () => {};
  export let onSelectionChange: (textarea: HTMLTextAreaElement) => void = () => {};
  export let onCompositionStart: () => void = () => {};
  export let onCompositionEnd: (textarea: HTMLTextAreaElement) => void = () => {};
  export let onGhostAccept: (candidateId: string, presentationKey: string) => void = () => {};
  export let onGhostDismiss: (candidateId: string, presentationKey: string) => void = () => {};
  export let onGhostVisibilityChange: (presentationKey: string) => void = () => {};

  let shell: HTMLDivElement;
  let viewport: HTMLDivElement;
  let mirror: HTMLDivElement;
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
    if (
      !ghostPresentationKey ||
      ghostPresentationKey === suppressedPresentationKey
    ) return null;
    return {
      active: true,
      candidateId: ghostCandidateId,
      presentationKey: ghostPresentationKey,
      text: ghostText
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
      reportVisiblePresentationKey(
        renderedSourceGhostPresentationKey(next, viewport.hidden)
      );
      return;
    }
    if (viewport) viewport.hidden = true;
    reportVisiblePresentationKey('');
    const expectedKey = next.presentationKey;
    void tick().then(() => {
      if (!viewport || plan?.presentationKey !== expectedKey) return;
      viewport.hidden = false;
      reportVisiblePresentationKey(renderedSourceGhostPresentationKey(plan, viewport.hidden));
    });
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
    ) suppressCurrentGhost();
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
    suppressCurrentGhost();
  }

  function handleBeforeInput(): void {
    suppressCurrentGhost();
  }

  function handleInput(): void {
    suppressCurrentGhost();
    readSelection(false, false);
    if (element) onValueInput(element);
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
    const visible = candidate &&
      renderedSourceGhostPresentationKey(candidate, viewport ? Boolean(viewport.hidden) : true) ===
        reportedVisiblePresentationKey
      ? candidate
      : null;
    const action = sourceGhostKeyAction(event, Boolean(visible));
    if (!visible || !action) return;
    if (action === 'dismiss') {
      event.preventDefault();
      event.stopPropagation();
      suppressCurrentGhost();
      onGhostDismiss(visible.candidateId, visible.presentationKey);
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    suppressCurrentGhost();
    onGhostAccept(visible.candidateId, visible.presentationKey);
  }

  function handleDocumentSelectionChange(): void {
    if (document.activeElement === element) readSelection(true, true);
  }

  export function focusAtDocumentEnd(): boolean {
    if (!element || readonly) return false;
    suppressCurrentGhost();
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

  onMount(() => {
    if (!element) return;
    selectionStart = element.selectionStart;
    selectionEnd = element.selectionEnd;
    focused = document.activeElement === element;
    resizeObserver = new ResizeObserver(invalidateAndRequestGeometry);
    resizeObserver.observe(element);
    document.addEventListener('selectionchange', handleDocumentSelectionChange);
    syncGeometry();
  });

  $: if (ghostPresentationKey !== observedPresentationKey) {
    ltrContent;
    observedPresentationKey = ghostPresentationKey;
    suppressedPresentationKey = '';
    installPlan(currentPlan());
  }

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
    reportVisiblePresentationKey('');
    document.removeEventListener('selectionchange', handleDocumentSelectionChange);
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
        <span>{plan.prefix}</span><span class="loom-source-ghost-text">{plan.text}</span><span>{plan.suffix}</span><span class="source-ghost-sentinel">&#8203;</span>
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
    on:input={handleInput}
    on:select={handleSelection}
    on:click={handleSelection}
    on:keyup={handleSelection}
    on:keydown={handleKeydown}
    on:scroll={invalidateAndRequestGeometry}
    on:compositionstart={handleCompositionStart}
    on:compositionend={handleCompositionEnd}
    aria-label={label}
    spellcheck="true"
    wrap={verse ? 'off' : 'soft'}
  ></textarea>
</div>
