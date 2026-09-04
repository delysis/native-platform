<script lang="ts">
  import { onMount, tick } from 'svelte';
  import LoomEditor from './LoomEditor.svelte';
  import VisualFormatMenu from './VisualFormatMenu.svelte';
  import {
    completionPresentation,
    completionSessionContextKey,
    completionShouldRequestNextBatch,
    consumeCompletionText,
    cycleCompletionSession,
    insertAtUtf8Boundary,
    removeBeforeUtf8Boundary,
    selectedCompletionCandidate,
    startCompletionSession,
    synchronizeCompletionCandidates,
    unconsumeCompletionWord,
    type CompletionCandidate,
    type CompletionSession
  } from './completionSession';
  import type { VisualFormatState } from './visualFormatting';
  import type { VisualCaretBoundaryFailure } from './ghostText';
  import {
    unavailableVisualSelectionWitness,
    type VisualSelectionAccessibilityWitness
  } from './completionAccessibility';

  export let initialValue = 'alpha beta gamma';
  export let completionCandidates: CompletionCandidate[] = [];
  export let completionFrames: readonly (readonly CompletionCandidate[])[] = [];
  export let autocomplete = true;
  export let shuttle = false;
  export let acceptImageAttachments = true;
  export let onImageAttachments: (files: readonly File[]) => Promise<readonly string[]> =
    async () => [];
  export let onImageAttachmentsCommitted: (count: number) => void = () => {};
  export let onImageAttachmentError: (message: string) => void = () => {};
  export let resolveImageAssetUrl: (markdownPath: string) => string | null = () => null;

  let markdown = initialValue;
  let pendingMarkdown: string | null = null;
  let editor: LoomEditor;
  const completionContextKey = completionSessionContextKey(
    'browser-session',
    'browser-document',
    1,
    'visual'
  );
  let session: CompletionSession | null = completionCandidates.length > 0
    ? startCompletionSession(completionContextKey, completionCandidates, completionCandidates[0].runId)
    : null;
  let checkpointRevision = 1;
  let generationRequests = 0;
  let completionFrameIndex = 0;
  let exhaustionHandled = false;
  let completionReady = false;
  let lastFormattingResult = 'none';
  let formattingMenuConnected = true;
  let formattingMenuMounted = true;
  let attachmentCommitWitness = 'none';
  let selectionCallbackCount = 0;
  let nullSelectionCallbackCount = 0;
  let latestSelectionCallback = 'none';
  let selectionAccessibility: VisualSelectionAccessibilityWitness =
    unavailableVisualSelectionWitness();
  let formatting: VisualFormatState = {
    block: 'body',
    bold: false,
    italic: false,
    blockquote: false,
    bulletList: false,
    orderedList: false,
    linkHref: '',
    selectionEmpty: true
  };

  $: presentation = completionReady && pendingMarkdown === null && session
    ? session.acceptedChunks.length === 0
      ? selectedCompletionCandidate(session)
      : completionPresentation(session)
    : null;
  $: alternatives = session?.acceptedChunks.length === 0
    ? session.candidates.map((candidate) => ({
        candidateId: candidate.candidateId,
        presentationKey: candidate.presentationKey,
        text: candidate.text,
        runId: candidate.runId
      }))
    : presentation ? [{
        candidateId: presentation.candidateId,
        presentationKey: presentation.presentationKey,
        text: presentation.text,
        runId: presentation.runId
      }] : [];
  $: unconsumeText = session?.acceptedChunks.at(-1) ?? '';
  $: {
    const exhausted = Boolean(
      completionReady &&
      session &&
      completionShouldRequestNextBatch(session, pendingMarkdown !== null, true)
    );
    if (exhausted && !exhaustionHandled) {
      exhaustionHandled = true;
      generationRequests += 1;
    } else if (!exhausted) {
      exhaustionHandled = false;
    }
  }

  function change(next: string): void {
    markdown = next;
    if (pendingMarkdown === next) pendingMarkdown = null;
  }

  function immediateMutation(): void {
    if (pendingMarkdown === null) generationRequests += 1;
  }

  function caretNavigation(): void {
    if (!completionReady) return;
    generationRequests += 1;
    session = null;
    pendingMarkdown = null;
  }

  function selectionChanged(
    markdownByteOffset: number | null,
    failure: VisualCaretBoundaryFailure | 'selection_settling' | null,
    diagnostic: string | null
  ): void {
    selectionCallbackCount += 1;
    if (markdownByteOffset === null) nullSelectionCallbackCount += 1;
    latestSelectionCallback = markdownByteOffset === null
      ? `null:${failure ?? 'none'}:${diagnostic ?? 'none'}`
      : `${markdownByteOffset}:${failure ?? 'none'}`;
  }

  function acknowledgeImageAttachments(count: number): void {
    attachmentCommitWitness = `${count}:${markdown}`;
    onImageAttachmentsCommitted(count);
  }

  function insert(candidateId: string, presentationKey: string, text: string): boolean {
    if (
      !session ||
      !presentation ||
      presentation.candidateId !== candidateId ||
      presentation.presentationKey !== presentationKey
    ) return false;
    const step = consumeCompletionText(session, text);
    const next = insertAtUtf8Boundary(markdown, presentation.targetByte, text);
    if (!step || next === null) return false;
    session = step.session;
    pendingMarkdown = next;
    return true;
  }

  function unconsume(candidateId: string, presentationKey: string, text: string): boolean {
    if (
      !session ||
      !presentation ||
      presentation.candidateId !== candidateId ||
      presentation.presentationKey !== presentationKey
    ) return false;
    const step = unconsumeCompletionWord(session);
    const next = removeBeforeUtf8Boundary(markdown, presentation.targetByte, text);
    if (!step || step.text !== text || next === null) return false;
    session = step.session;
    pendingMarkdown = next;
    return true;
  }

  function cycle(offset: number): void {
    if (session) session = cycleCompletionSession(session, offset);
  }

  function dismiss(candidateId: string, presentationKey: string): void {
    if (
      presentation?.candidateId !== candidateId ||
      presentation.presentationKey !== presentationKey
    ) return;
    session = null;
    pendingMarkdown = null;
  }

  function simulateCheckpoint(): void {
    checkpointRevision += 1;
  }

  function advanceCompletionFrame(): void {
    const candidates = completionFrames[completionFrameIndex];
    if (!candidates) return;
    completionFrameIndex += 1;
    session = session
      ? synchronizeCompletionCandidates(session, candidates)
      : candidates.length > 0
        ? startCompletionSession(completionContextKey, candidates, candidates[0].runId)
        : null;
  }

  function advanceShuttle(): void {
    if (shuttle) editor.acceptGhostWord(false);
  }

  function replaceManuscriptExternally(): void {
    pendingMarkdown = null;
    markdown = 'External authority';
  }

  function applyBoldDirectly(): void {
    const applied = editor?.applyFormatting('bold') ?? false;
    const diagnostic = editor?.formattingDiagnostic() ?? 'editor_unavailable';
    lastFormattingResult = `bold:${applied ? 'applied' : 'refused'}:${diagnostic}`;
  }

  onMount(async () => {
    await tick();
    editor.focusAtDocumentEnd();
    completionReady = true;
  });
