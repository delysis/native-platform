<script lang="ts">
  import { onMount, tick } from 'svelte';
  import LoomEditor from './LoomEditor.svelte';
  import SourceEditor from './SourceEditor.svelte';
  import {
    authorizeCompletionInsertion,
    clearCompletionSession,
    completionControllerView,
    initialCompletionControllerState,
    observeTextMutation,
    reconcileCompletionController
  } from './completionController';
  import { completionPresentation } from './completionSession';
  import type { InlineGhostSuggestion } from './inlineSuggestionFamily';
  import type { CompletionInsertionAction } from './suggestionInteraction';

  export let mode: 'visual' | 'source' = 'visual';

  const family: InlineGhostSuggestion[] = [
    {
      candidateId: 'candidate-a',
      presentationKey: 'candidate-a:presentation',
      text: ' one two',
      runId: 'run-a',
      targetByte: 5,
      insertsOnAccept: true
    },
    {
      candidateId: 'candidate-b',
      presentationKey: 'candidate-b:presentation',
      text: ' another path',
      runId: 'run-b',
      targetByte: 5,
      insertsOnAccept: true
    },
    {
      candidateId: 'candidate-c',
      presentationKey: 'candidate-c:presentation',
      text: ' third road',
      runId: 'run-c',
      targetByte: 5,
      insertsOnAccept: true
    },
    {
      candidateId: 'candidate-d',
      presentationKey: 'candidate-d:presentation',
      text: ' final turn',
      runId: 'run-d',
      targetByte: 5,
      insertsOnAccept: true
    }
  ];
  const contextKey = `browser-session:browser-document:1:${mode}`;
  let markdown = 'Hello';
  let visualEditor: LoomEditor;
  let sourceEditor: SourceEditor;
  let sourceTextarea: HTMLTextAreaElement;
  let ready = false;
  let invalidations = 0;
  let controller = reconcileCompletionController(
    initialCompletionControllerState(),
    contextKey,
    family
  );

  $: controllerView = completionControllerView(controller, contextKey, family);
  $: selected = ready ? controllerView.selected : null;

  function insert(
    candidateId: string,
    presentationKey: string,
    text: string,
    action: CompletionInsertionAction
  ): boolean {
    const authorization = authorizeCompletionInsertion(controller, {
      contextKey,
      family: controllerView.activeFamily,
      eligible: controllerView.selected,
      candidateId,
      presentationKey,
      text,
      action,
      manuscriptText: markdown,
      promotionReady: true
    });
    controller = authorization.state;
    return authorization.authorized;
  }

  function observe(next: string): void {
    const mutation = observeTextMutation(controller, next, markdown, false);
    controller = mutation.state;
    markdown = next;
  }

  function invalidateIfManualMutation(): void {
    if (controller.pendingText !== null) return;
    invalidations += 1;
    controller = clearCompletionSession(controller);
  }

  function reportCaret(targetByte: number | null): void {
    if (controller.pendingText !== null || targetByte === null) return;
    const expected = controller.session
      ? completionPresentation(controller.session)?.targetByte ?? null
      : controllerView.selected?.targetByte ?? controller.generationIntent?.anchorByte ?? null;
    if (expected !== null && targetByte !== expected) {
      invalidations += 1;
      controller = clearCompletionSession(controller);
    }
  }

  function sourceInput(textarea: HTMLTextAreaElement): void {
    reportCaret(textarea.selectionStart);
    observe(textarea.value);
  }

  onMount(async () => {
    await tick();
    if (mode === 'visual') visualEditor.focusAtDocumentEnd();
    else sourceEditor.focusAtDocumentEnd();
    ready = true;
  });
</script>

<main>
  {#if mode === 'visual'}
    <div class="editor-pane visual-pane">
      <LoomEditor
        bind:this={visualEditor}
        value={markdown}
        ghostText={selected?.text ?? ''}
        ghostCandidateId={selected?.candidateId ?? ''}
        ghostPresentationKey={selected?.presentationKey ?? ''}
        ghostAnchorByteOffset={selected?.targetByte ?? null}
        ghostInsertsOnAccept={true}
        ghostAlternatives={controllerView.alternatives}
        ghostUnconsumeText={controllerView.unconsumeText}
        surfaceKey={contextKey}
        onChange={observe}
        onImmediateDocumentMutation={invalidateIfManualMutation}
        onSelectionChange={(targetByte) => reportCaret(targetByte)}
        onGhostInsert={insert}
        onGhostPresentationRejected={() => {}}
      />
    </div>
  {:else}
    <div class="editor-pane source-pane">
      <SourceEditor
        bind:this={sourceEditor}
        bind:element={sourceTextarea}
        value={markdown}
        surfaceKey={contextKey}
        ghostText={selected?.text ?? ''}
        ghostCandidateId={selected?.candidateId ?? ''}
        ghostPresentationKey={selected?.presentationKey ?? ''}
        ghostInsertsOnAccept={true}
        ghostAlternatives={controllerView.alternatives}
        ghostUnconsumeText={controllerView.unconsumeText}
        onValueInput={sourceInput}
        onSelectionChange={(textarea) => reportCaret(textarea.selectionStart)}
        onGhostInsert={insert}
      />
    </div>
  {/if}
  <output aria-label="Controller Markdown">{markdown}</output>
  <output aria-label="Controller Remainder">{selected?.text ?? 'none'}</output>
  <output aria-label="Controller Invalidations">{invalidations}</output>
</main>
