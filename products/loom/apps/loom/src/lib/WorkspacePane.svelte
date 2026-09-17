<script context="module" lang="ts">
  export interface WorkspacePaneConfig {
    kind: 'editor' | 'chat' | 'terminal' | 'browser';
    position: 'main' | 'right' | 'bottom';
    visible: boolean;
    title: string | null;
    document: string | null;
    context: string[];
  }
</script>

<script lang="ts">
  import { afterUpdate, onMount } from 'svelte';
  import { convertFileSrc } from '@tauri-apps/api/core';
  import LoomEditor from './LoomEditor.svelte';
  import TerminalPane from './TerminalPane.svelte';
  import ChatTurn from './ChatTurn.svelte';
  import SourceEditor from './SourceEditor.svelte';
  import { addDocumentContexts, importAttachmentPaths, bindAttachmentMaterial, addLibraryMaterialPath, cancelWorkspacePaneRun, listWorkspacePaneRuns, readWorkspacePaneOutput, resolveWorkspaceDocument, normalizeFailure, runWorkspacePane } from './ipc';
  import { importedMaterialMarkdown, materialReferenceMarkdown, isDatabasePath } from './materials';
  import { decodeVerseForEditor as decodeSourceForEditor, encodeVerseFromEditor as encodeSourceFromEditor } from './verseCodec';
  import { canUseVisualMarkdown } from './markdownSafety';
  import { newUlid } from './ulid';
  import { createWorkspacePaneDraft, type WorkspacePaneDraftStore, type WorkspacePaneSubmission, type WorkspacePaneRunRequest, type WorkspacePanePreparation } from './workspacePaneDrafts';
  import type { DocumentContextSnapshot, OpenDocument, TerminalRun } from './types';

  export let paneId: string;
  export let draft: WorkspacePaneDraftStore = createWorkspacePaneDraft();
  export let config: WorkspacePaneConfig;
  export let projectId: string;
  export let sessionId: string;
  export let workspaceScope: import('./materialEvidenceScope').MaterialSourceScope | null = null;
  export let configurationRevisionId: string | null = null;
  export let ownerActive = false;
  export let outputRevision = 0;
  export let materialOwnerScope: import('./materialEvidenceScope').MaterialSourceScope | null = null;
  export let source: OpenDocument | null = null;
  export let value = '';
  export let referenceScope: import('./referenceDiagnostics').ReferenceScope | null = null;
  export let readonly = false;
  export let onCompositionChange: (active: boolean) => void = () => {};
  export let onChange: (value: string) => void = () => {};
  export let beforeRun: () => Promise<WorkspacePanePreparation | null>;
  export let onOpenDocument: (id: string) => void;
  export let onRunsChanged: () => void = () => {};
  export let beforeAttachmentImport: () => Promise<boolean> = async () => true;
  export let onContextChanged: (snapshot: DocumentContextSnapshot, project: string, session: string, document: string) => void = () => {};
  export let onBusyChange: (busy: boolean) => void = () => {};
  export let pinnedOutputs = new Set<string>();
  export let onPinOutput: ((documentId: string) => void) | undefined = undefined;
  export let onFocus: () => void = () => {};

  let composing = false;
  function setComposing(active: boolean): void { composing = active; onCompositionChange(active); }
  let mounted = false;
  let scope = '';
  let scopeSerial = 0;
  let activeDraft = draft;
  let runs: TerminalRun[] = [];
  let error = '';
  let dispatching = false;
  let importing = false;
  let cancelRequested = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let refreshing = false;
  let refreshedOutputRevision = -1;

  let visualSession = '';
  let outputText: Record<string, string> = {};
  let outputSerial = 0;
  let outputDocuments: Record<string, OpenDocument> = {};
  let configuredDocument: OpenDocument | null = null;
  let configuredDocumentKey = '';
  let configuredDocumentSerial = 0;
  const MAX_PROMPT_BYTES = 64 * 1024;
  let editor: LoomEditor | undefined;
  let sourceEditor: SourceEditor | undefined;
  let historyViewport: HTMLDivElement | undefined;
  let entryArea: HTMLTextAreaElement | undefined;
  let following = true;
  afterUpdate(() => { if (following && historyViewport) historyViewport.scrollTop = historyViewport.scrollHeight; });
  function trackScroll(): void {
    if (historyViewport) following = historyViewport.scrollHeight - historyViewport.scrollTop - historyViewport.clientHeight < 48;
  }
  $: nextScope = `${projectId}/${sessionId}/${workspaceScope?.projectId ?? ''}/${workspaceScope?.sessionId ?? ''}/${paneId}`;
  $: if (mounted && (scope !== nextScope || activeDraft !== draft)) resetScope(nextScope);
  $: if (mounted && !refreshing && refreshedOutputRevision !== outputRevision) void refresh();
  $: busy = importing || dispatching || $draft.pending !== null || runs.some((run) => run.status === 'running');
  $: reportBusy(busy);
  $: paneError = error || $draft.failure || ($draft.pending && !dispatching ? 'The previous submission has not been confirmed.' : '');
  $: nextConfiguredDocumentKey = `${workspaceScope?.projectId ?? ''}/${workspaceScope?.sessionId ?? ''}/${config.document ?? ''}/${configurationRevisionId ?? ''}/${outputRevision}`;
  $: if (mounted && configuredDocumentKey !== nextConfiguredDocumentKey) void loadConfiguredDocument(nextConfiguredDocumentKey);
  $: target = config.document === '@document' ? source?.summary ?? null : configuredDocument?.summary ?? null;
  $: recentRuns = runs;
  $: pendingRun = $draft.pending && !runs.some(run => run.run_id === $draft.pending?.command_id) ? {
    run_id: $draft.pending.command_id, presentation: { pane_id: $draft.pending.pane_id, input: $draft.pending.input }, status: 'running' as const,
    expression: $draft.pending.expression, output_document_id: null, output_relative_path: null,
    preview: '', error: null, created_at_ms: 0
  } : null;
  $: if (mounted && (config.kind === 'chat' || config.kind === 'browser')) void hydrateOutputs(scope, recentRuns, outputRevision);
  $: editingCurrent = !config.document || config.document === '@document' || (ownerActive && target?.document_id === source?.summary.document_id);
  $: editorKey = `${scope}/${source?.summary.document_id ?? ''}`;
  $: visual = selectVisual(value, editorKey);
  $: sourceDecoded = decodeSourceForEditor(value);
  $: preview = mounted && config.kind === 'browser' ? previewUrl(runs, outputDocuments, configuredDocument) : '';

  export function flush(): boolean { return !composing && (editor?.flushPending() ?? true); }

  export function focusInput(): void {
    if (config.kind === 'editor') { editor?.focusCurrentSelection(); sourceEditor?.focusCurrentSelection(); }
    else entryArea?.focus();
  }

  /** Keep the initiating pane and exact selection while a source takes focus. */
  export function captureReferenceInsertion(openedReference?: Element): ((markdown: string) => boolean) | null {
    if (!mounted || readonly || composing || busy || !source || !flush() || (config.kind === 'editor' && !editingCurrent)) return null;
    const captured = { scope, scopeSerial, draft, kind: config.kind, documentId: source.summary.document_id, value, entry: $draft.entry, visual,
      start: entryArea?.selectionStart ?? $draft.entry.length, end: entryArea?.selectionEnd ?? $draft.entry.length };
    const visualAnchor = visual && config.kind === 'editor' ? editor?.captureTextInsertionAnchor(openedReference) : null;
    const sourceAnchor = !visual && config.kind === 'editor' ? sourceEditor?.captureTextInsertionAnchor() : null;
    return markdown => {
      if (!mounted || readonly || composing || busy || scope !== captured.scope || scopeSerial !== captured.scopeSerial || draft !== captured.draft || config.kind !== captured.kind ||
          source?.summary.document_id !== captured.documentId || value !== captured.value || $draft.entry !== captured.entry || visual !== captured.visual) return false;
      if (captured.kind === 'editor') return Boolean(captured.visual
        ? visualAnchor && editor?.insertMarkdownAtAnchor(visualAnchor, markdown)
        : sourceAnchor && sourceEditor?.insertTextAtAnchor(sourceAnchor, markdown));
      const before = captured.entry.slice(0, captured.start), after = captured.entry.slice(captured.end);
      const next = before + markdown + after;
      if (new TextEncoder().encode(next).length > MAX_PROMPT_BYTES) return false;
      $draft.entry = next;
      return true;
    };
  }


  export async function importDroppedPaths(paths: string[], point: { x: number; y: number }): Promise<void> {
    if (!mounted || readonly || composing || busy || !source || !paths.length ||
        (config.kind === 'editor' && !editingCurrent)) return;
    if (!flush()) return;
    const captured = { scope, scopeSerial, draft, projectId, sessionId, documentId: source.summary.document_id,
      kind: config.kind, value, entry: $draft.entry, visual, materialOwnerScope };
    const visualAnchor = visual && config.kind === 'editor' ? editor?.captureAttachmentAnchor(point.x, point.y) : null;
    const sourceAnchor = !visual && config.kind === 'editor' ? sourceEditor?.captureTextInsertionAnchor() : null;
    importing = true; error = '';
    const current = () => mounted && scope === captured.scope && scopeSerial === captured.scopeSerial && draft === captured.draft && !readonly && !composing &&
      source?.summary.document_id === captured.documentId && config.kind === captured.kind &&
      materialOwnerScope?.projectId === captured.materialOwnerScope?.projectId && materialOwnerScope?.sessionId === captured.materialOwnerScope?.sessionId &&
      value === captured.value && $draft.entry === captured.entry && visual === captured.visual;
    try {
      if (!await beforeAttachmentImport()) return;
      if (!current()) throw new Error('The pane changed before import. Drop the files again at the intended location.');
      const files = paths.filter(path => !isDatabasePath(path));
      const libraryPaths = paths.filter(isDatabasePath);
      const owner = captured.materialOwnerScope;
      if (libraryPaths.length && !owner) throw new Error('The workspace is not ready for sources yet.');
      const libraries = owner ? await Promise.all(libraryPaths.map(path => addLibraryMaterialPath(owner.projectId, owner.sessionId, path))) : [];
      const report = files.length ? await importAttachmentPaths(captured.projectId, captured.sessionId, files) : { imported: [], references: [], failures: [] };
      const imported = report.imported;
      error = report.failures.map((item) => `${item.name}: ${item.message}`).join("\n");
      if (!imported.length && !libraries.length && !report.references?.length) return;
      if (!current()) throw new Error('The pane changed during import. The files are retained; drop them again at the intended location.');
      const materials = await Promise.all(imported.map(item => bindAttachmentMaterial(captured.projectId, captured.sessionId, item.id)));
      if (!current()) throw new Error('The pane changed. Your sources are retained in this workspace.');
      const markdown = [...libraries.map(materialReferenceMarkdown), ...(report.references ?? []), ...imported.map((item, index) => importedMaterialMarkdown(item, materials[index]))].join('\n\n');
      if (captured.kind === 'editor') {
        const before = sourceAnchor?.value.slice(0, sourceAnchor.start) ?? '';
        const after = sourceAnchor?.value.slice(sourceAnchor.end) ?? '';
        const prefix = before && !before.endsWith('\n\n') ? (before.endsWith('\n') ? '\n' : '\n\n') : '';
        const suffix = after && !after.startsWith('\n\n') ? (after.startsWith('\n') ? '\n' : '\n\n') : '';
        const inserted = captured.visual
          ? visualAnchor && editor?.insertMarkdownAtAnchor(visualAnchor, markdown)
          : sourceAnchor && sourceEditor?.insertTextAtAnchor(sourceAnchor, `${prefix}${markdown}${suffix}`);
        if (!inserted) throw new Error('The files are retained, but their content could not be inserted at this selection.');
      } else {
        const next = [captured.entry, markdown].filter(Boolean).join('\n\n');
        if (new TextEncoder().encode(next).length > MAX_PROMPT_BYTES) throw new Error('The imported text exceeds the 64 KiB input limit. The original files are retained.');
        // Terminal media comes from registered document context, not arbitrary
        // links pasted into a prompt. Bind exact retained identities first.
        const cards = imported.filter((item) => item.media_kinds.length > 0 || item.text_bytes === 0);
        if (cards.length) {
          const snapshot = await addDocumentContexts(captured.projectId, captured.sessionId,
            captured.documentId, cards.map((item) => item.id));
          onContextChanged(snapshot, captured.projectId, captured.sessionId, captured.documentId);
        }
        if (!current()) throw new Error('The pane changed during import. The files remain attached to their original document.');
        $draft.entry = next;
        onRunsChanged();
      }
    } catch (failure) {
      if (mounted && scope === captured.scope && scopeSerial === captured.scopeSerial && draft === captured.draft) error = normalizeFailure(failure).message;
    } finally { if (mounted && scope === captured.scope && scopeSerial === captured.scopeSerial && draft === captured.draft) importing = false; }
  }

  function reportBusy(value: boolean): void { onBusyChange(value); }
  function selectVisual(text: string, key: string): boolean {
    const admitted = canUseVisualMarkdown(text, visualSession === key);
    if (admitted) visualSession = key;
    return admitted;
  }
  async function loadConfiguredDocument(key: string): Promise<void> {
    configuredDocumentKey = key;
    const serial = ++configuredDocumentSerial, owner = workspaceScope;
    configuredDocument = null;
    if (!owner || !config.document || config.document === '@document') return;
    const current = captureScope();
    try {
      let reference = config.document.replace(/^@/, '');
      if (reference.startsWith('"')) reference = JSON.parse(reference);
      const opened = await resolveWorkspaceDocument(owner.projectId, owner.sessionId, reference);
      if (current() && serial === configuredDocumentSerial) configuredDocument = opened;
    } catch (failure) { if (current() && serial === configuredDocumentSerial) error = normalizeFailure(failure).message; }
  }
  function resetScope(next: string): void {
    if (timer) clearTimeout(timer);
    scope = next; configuredDocumentKey = ''; scopeSerial += 1; activeDraft = draft; following = true;
    runs = []; error = ''; dispatching = false; importing = false;
    refreshing = false; cancelRequested = false;
    outputText = {}; outputDocuments = {}; outputSerial += 1;
    void refresh(next);
  }
  function captureScope(): () => boolean {
    const expected = scope, serial = scopeSerial, captured = draft;
    return () => mounted && scope === expected && scopeSerial === serial && draft === captured;
  }
  function settleSubmission(retained: WorkspacePaneDraftStore, request: WorkspacePaneRunRequest, submission: WorkspacePaneSubmission): void {
    retained.update(current => {
      if (current.pending !== request) return current;
      const accepted = submission.status === 'accepted' && (submission.run.status === 'running' || submission.run.status === 'completed');
      return { entry: accepted && current.entry === request.input ? '' : current.entry, pending: null,
        failure: submission.status === 'rejected' ? submission.error.message : accepted ? null : submission.run.error ?? 'The submission did not complete. Your input is retained.' };
    });
  }
  function confirmSubmission(request: WorkspacePaneRunRequest, run: TerminalRun): void {
    if ($draft.pending !== request || run.run_id !== request.command_id) return;
    settleSubmission(draft, request, { status: 'accepted', run });
    error = '';
  }
  async function checkResult(): Promise<void> {
    const request = $draft.pending;
    if (!request) return;
    const current = captureScope();
    try {
      // Recovery reads the admitted owner request; it never starts a new run.
      const history = await listWorkspacePaneRuns(request.workspace_id, request.workspace_session_id);
      if (!current() || $draft.pending !== request) return;
      const run = history.find(item => item.run_id === request.command_id);
      if (run) {
        confirmSubmission(request, run);
        runs = [...runs.filter(item => item.run_id !== run.run_id), run];
        onRunsChanged();
        await refresh();
      } else error = '';
    } catch (failure) { if (current()) error = normalizeFailure(failure).message; }
  }
  async function refresh(expected = scope): Promise<void> {
    if (!mounted || expected !== scope || refreshing || !workspaceScope) return;
    refreshing = true;
    refreshedOutputRevision = outputRevision;
    const { projectId: project, sessionId: session } = workspaceScope;
    const current = captureScope();
    try {
      const all = await listWorkspacePaneRuns(project, session);
      if (!current()) return;
      const previous = runs.map((run) => `${run.run_id}/${run.status}`).join();
      runs = all.filter((run) => run.presentation?.pane_id === paneId).sort((a, b) => a.created_at_ms - b.created_at_ms);
      const request = $draft.pending;
      if (request?.workspace_id === project && request.workspace_session_id === session) {
        const run = runs.find(item => item.run_id === request.command_id);
        if (run) confirmSubmission(request, run);
      }
      if (cancelRequested) {
        for (const run of runs.filter((run) => run.status === 'running')) {
          if (!current()) return;
          await cancelWorkspacePaneRun(project, session, run.run_id);
        }
      }
      if (!current()) return;
      if (previous !== runs.map((run) => `${run.run_id}/${run.status}`).join()) onRunsChanged();
    } catch (failure) {
      if (current()) error = normalizeFailure(failure).message;
    } finally {
      if (current()) {
        refreshing = false;
        if (timer) clearTimeout(timer);
        if (runs.some((run) => run.status === 'running')) timer = setTimeout(() => void refresh(expected), 800);
      }
    }
  }
  async function readOutput(run: TerminalRun, owner = workspaceScope): Promise<OpenDocument> {
    if (!owner) throw new Error('The workspace is not ready.');
    const opened = await readWorkspacePaneOutput(owner.projectId, owner.sessionId, run.run_id);
    if (!opened) throw new Error('The retained document is not available yet.');
    return opened;
  }
  async function hydrateOutputs(_expected: string, history: TerminalRun[], _revision: number): Promise<void> {
    const serial = ++outputSerial, current = captureScope();
    const texts: Record<string, string> = {}, opened: Record<string, OpenDocument> = {};
    for (const run of history) {
      if (!run.output_document_id) continue;
      try {
        const document = await readOutput(run);
        texts[run.run_id] = document.text; opened[run.run_id] = document;
      } catch (failure) { if (current() && serial === outputSerial) error = normalizeFailure(failure).message; }
      if (!current() || serial !== outputSerial) return;
    }
    if (current() && serial === outputSerial) { outputText = texts; outputDocuments = opened; }
  }
  async function prompt(input: string, current: () => boolean): Promise<string> {
    const owner = workspaceScope;
    let expression = input;
    if (config.kind !== 'terminal') {
      const history: string[] = [];
      if (config.kind === 'chat') {
        for (const run of runs.filter((run) => run.status === 'completed').slice(-8)) {
          const output = (await readOutput(run, owner)).text;
          if (!current()) throw new Error('The pane changed before submission.');
          history.push(`User: ${run.presentation?.input ?? ''}\nAssistant: ${output}`);
        }
      }
      const task = config.kind === 'browser' ? `Write a complete static HTML page for: ${input}\nHTML:\n` : `User: ${input}\nAssistant:`;
      expression = [...history, task].join('\n\n');
    }
    if (new TextEncoder().encode(expression).length > MAX_PROMPT_BYTES) throw new Error('The conversation exceeds the 64 KiB prompt limit.');
    return expression;
  }
  async function submit(): Promise<void> {
    if (busy || composing || readonly || !workspaceScope || !configurationRevisionId || !$draft.entry.trim()) return;
    const expected = scope, input = $draft.entry, stillCurrent = captureScope();
    const owner = workspaceScope, revision = configurationRevisionId, retained = draft;
    const sourceScope = { projectId, sessionId };
    dispatching = true; error = ''; $draft.failure = null; cancelRequested = false;
    try {
      const prepared = await beforeRun();
      if (!stillCurrent() || cancelRequested) return;
      if (!prepared) return;
      const current = prepared.document;
      if (current && !current.summary.revision_id) throw new Error('Save the document before running.');
      const expression = await prompt(input, stillCurrent);
      if (!stillCurrent() || cancelRequested) return;
      const request: WorkspacePaneRunRequest = {
        workspace_id: owner.projectId, workspace_session_id: owner.sessionId,
        command_id: newUlid(), pane_id: paneId, configuration_revision_id: revision,
        expression, input,
        ...(current ? { captured_document: {
          project_id: sourceScope.projectId, session_id: sourceScope.sessionId,
          document_id: current.summary.document_id, revision_id: current.summary.revision_id!,
          visible_blob_id: current.visible_blob_id
        } } : {})
      };
      $draft.pending = request;
      const submission = await runWorkspacePane(request);
      if (submission.status === 'rejected') {
        settleSubmission(retained, request, submission);
        if (stillCurrent()) error = '';
        return;
      }
      const run = submission.run;
      if (run.run_id !== request.command_id) throw new Error('The retained result belongs to a different run.');
      settleSubmission(retained, request, submission);
      if (!stillCurrent()) return;
      error = run.status === 'running' || run.status === 'completed' ? '' : run.error ?? 'The submission did not complete. Your input is retained.';
      runs = [...runs.filter((item) => item.run_id !== run.run_id), run];
      onRunsChanged();
      await refresh(expected);
    } catch (failure) {
      if (stillCurrent()) {
        error = normalizeFailure(failure).message;
        // A typed persistence failure can follow admission too. Only a retained
        // receipt may settle this request; a thrown error never authorizes retry.
        await refresh(expected);
      }
    } finally { if (stillCurrent()) dispatching = false; }
  }
  async function stop(): Promise<void> {
    cancelRequested = true;
    const expected = scope, current = captureScope();
    if (!workspaceScope) return;
    const { projectId: project, sessionId: session } = workspaceScope;
    const request = $draft.pending;
    const ids = new Set(runs.filter(run => run.status === 'running').map(run => run.run_id));
    try {
      if (request) {
        await cancelWorkspacePaneRun(request.workspace_id, request.workspace_session_id, request.command_id);
        if (!current()) return;
        if (request.workspace_id === project && request.workspace_session_id === session) ids.delete(request.command_id);
      }
      for (const id of ids) {
        if (!current()) return;
        await cancelWorkspacePaneRun(project, session, id);
      }
      await refresh(expected);
    } catch (failure) { if (current()) error = normalizeFailure(failure).message; }
  }
  function previewUrl(history: TerminalRun[], outputs: Record<string, OpenDocument>, configured: OpenDocument | null): string {
    const latest = history.filter(run => run.status === 'completed' && run.output_document_id).at(-1);
    const output = latest ? outputs[latest.run_id] : null;
    const document = output ?? (config.document === '@document' ? source : configured);
    const selectedScope = output || config.document !== '@document' ? workspaceScope : { projectId, sessionId };
    const summary = document?.summary;
    if (!selectedScope || !summary?.revision_id || !summary.active_blob_id) return '';
    return convertFileSrc(`v1-${selectedScope.projectId}-${selectedScope.sessionId}-${summary.document_id}-${summary.revision_id}-${summary.active_blob_id}`, 'loom-preview');
  }
  function keydown(event: KeyboardEvent): void {
    if (event.isComposing) return;
    if (event.key === 'Enter' && ((event.metaKey || event.ctrlKey) || (config.kind === 'chat' && !event.shiftKey))) {
      event.preventDefault(); event.stopPropagation(); void submit();
    }
  }
  onMount(() => {
    mounted = true;
    return () => { mounted = false; if (timer) clearTimeout(timer); if (composing) setComposing(false); onBusyChange(false); };
  });
