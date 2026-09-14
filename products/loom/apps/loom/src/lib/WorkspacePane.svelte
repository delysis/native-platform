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
  import SourceEditor from './SourceEditor.svelte';
  import { cancelTerminalRun, listTerminalRuns, normalizeFailure, runTerminal } from './ipc';
  import { canUseVisualMarkdown } from './markdownSafety';
  import { newUlid } from './ulid';
  import { RetainedOutputLoader } from './retainedOutput';
  import type { DocumentSummary, OpenDocument, TerminalRun, TerminalRunRequest } from './types';

  export let paneId: string;
  export let config: WorkspacePaneConfig;
  export let projectId: string;
  export let sessionId: string;
  export let documents: DocumentSummary[] = [];
  export let source: OpenDocument | null = null;
  export let value = '';
  export let readonly = false;
  export let modelLabel = '';
  export let onModelSelect: ((trigger: HTMLElement) => void) | null = null;
  export let onCompositionChange: (active: boolean) => void = () => {};
  export let onChange: (value: string) => void = () => {};
  export let beforeRun: () => Promise<OpenDocument | null>;
  export let onOpenDocument: (id: string) => void;
  export let onRunsChanged: () => void = () => {};
  export let onBusyChange: (busy: boolean) => void = () => {};

  let composing = false;
  function setComposing(active: boolean): void { composing = active; onCompositionChange(active); }
  let mounted = false;
  let scope = '';
  let entry = '';
  let runs: TerminalRun[] = [];
  let error = '';
  let dispatching = false;
  let pending: TerminalRunRequest | null = null;
  let cancelRequested = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let refreshing = false;

  let visualSession = '';
  let outputText: Record<string, string> = {};
  let outputSerial = 0;
  const outputLoader = new RetainedOutputLoader();
  const MAX_PROMPT_BYTES = 64 * 1024;
  let editor: LoomEditor | undefined;
  let historyViewport: HTMLDivElement | undefined;
  let following = true;
  afterUpdate(() => { if (following && historyViewport) historyViewport.scrollTop = historyViewport.scrollHeight; });
  function trackScroll(): void {
    if (historyViewport) following = historyViewport.scrollHeight - historyViewport.scrollTop - historyViewport.clientHeight < 48;
  }
  $: nextScope = `${projectId}/${sessionId}/${paneId}`;
  $: if (mounted && scope !== nextScope) resetScope(nextScope);
  $: busy = dispatching || pending !== null || runs.some((run) => run.status === 'running');
  $: reportBusy(busy);
  $: target = resolveDocument(config.document, documents, source);
  $: recentRuns = runs.slice(-8);
  $: if (mounted && config.kind === 'chat') void hydrateOutputs(scope, recentRuns, documents);
  $: editingCurrent = !config.document || target?.document_id === source?.summary.document_id;
  $: editorKey = `${scope}/${source?.summary.document_id ?? ''}`;
  $: visual = selectVisual(value, editorKey);
  $: preview = mounted && config.kind === 'browser' ? previewUrl(projectId, sessionId, target, runs, documents) : '';

  export function flush(): boolean { return !composing && (editor?.flushPending() ?? true); }

  function reportBusy(value: boolean): void { onBusyChange(value); }
  function selectVisual(text: string, key: string): boolean {
    const admitted = canUseVisualMarkdown(text, visualSession === key);
    if (admitted) visualSession = key;
    return admitted;
  }
  function resolveDocument(reference: string | null, summaries: DocumentSummary[], current: OpenDocument | null): DocumentSummary | null {
    if (!reference) return null;
    if (reference === '@document') return current?.summary ?? null;
    let name = reference.replace(/^@/, '');
    if (name.startsWith('"')) { try { name = JSON.parse(name); } catch { return null; } }
    const exact = summaries.find((doc) => doc.document_id === name || doc.relative_path === name);
    if (exact) return exact;
    const matches = summaries.filter((doc) => doc.title === name);
    return matches.length === 1 ? matches[0] : null;
  }
  function resetScope(next: string): void {
    if (timer) clearTimeout(timer);
    scope = next; following = true;
    runs = []; entry = ''; error = ''; pending = null; dispatching = false;
    refreshing = false; cancelRequested = false;
    outputText = {}; outputLoader.clear(); outputSerial += 1;
    void refresh(next);
  }
  async function refresh(expected = scope): Promise<void> {
    if (!mounted || expected !== scope || refreshing) return;
    refreshing = true;
    const project = projectId, session = sessionId;
    try {
      const all = await listTerminalRuns(project, session);
      if (!mounted || expected !== scope) return;
      const previous = runs.map((run) => `${run.run_id}/${run.status}`).join();
      runs = all.filter((run) => run.presentation?.pane_id === paneId).sort((a, b) => a.created_at_ms - b.created_at_ms);
      if (pending && runs.some((run) => run.run_id === pending?.commandId)) pending = null;
      if (cancelRequested) {
        for (const run of runs.filter((run) => run.status === 'running')) await cancelTerminalRun(project, session, run.run_id);
      }
      if (previous !== runs.map((run) => `${run.run_id}/${run.status}`).join()) onRunsChanged();
    } catch (failure) {
      if (mounted && expected === scope) error = normalizeFailure(failure).message;
    } finally {
      if (mounted && expected === scope) {
        refreshing = false;
        if (timer) clearTimeout(timer);
        if (runs.some((run) => run.status === 'running')) timer = setTimeout(() => void refresh(expected), 800);
      }
    }
  }
  function readOutput(run: TerminalRun, summaries: DocumentSummary[]): Promise<string> {
    return outputLoader.read(projectId, sessionId, run, summaries);
  }
  async function hydrateOutputs(expected: string, history: TerminalRun[], summaries: DocumentSummary[]): Promise<void> {
    const serial = ++outputSerial;
    const texts: Record<string, string> = {};
    for (const run of history) {
      if (!run.output_document_id || !summaries.some((doc) => doc.document_id === run.output_document_id)) continue;
      try { texts[run.run_id] = await readOutput(run, summaries); }
      catch (failure) { if (mounted && expected === scope && serial === outputSerial) error = normalizeFailure(failure).message; }
      if (!mounted || expected !== scope || serial !== outputSerial) return;
    }
    if (mounted && expected === scope && serial === outputSerial) outputText = texts;
  }
  function referenceName(reference: string, current: OpenDocument): string {
    if (reference === '@document') return current.summary.relative_path;
    const name = reference.replace(/^@/, '');
    return name.startsWith('"') ? JSON.parse(name) : name;
  }
  async function prompt(input: string, current: OpenDocument): Promise<{ expression: string; contextReferences?: string[] }> {
    let expression = input;
    let contextReferences: string[] | undefined;
    if (config.kind !== 'terminal') {
      contextReferences = [...new Set([...config.context, ...(config.document ? [config.document] : [])]
        .map((name) => referenceName(name, current)))];
      const history: string[] = [];
      if (config.kind === 'chat') {
        for (const run of runs.filter((run) => run.status === 'completed').slice(-8)) {
          const output = await readOutput(run, documents);
          history.push(`User: ${run.presentation?.input ?? ''}\nAssistant: ${output}`);
        }
      }
      const task = config.kind === 'browser' ? `Write a complete static HTML page for: ${input}\nHTML:\n` : `User: ${input}\nAssistant:`;
      expression = [...history, task].join('\n\n');
    }
    if (new TextEncoder().encode(expression).length > MAX_PROMPT_BYTES) throw new Error('The conversation exceeds the 64 KiB prompt limit.');
    return { expression, ...(contextReferences ? { contextReferences } : {}) };
  }
  async function submit(): Promise<void> {
    if (busy || composing || readonly || !entry.trim()) return;
    const expected = scope, input = entry;
    dispatching = true; error = ''; cancelRequested = false;
    try {
      const current = await beforeRun();
      if (!mounted || expected !== scope || cancelRequested) return;
      if (!current?.summary.revision_id) throw new Error('Open and save a document first.');
      const preparedPrompt = await prompt(input, current);
      if (!mounted || expected !== scope || cancelRequested) return;
      const request: TerminalRunRequest = {
        projectId, sessionId, commandId: newUlid(), documentId: current.summary.document_id,
        sourceRevisionId: current.summary.revision_id, expectedVisibleBlobId: current.visible_blob_id,
        sourceStartByte: 0, sourceEndByte: 0, ...preparedPrompt,
        presentation: { pane_id: paneId, input }
      };
      pending = request;
      const run = await runTerminal(request);
      if (!mounted || expected !== scope) return;
      if (run.run_id !== request.commandId) throw new Error('The retained result belongs to a different run.');
      pending = null; entry = '';
      runs = [...runs.filter((item) => item.run_id !== run.run_id), run];
      onRunsChanged();
      await refresh(expected);
    } catch (failure) {
      if (mounted && expected === scope) {
        error = normalizeFailure(failure).message;
        if (failure && typeof failure === 'object' && 'code' in failure) pending = null;
        await refresh(expected);
      }
    } finally { if (mounted && expected === scope) dispatching = false; }
  }
  async function stop(): Promise<void> {
    cancelRequested = true;
    const expected = scope;
    try {
      const ids = new Set(runs.filter((run) => run.status === 'running').map((run) => run.run_id));
      if (pending) ids.add(pending.commandId);
      for (const id of ids) await cancelTerminalRun(projectId, sessionId, id);
      await refresh(expected);
    } catch (failure) { if (mounted && expected === scope) error = normalizeFailure(failure).message; }
  }
  function previewUrl(project: string, session: string, chosen: DocumentSummary | null, history: TerminalRun[], summaries: DocumentSummary[]): string {
    const latest = history.filter((run) => run.status === 'completed' && run.output_document_id).at(-1);
    const summary = summaries.find((doc) => doc.document_id === latest?.output_document_id) ?? chosen;
    if (!project || !session || !summary?.revision_id || !summary.active_blob_id) return '';
    return convertFileSrc(`v1-${project}-${session}-${summary.document_id}-${summary.revision_id}-${summary.active_blob_id}`, 'loom-preview');
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
<section class="workspace-pane" class:browser={config.kind === 'browser'} aria-label={config.title ?? config.kind} aria-busy={busy}>
  {#if config.title}<header>{config.title}</header>{/if}
  {#if error && config.kind !== 'terminal'}<p class="error" role="alert">{error}{#if pending}<button on:click={() => void refresh()}>Check result</button>{/if}</p>{/if}
  {#if config.kind === 'editor'}
    {#if source && editingCurrent}
      <div class="editor">
        {#key editorKey}
          {#if visual}
            <LoomEditor bind:this={editor} {value} {readonly} {onChange} onCompositionChange={setComposing} acceptImageAttachments={false} label={config.title ?? 'Pane editor'} onGhostPresentationRejected={() => {}} />
          {:else}
            <SourceEditor element={undefined} {value} {readonly} label={config.title ?? 'Pane editor'} onValueInput={(area) => onChange(area.value)} onCompositionStart={() => setComposing(true)} onCompositionEnd={() => setComposing(false)} />
          {/if}
        {/key}
      </div>
    {:else if target}<button on:click={() => onOpenDocument(target.document_id)}>Open {target.title}</button>
    {:else}<p class="empty">{config.document ? 'Document unavailable' : 'Open a document'}</p>{/if}
  {:else if config.kind === 'terminal'}
    <TerminalPane embedded open={true} bind:entry {projectId} {sessionId} {documents} {runs} {busy} {error} {modelLabel} disabled={readonly} runDisabled={!entry.trim()} uncertain={pending !== null} onCheck={() => void refresh()} onRun={() => void submit()} onCancel={() => void stop()} onOpen={(run) => run.output_document_id && onOpenDocument(run.output_document_id)} onClose={() => {}} />
  {:else}
    {#if config.kind === 'browser'}
      {#if preview}<iframe title={config.title ?? 'Page preview'} sandbox="" referrerpolicy="no-referrer" src={preview}></iframe>{/if}
    {:else}
      <div class="history" aria-label="Retained conversation" bind:this={historyViewport} on:scroll={trackScroll}>
        {#each recentRuns as run (run.run_id)}
          <article>
            {#if run.presentation?.input}<p class="input">{run.presentation.input}</p>{/if}
            {#if outputText[run.run_id] !== undefined || run.preview}<div class="output">{outputText[run.run_id] ?? run.preview}</div>{/if}
            {#if run.output_document_id}<button class="output-link" aria-label="Open output document" on:click={() => run.output_document_id && onOpenDocument(run.output_document_id)}>Open</button>{/if}
            {#if run.error}<p class="error">{run.error}</p>{/if}
            {#if run.status !== 'completed'}<small role="status">{run.status === 'running' ? 'Thinking…' : run.status}</small>{/if}
          </article>
        {/each}
      </div>
    {/if}
    <form on:submit|preventDefault={() => void submit()}>
      <textarea bind:value={entry} rows="1" aria-label={config.kind === 'chat' ? 'Message' : 'Page description'} on:keydown={keydown} on:compositionstart={() => setComposing(true)} on:compositionend={() => setComposing(false)} disabled={readonly}></textarea>
      <div class="composer-actions">
      {#if onModelSelect}<button class="model" type="button" title={modelLabel || 'Choose model'} aria-label={modelLabel ? `Model: ${modelLabel}` : 'Choose model'} disabled={readonly || busy} on:click={(event) => onModelSelect?.(event.currentTarget)}>{modelLabel || 'Model'}</button>{/if}
      {#if busy}<button class="send" type="button" aria-label="Stop" on:click={() => void stop()} disabled={readonly}>■</button>
      {:else}<button class="send" type="submit" aria-label={config.kind === 'chat' ? 'Send' : 'Run'} disabled={readonly || composing || !entry.trim()}>↑</button>{/if}
      </div>
    </form>
  {/if}
</section>
{/if}

<style>
  .workspace-pane { display:flex; flex-direction:column; min-width:0; min-height:0; height:100%; overflow:hidden; color:inherit; }
  header { font-size:.8rem; font-weight:600; padding:6px 9px; border-bottom:1px solid #8883; }
  .history,.editor { min-height:0; flex:1; overflow:auto; }
  .history { padding:6px 9px; }
  article { margin-bottom:12px; }
  p { margin:4px 0; white-space:pre-wrap; overflow-wrap:anywhere; }
  .input { font-weight:600; }
  .output { display:block; padding:0; width:100%; border:0; background:none; color:inherit; text-align:left; white-space:pre-wrap; overflow-wrap:anywhere; font:inherit; line-height:1.45; }
  .output-link { opacity:0; border:0; background:none; color:inherit; padding:0; font-size:.75rem; }
  article:hover .output-link, article:focus-within .output-link { opacity:.65; }
  form { display:flex; flex-direction:column; gap:0; padding:4px; border-top:1px solid #8883; }
  .composer-actions { display:flex; align-items:center; gap:5px; }
  .composer-actions button { min-height:26px; padding:2px 6px; }
  .send { margin-left:auto; width:28px; }
  textarea { width:100%; box-sizing:border-box; min-width:0; min-height:28px; max-height:120px; padding:5px; resize:vertical; background:transparent; color:inherit; font:inherit; border:1px solid #8884; border-radius:4px; }
  button { min-height:30px; padding:4px 8px; cursor:pointer; }
  .model { max-width:110px; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; font-size:.75rem; }
  .error { color:#b64238; font-size:.8rem; padding:4px 8px; }
  small,.empty { opacity:.65; }
  iframe { flex:1; width:100%; min-height:120px; border:0; background:white; }
</style>
