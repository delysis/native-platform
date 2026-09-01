<script lang="ts">
  import { onMount, tick } from 'svelte';
  import SourceEditor from './SourceEditor.svelte';
  import {
    completionPresentation,
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

  export let initialValue = 'hello';
  export let completionCandidates: CompletionCandidate[] = [];
  export let onImageAttachments: (files: readonly File[]) => Promise<readonly string[]> =
    async () => [];
  export let onImageAttachmentsCommitted: (count: number) => void = () => {};
  export let onImageAttachmentError: (message: string) => void = () => {};

  let markdown = initialValue;
  let pendingMarkdown: string | null = null;
  let editor: SourceEditor;
  let textarea: HTMLTextAreaElement;
  let session: CompletionSession | null = completionCandidates.length > 0
    ? startCompletionSession(
        'browser-session:browser-document:1:source',
        completionCandidates,
        completionCandidates[0].runId
      )
    : null;
  let ready = false;
  let generationRequests = 0;
  let rerenderSerial = 1;
  let attachmentCommitWitness = 'none';

  $: presentation = ready && session
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

  function input(next: HTMLTextAreaElement): void {
    const completionMutation = pendingMarkdown === next.value;
    pendingMarkdown = null;
    markdown = next.value;
    if (!completionMutation) {
      session = null;
      generationRequests += 1;
    }
  }

  function cycle(offset: number): void {
    if (session) session = cycleCompletionSession(session, offset);
  }

  function acknowledgeImageAttachments(count: number): void {
    attachmentCommitWitness = `${count}:${markdown}`;
    onImageAttachmentsCommitted(count);
  }

  onMount(async () => {
    await tick();
    editor.focusAtDocumentEnd();
    ready = true;
  });
</script>

<main>
  <div class="editor-pane source-pane" aria-label="Source editor pane">
    <SourceEditor
      bind:this={editor}
      bind:element={textarea}
      value={markdown}
      surfaceKey="browser-session:browser-document:1:source"
      ghostText={presentation?.text ?? ''}
      ghostCandidateId={presentation?.candidateId ?? ''}
      ghostPresentationKey={presentation?.presentationKey ?? ''}
      ghostInsertsOnAccept={true}
      ghostAlternatives={alternatives}
      ghostUnconsumeText={unconsumeText}
      {onImageAttachments}
      onImageAttachmentsCommitted={acknowledgeImageAttachments}
      {onImageAttachmentError}
      onValueInput={input}
      onGhostInsert={insert}
      onGhostUnconsume={unconsume}
      onGhostCycle={cycle}
    />
  </div>
  <output aria-label="Source Markdown">{markdown}</output>
  <output aria-label="Source Generation Requests">{generationRequests}</output>
  <output aria-label="Source Rerender Serial">{rerenderSerial}</output>
  <output aria-label="Source Attachment Commit Witness">{attachmentCommitWitness}</output>
  <button type="button" on:mousedown|preventDefault on:click={() => rerenderSerial += 1}>Stable rerender</button>
</main>
