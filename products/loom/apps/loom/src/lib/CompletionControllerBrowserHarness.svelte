<script lang="ts">
  import { onMount, tick } from 'svelte';
  import LoomEditor from './LoomEditor.svelte';
  import SourceEditor from './SourceEditor.svelte';
  import Loompad from './Loompad.svelte';
  import { loompadPrefix, type LoompadLength } from './loompad';
  import {
    authorizeCompletionInsertion,
    authorizeCompletionUnconsume,
    clearCompletionSession,
    completionControllerView,
    cycleCompletion,
    initialCompletionControllerState,
    observeTextMutation,
    reconcileCompletionController,
    rejectVisualPresentation,
    refreshCompletionCandidate
  } from './completionController';
  import { completionPresentation } from './completionSession';
  import type { InlineGhostSuggestion } from './inlineSuggestionFamily';
  import type { CompletionInsertionAction } from './suggestionInteraction';

  export let mode: 'visual' | 'source' = 'visual';
  export let loompad = false;
  export let insertsOnAccept = true;

  const initialFamily: InlineGhostSuggestion[] = [
    {
      candidateId: 'candidate-a',
      presentationKey: 'candidate-a:presentation',
      text: ' one two',
      runId: 'run-a',
      targetByte: 5,
      insertsOnAccept
    },
    {
      candidateId: 'candidate-b',
      presentationKey: 'candidate-b:presentation',
      text: ' another path',
      runId: 'run-b',
      targetByte: 5,
      insertsOnAccept
    },
    {
      candidateId: 'candidate-c',
      presentationKey: 'candidate-c:presentation',
      text: ' third road',
      runId: 'run-c',
      targetByte: 5,
      insertsOnAccept
    },
    {
      candidateId: 'candidate-d',
      presentationKey: 'candidate-d:presentation',
      text: ' final turn',
      runId: 'run-d',
      targetByte: 5,
      insertsOnAccept
    }
  ];
  let family: InlineGhostSuggestion[] = [];
  const contextKey = `browser-session:browser-document:1:${mode}`;
  let markdown = 'Hello';
  let visualEditor: LoomEditor;
  let sourceEditor: SourceEditor;
  let sourceTextarea: HTMLTextAreaElement;
  let ready = false;
  let invalidations = 0;
  let controller = initialCompletionControllerState();

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

  function unconsume(candidateId: string, presentationKey: string, text: string): boolean {
    const authorization = authorizeCompletionUnconsume(controller, {
      eligible: selected, candidateId, presentationKey, text, manuscriptText: markdown
    });
    controller = authorization.state;
    return authorization.authorized;
  }

  function rejectVisual(
    candidateId: string,
    presentationKey: string,
    surfaceKey: string,
    anchorByte: number
  ): void {
    controller = rejectVisualPresentation(controller, {
      mode,
      eligible: selected,
      candidateId,
      presentationKey,
      surfaceKey,
      currentSurfaceKey: contextKey,
      anchorByte
    });
  }

  export function installRefill(candidates: InlineGhostSuggestion[]): void {
    family = candidates;
    controller = reconcileCompletionController(controller, contextKey, family, true, loompad);
  }

  // Feed a transport update through the production live-refresh transition.
  // This harness tests editor/controller integration, not native inference.
  export function appendStream(runId: string, text: string, presentationKey: string): boolean {
    if (!controller.session) return false;
    const next = refreshCompletionCandidate(
      controller, controller.session, runId, text, presentationKey
    );
    const applied = next !== controller;
    controller = next;
    return applied;
  }

  function chooseLoompad(candidate: InlineGhostSuggestion): void {
    const active = controllerView.activeFamily;
    const target = active.findIndex(item => item.runId === candidate.runId);
    const current = active.findIndex(item => item.runId === selected?.runId);
    if (target >= 0 && current >= 0) controller = cycleCompletion(controller, active, target - current).state;
  }

  export function attemptLoompad(candidateId: string, presentationKey: string, text: string): boolean {
    return mode === 'visual'
      ? visualEditor.acceptLoompadText(candidateId, presentationKey, text)
      : sourceEditor.acceptLoompadText(candidateId, presentationKey, text);
  }

  async function acceptLoompad(candidate: InlineGhostSuggestion, length: LoompadLength): Promise<void> {
    chooseLoompad(candidate);
    await tick();
    const prefix = loompadPrefix(candidate.text, length, mode === 'visual');
    if (prefix) attemptLoompad(candidate.candidateId, candidate.presentationKey, prefix);
  }

  function observe(next: string): void {
    const mutation = observeTextMutation(controller, next, markdown, false);
    controller = mutation.state;
    if (next !== markdown && !mutation.completionOwned) family = [];
    markdown = next;
  }

  function invalidateIfManualMutation(): void {
    if (controller.pendingText !== null) return;
    invalidations += 1;
    family = [];
    controller = clearCompletionSession(controller);
  }

  function reportCaret(targetByte: number | null): void {
    if (controller.pendingText !== null || targetByte === null) return;
    const expected = controller.session
      ? completionPresentation(controller.session)?.targetByte ?? null
      : controllerView.selected?.targetByte ?? controller.generationIntent?.anchorByte ?? null;
    if (expected !== null && targetByte !== expected) {
      invalidations += 1;
      family = [];
      controller = clearCompletionSession(controller);
    }
  }

  function sourceInput(textarea: HTMLTextAreaElement): void {
    reportCaret(textarea.selectionStart);
    observe(textarea.value);
  }

  onMount(async () => {
    await tick();
    // Mounting reports the initial caret; Source focus also precedes its end
    // selection. Admit the family only after the real editor owns that caret.
    const editor = mode === 'visual' ? visualEditor : sourceEditor;
    if (!editor.focusAtDocumentEnd()) throw new Error('completion harness could not focus its editor');
    family = initialFamily;
    controller = reconcileCompletionController(controller, contextKey, family);
    ready = true;
  });