</script>

{#if config.visible}
<section data-workspace-pane={paneId} class="workspace-pane" class:browser={config.kind === 'browser'} aria-label={config.title ?? config.kind} aria-busy={busy} on:focusin={onFocus}>
  {#if paneError && config.kind !== 'terminal'}<p class="error" role="alert">{paneError}{#if $draft.pending}<button on:click={() => void checkResult()}>Check result</button>{/if}</p>{/if}
  {#if config.kind === 'editor'}
    {#if source && editingCurrent}
      <div class="editor">
        {#key editorKey}
          {#if visual}
            <LoomEditor {referenceScope} bind:this={editor} {value} {readonly} {onChange} onCompositionChange={setComposing} acceptImageAttachments={false} label={config.title ?? 'Pane editor'} onGhostPresentationRejected={() => {}} />
          {:else}
            <SourceEditor {referenceScope} bind:this={sourceEditor} element={undefined} value={sourceDecoded.display} readonly={readonly || !sourceDecoded.codec.editable} verseNewline={sourceDecoded.codec.newline} label={config.title ?? 'Pane editor'} onValueInput={(area) => onChange(encodeSourceFromEditor(area.value, sourceDecoded.codec))} onCompositionStart={() => setComposing(true)} onCompositionEnd={() => setComposing(false)} />
          {/if}
        {/key}
      </div>
    {:else if target}<button on:click={() => onOpenDocument(target.document_id)}>Open {target.title}</button>
    {:else}<p class="empty">{config.document ? 'Document unavailable' : 'Open a document'}</p>{/if}
  {:else if config.kind === 'terminal'}
    <TerminalPane embedded open={true} bind:entry={$draft.entry} projectId={workspaceScope?.projectId ?? ''} sessionId={workspaceScope?.sessionId ?? ''} documents={[]} {outputRevision} readOutput={(run) => readOutput(run).then(document => document.text)} {runs} {busy} error={paneError} disabled={readonly} runDisabled={!$draft.entry.trim()} uncertain={$draft.pending !== null} onCheck={() => void checkResult()} onRun={() => void submit()} onCancel={() => void stop()} pinnedOutputs={pinnedOutputs} onPin={onPinOutput ? (run) => run.output_document_id && onPinOutput?.(run.output_document_id) : undefined} onOpen={(run) => run.output_document_id && onOpenDocument(run.output_document_id)} onClose={() => {}} />
  {:else}
    {#if config.kind === 'browser'}
      {#if preview}<iframe title={config.title ?? 'Page preview'} sandbox="" referrerpolicy="no-referrer" src={preview}></iframe>{/if}
    {:else}
      <div class="history" aria-label="Retained conversation" bind:this={historyViewport} on:scroll={trackScroll}>
        {#each recentRuns as run (run.run_id)}
          <ChatTurn {run} output={outputText[run.run_id] ?? run.preview}
            pinned={Boolean(run.output_document_id && pinnedOutputs.has(run.output_document_id))}
            onOpen={run.output_document_id ? () => onOpenDocument(run.output_document_id!) : undefined}
            onPin={run.output_document_id && onPinOutput ? () => onPinOutput?.(run.output_document_id!) : undefined} />
        {/each}
        {#if pendingRun}<ChatTurn run={pendingRun} output="" />{/if}
      </div>
    {/if}
    <form on:submit|preventDefault={() => void submit()}>
      <textarea bind:this={entryArea} bind:value={$draft.entry} rows="1" aria-label={config.kind === 'chat' ? 'Message' : 'Page description'} on:keydown={keydown} on:compositionstart={() => setComposing(true)} on:compositionend={() => setComposing(false)} disabled={readonly}></textarea>
      {#if busy || config.kind === 'browser'}
      <div class="composer-actions">
      {#if busy}<button class="send" type="button" aria-label="Stop" on:click={() => void stop()} disabled={readonly}>■</button>
      {:else}<button class="send" type="submit" aria-label="Run" disabled={readonly || composing || !$draft.entry.trim()}>↑</button>{/if}
      </div>
      {/if}
    </form>
  {/if}
</section>
{/if}

<style>
  .workspace-pane { display:flex; flex-direction:column; min-width:0; min-height:0; height:100%; overflow:hidden; color:inherit; }
  .history,.editor { min-height:0; flex:1; overflow:auto; }
  .history { padding:12px 14px 0; font-size:14px; }
  .history :global(.chat-turn) { width:100%; max-width:780px; margin-left:auto; margin-right:auto; }
  form { display:flex; flex-direction:column; gap:0; padding:5px; border-top:1px solid var(--line-soft); }
  .composer-actions { display:flex; align-items:center; gap:5px; }
  .composer-actions button { min-height:26px; padding:2px 6px; }
  .send { margin-left:auto; width:28px; }
  textarea { width:100%; box-sizing:border-box; min-width:0; min-height:28px; max-height:120px; padding:7px 9px; resize:vertical; background:var(--paper-deep); color:inherit; font:inherit; border:1px solid var(--line-soft); border-radius:8px; }
  button { min-height:30px; padding:4px 8px; cursor:pointer; }
  .error { color:var(--danger); font-size:.8rem; padding:4px 8px; }
  .empty { opacity:.65; }
  iframe { flex:1; width:100%; min-height:120px; border:0; background:white; }
</style>
