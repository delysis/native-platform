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
    unconsumeCompletionWord,
    type CompletionCandidate,
    type CompletionSession
  } from './completionSession';
  import type { VisualFormatState } from './visualFormatting';

  export let initialValue = 'alpha beta gamma';
  export let completionCandidates: CompletionCandidate[] = [];
  export let autocomplete = true;
  export let shuttle = false;

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
  let exhaustionHandled = false;
  let completionReady = false;
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
        text: candidate.text
      }))
    : presentation ? [{
        candidateId: presentation.candidateId,
        presentationKey: presentation.presentationKey,
        text: presentation.text
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
    if (completionReady) generationRequests += 1;
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

  function simulateCheckpoint(): void {
    checkpointRevision += 1;
  }

  function advanceShuttle(): void {
    if (shuttle) editor.acceptGhostWord(false);
  }

  onMount(async () => {
    await tick();
    editor.focusAtDocumentEnd();
    completionReady = true;
  });
</script>

<main>
  <VisualFormatMenu {editor} {formatting} />
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
    ghostHidden={!autocomplete}
    ghostUnconsumeText={unconsumeText}
    surfaceKey="browser:surface"
    onChange={change}
    onImmediateDocumentMutation={immediateMutation}
    onCaretNavigation={caretNavigation}
    onGhostInsert={insert}
    onGhostUnconsume={unconsume}
    onGhostCycle={cycle}
    onGhostPresentationRejected={() => {}}
    onFormatStateChange={(state) => formatting = state}
  />
  <output aria-label="Serialized Markdown">{markdown}</output>
  <output aria-label="Generation Requests">{generationRequests}</output>
  <output aria-label="Completion Presentation">{presentation ? `${presentation.targetByte}:${presentation.presentationKey}:${presentation.text}` : 'none'}</output>
  <output aria-label="Completion Context">{session?.contextKey ?? 'none'}</output>
  <output aria-label="Checkpoint Revision">{checkpointRevision}</output>
  <button type="button" on:mousedown|preventDefault on:click={simulateCheckpoint}>Simulate checkpoint</button>
  <button type="button" on:mousedown|preventDefault on:click={advanceShuttle}>Advance Shuttle</button>
</main>