</script>

<main class="editor-stage" style="height:600px">
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
        ghostHidden={loompad}
        ghostAlternatives={controllerView.alternatives}
        ghostUnconsumeText={controllerView.unconsumeText}
        surfaceKey={contextKey}
        onChange={observe}
        onImmediateDocumentMutation={invalidateIfManualMutation}
        onSelectionChange={(targetByte) => reportCaret(targetByte)}
        onGhostInsert={insert}
        onGhostUnconsume={unconsume}
        onGhostPresentationRejected={rejectVisual}
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
        ghostHidden={loompad}
        ghostAlternatives={controllerView.alternatives}
        ghostUnconsumeText={controllerView.unconsumeText}
        onValueInput={sourceInput}
        onSelectionChange={(textarea) => reportCaret(textarea.selectionStart)}
        onGhostInsert={insert}
        onGhostUnconsume={unconsume}
      />
    </div>
  {/if}
  {#if loompad}
    <Loompad choices={controllerView.activeFamily} selectedRunId={selected?.runId ?? ''}
      scope={contextKey} visual={mode === 'visual'} onChoose={chooseLoompad}
      onAccept={(candidate, length) => void acceptLoompad(candidate, length)} />
  {/if}
  <output aria-label="Controller Session">{controllerView.boundSession ? 'bound' : 'none'}</output>
  <output aria-label="Controller Frozen">{controller.session?.authorityFrozen ? 'yes' : 'no'}</output>
  <output aria-label="Controller Actions">{controller.actionSequence}</output>
  <output aria-label="Controller Action Kind">{controller.lastAction?.kind ?? 'none'}</output>
  <output aria-label="Controller Markdown">{markdown}</output>
  <output aria-label="Controller Remainder">{selected?.text ?? 'none'}</output>
  <output aria-label="Controller Invalidations">{invalidations}</output>
</main>
