<script lang="ts">
  import { afterUpdate, onMount, tick } from 'svelte';
  import { normalizeFailure } from './ipc';
  import { RetainedOutputLoader } from './retainedOutput';
  import type { DocumentSummary, TerminalRun } from './types';
  export let projectId = '';
  export let sessionId = '';
  export let documents: DocumentSummary[] = [];
  export let entry = '';
  export let open = false;
  export let embedded = false;
  export let runDisabled = false;
  export let runs: TerminalRun[] = [];
  export let busy = false;
  export let disabled = false;
  export let error = '';
  export let uncertain = false;
  export let onCheck: () => void;
  export let modelLabel = '';
  export let onRecover: (run: TerminalRun, mode: 'check' | 'resume') => void = () => {};
  export let onCancelRun: (run: TerminalRun) => void = () => {};
  export let onRun: () => void;
  export let onCancel: () => void;
  export let onOpen: (run: TerminalRun) => void;
  export let onClose: () => void;
  let input: HTMLTextAreaElement | undefined;
  let viewport: HTMLDivElement | undefined;
  let following = true;
  let wasOpen = false;
  $: void focusOnOpen(open);
  afterUpdate(() => { if (open && following && viewport) viewport.scrollTop = viewport.scrollHeight; });
  async function focusOnOpen(visible: boolean): Promise<void> {
    const opening = visible && !wasOpen;
    wasOpen = visible;
    if (!opening) return;
    following = true;
    await tick();
    if (open) { input?.focus(); if (viewport) viewport.scrollTop = viewport.scrollHeight; }
  }
  function trackScroll(): void {
    if (viewport) following = viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight < 48;
  }
  let cleared = new Set<string>();
  let historyIndex = -1;
  let draft = '';
  let mounted = false;
  let outputSerial = 0;
  let outputScope = '';
  let outputs: Record<string, string> = {};
  let outputError = '';
  const outputLoader = new RetainedOutputLoader();
  $: if (mounted) void hydrate(projectId, sessionId, open, runs, documents);
  $: history = [...runs].sort((a, b) => a.created_at_ms - b.created_at_ms);
  $: commands = [...history].reverse().map(run => run.presentation?.input ?? run.expression).filter(Boolean);
  onMount(() => {
    mounted = true;
    const handleCommandKey = async (event: KeyboardEvent) => {
      if (disabled || event.defaultPrevented || event.isComposing || event.altKey || event.shiftKey || !(event.metaKey || event.ctrlKey) || event.code !== 'Backquote') return;
      event.preventDefault();
      if (open) { onClose(); return; }
      open = true;
      await tick();
      input?.focus();
    };
    if (!embedded) window.addEventListener('keydown', handleCommandKey);
    return () => { mounted = false; outputSerial++; outputLoader.clear(); window.removeEventListener('keydown', handleCommandKey); };
  });
  async function hydrate(project: string, session: string, visible: boolean, retained: TerminalRun[], summaries: DocumentSummary[]): Promise<void> {
    const serial = ++outputSerial;
    const scope = `${project}/${session}`;
    if (scope !== outputScope) {
      outputScope = scope; outputs = {}; outputError = ''; outputLoader.clear();
      cleared = new Set(); historyIndex = -1; draft = '';
    }
    if (!visible || !project || !session) return;
    const next: Record<string, string> = {};
    let failureMessage = '';
    for (const run of retained.slice(-64)) {
      if (!run.output_document_id || !summaries.some((document) => document.document_id === run.output_document_id)) continue;
      try { next[run.run_id] = await outputLoader.read(project, session, run, summaries); }
      catch (failure) { failureMessage = normalizeFailure(failure).message; }
      if (!mounted || serial !== outputSerial) return;
    }
    if (mounted && serial === outputSerial) { outputs = next; outputError = failureMessage; }
  }
  function handleKey(event: KeyboardEvent): void {
    if (event.isComposing) return;
    if (event.key === 'Enter' && !event.shiftKey && !event.altKey) {
      event.preventDefault(); event.stopPropagation();
      if (!disabled && !runDisabled && !busy) { historyIndex = -1; onRun(); }
    } else if (event.key === 'Escape' && !embedded) {
      event.preventDefault(); event.stopPropagation(); onClose();
    } else if (event.ctrlKey && event.key.toLowerCase() === 'c') {
      event.preventDefault(); if (busy) onCancel();
    } else if (event.ctrlKey && event.key.toLowerCase() === 'l') {
      event.preventDefault(); cleared = new Set(runs.map(run => run.run_id));
    } else if ((event.key === 'ArrowUp' || event.key === 'ArrowDown') && !entry.includes('\n')) {
      if (event.key === 'ArrowUp' && historyIndex + 1 < commands.length) {
        event.preventDefault(); if (historyIndex < 0) draft = entry; entry = commands[++historyIndex];
      } else if (event.key === 'ArrowDown' && historyIndex >= 0) {
        event.preventDefault(); historyIndex--; entry = historyIndex < 0 ? draft : commands[historyIndex];
      }
    }
  }
</script>

{#if open}
<section id={embedded ? undefined : "retained-terminal"} class="retained-terminal" class:embedded aria-label="Terminal">
  {#if !embedded}<header class="terminal-header"><span>Terminal</span><slot name="model"><span class="terminal-status" title={modelLabel}>{modelLabel}</span></slot><button class="terminal-close" type="button" aria-label="Close terminal" on:click={onClose}>×</button></header>{/if}
  <div class="terminal-scroll" bind:this={viewport} on:scroll={trackScroll}>
    {#each history.filter(run => !cleared.has(run.run_id)) as run (run.run_id)}
      <div class="terminal-history">
        <div class="terminal-command"><span aria-hidden="true">› </span>{run.presentation?.input ?? (run.expression || run.title || '')}</div>
        {#if outputs[run.run_id] !== undefined || run.preview}<pre>{outputs[run.run_id] ?? run.preview}</pre>{/if}
        {#if run.error}<p class="terminal-error">{run.error}</p>{/if}
        {#if run.remote}<small class="terminal-status">{run.remote.model.name} · {run.status === 'completed' ? 'peer result' : 'peer job'}</small>{/if}
        {#if run.status === 'unconfirmed'}
          <div class="terminal-recovery">
            <button type="button" on:click={() => onRecover(run, 'check')} disabled={disabled || busy}>Check</button>
            <button type="button" on:click={() => onRecover(run, 'resume')} disabled={disabled || busy}>Resume</button>
            <button type="button" on:click={() => onCancelRun(run)} disabled={disabled || busy}>Cancel remaining steps</button>
          </div>
        {/if}
        {#if run.status === 'running'}<span class="terminal-status" role="status">…</span>{/if}
        {#if run.output_document_id}<button type="button" class="terminal-output-link" on:click={() => onOpen(run)} disabled={disabled}>{run.output_relative_path ?? 'Open output'}</button>{/if}
      </div>
    {/each}
    {#if outputError}<p class="terminal-error" role="alert">{outputError}</p>{/if}
    {#if error}<p class="terminal-error" role="alert">{error}{#if uncertain}<button class="terminal-action" type="button" on:click={onCheck} disabled={disabled}>Check result</button>{/if}</p>{/if}
    <div class="terminal-command-row"><span aria-hidden="true">›</span><textarea bind:this={input} bind:value={entry} rows="1" aria-label="Command" spellcheck="false" disabled={disabled} on:keydown={handleKey}></textarea></div>
  </div>
</section>
{/if}

<style>
  .retained-terminal.embedded { flex:1; min-height:0; max-height:none; border:0; }
</style>