</script>

<main>
  {#if formattingMenuMounted}
    <VisualFormatMenu
      editor={formattingMenuConnected ? editor : null}
      {formatting}
      onCommandResult={(action, applied, diagnostic) => {
        lastFormattingResult = `${action}:${applied ? 'applied' : 'refused'}:${diagnostic}`;
      }}
    />
  {/if}
  <section class="editor-pane">
    <LoomEditor
      bind:this={editor}
      value={markdown}
      autofocus={true}
      ghostText={presentation?.text ?? ''}
      ghostCandidateId={presentation?.candidateId ?? ''}
      ghostPresentationKey={presentation?.presentationKey ?? ''}
      ghostAnchorByteOffset={presentation?.targetByte ?? null}
      ghostInsertsOnAccept={true}
      ghostAlternatives={alternatives}
      ghostHidden={shuttle || !autocomplete}
      ghostUnconsumeText={unconsumeText}
      surfaceKey="browser:surface"
      {acceptImageAttachments}
      {onImageAttachments}
      onImageAttachmentsCommitted={acknowledgeImageAttachments}
      {onImageAttachmentError}
      {resolveImageAssetUrl}
      onChange={change}
      onImmediateDocumentMutation={immediateMutation}
      onCaretNavigation={caretNavigation}
      onSelectionChange={selectionChanged}
      onSelectionAccessibilityChange={(witness) => selectionAccessibility = witness}
      onGhostInsert={insert}
      onGhostUnconsume={unconsume}
      onGhostCycle={cycle}
      onGhostDismiss={dismiss}
      onGhostPresentationRejected={() => {}}
      onFormatStateChange={(state) => formatting = state}
    />
  </section>
  <output aria-label="Serialized Markdown">{markdown}</output>
  <output aria-label="Generation Requests">{generationRequests}</output>
  <output aria-label="Completion Presentation">{presentation ? `${presentation.targetByte}:${presentation.presentationKey}:${presentation.text}` : 'none'}</output>
  <output aria-label="Completion Context">{session?.contextKey ?? 'none'}</output>
  <output aria-label="Checkpoint Revision">{checkpointRevision}</output>
  <output aria-label="Completion Stream Frame">{completionFrameIndex}</output>
  <output aria-label="Formatting Result">{lastFormattingResult}</output>
  <output aria-label="Attachment Commit Witness">{attachmentCommitWitness}</output>
  <output aria-label="Selection Callback Count">{selectionCallbackCount}</output>
  <output aria-label="Null Selection Callback Count">{nullSelectionCallbackCount}</output>
  <output aria-label="Latest Selection Callback">{latestSelectionCallback}</output>
  <output aria-label="Visual Selection Witness">{JSON.stringify(selectionAccessibility)}</output>
  <button type="button" on:mousedown|preventDefault on:click={simulateCheckpoint}>Simulate checkpoint</button>
  <button
    type="button"
    disabled={completionFrameIndex >= completionFrames.length}
    on:mousedown|preventDefault
    on:click={advanceCompletionFrame}
  >Advance completion stream</button>
  <button
    type="button"
    on:mousedown|preventDefault
    on:click={() => editor.reconcileCurrentSelection()}
  >Reconcile current selection</button>
  <button type="button" on:mousedown|preventDefault on:click={advanceShuttle}>Advance Shuttle</button>
  <button type="button" on:mousedown|preventDefault on:click={() => editor.insertTextAtSelection(' dictated')}>Insert transcript</button>
  <button type="button" on:click={replaceManuscriptExternally}>Replace manuscript externally</button>
  <button type="button" on:click={() => formattingMenuConnected = false}>Disconnect formatting editor</button>
  <button type="button" on:click={() => formattingMenuMounted = false}>Destroy formatting menu</button>
  <button type="button" on:click={applyBoldDirectly}>Invoke direct format command</button>
</main>
