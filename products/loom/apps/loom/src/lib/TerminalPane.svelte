<script lang="ts">
  import { onMount, tick } from 'svelte';
  import type { TerminalRun } from './types';

  export let entry = '';
  export let open = false;
  let showEntry = false;
  $: if (!open) showEntry = false;
  export let runDisabled = false;
  export let runs: TerminalRun[] = [];
  export let busy = false;
  export let disabled = false;
  export let error = '';
  export let uncertain = false;
  export let onCheck: () => void;
  export let modelLabel = '';
  export let onRun: () => void;
  export let onCancel: () => void;
  export let onOpen: (run: TerminalRun) => void;
  export let onClose: () => void;
  let input: HTMLTextAreaElement | undefined;

  onMount(() => {
    const handleCommandKey = async (event: KeyboardEvent): Promise<void> => {
      if (disabled || event.defaultPrevented || event.isComposing || event.altKey || event.shiftKey ||
        !(event.metaKey || event.ctrlKey) || event.code !== 'Backquote') return;
      event.preventDefault();
      if (open && showEntry) { onClose(); return; }
      open = true;
      showEntry = true;
      await tick();
      input?.focus();
    };
    window.addEventListener('keydown', handleCommandKey);
    return () => window.removeEventListener('keydown', handleCommandKey);
  });

  function handleKey(event: KeyboardEvent): void {
    if (event.isComposing) return;
    if ((event.metaKey || event.ctrlKey) && event.key === 'Enter') {
      event.preventDefault();
      event.stopPropagation();
      if (!disabled && !runDisabled && !busy) onRun();
    } else if (event.key === 'Escape') {
      event.preventDefault();
      event.stopPropagation();
      onClose();
    }
  }
</script>

{#if open}
<section id="retained-terminal" class="retained-terminal" class:empty={runs.length === 0} aria-label="Retained outputs">
  <div class="terminal-command-row">
    {#if showEntry}
    <span class="terminal-prompt" aria-hidden="true">❯</span>
    <textarea
      bind:this={input}
      bind:value={entry}
      rows="1"
      aria-label="Command"
      spellcheck="false"
      disabled={disabled}
      on:keydown={handleKey}
    ></textarea>
    {:else}<span class="terminal-heading">Results</span>{/if}
    {#if busy}
      <button class="terminal-action" type="button" on:click={onCancel} disabled={disabled} aria-label="Stop generating">Stop</button>
    {:else if showEntry}
      <button class="terminal-action" type="button" on:click={onRun} disabled={disabled || runDisabled} aria-label="Run command">Run</button>
    {/if}
    {#if busy || modelLabel}<span class="terminal-status" role="status" title={modelLabel}>{busy ? 'Running locally' : modelLabel}</span>{/if}
    <button class="terminal-close" type="button" aria-label="Close retained outputs" on:click={onClose}>×</button>
  </div>
  {#if error}<p class="terminal-error" role="alert">{error}
    {#if uncertain}<button class="terminal-action" type="button" on:click={onCheck} disabled={disabled}>Check result</button>{/if}
  </p>{/if}
  <div class="terminal-results" aria-label="Retained runs" aria-busy={busy}>
    {#each runs as run (run.run_id)}
      <div class="terminal-result">
        <button type="button" on:click={() => onOpen(run)} disabled={!run.output_document_id || disabled} title={run.output_relative_path ?? undefined}>
          <span>{run.title ?? 'Take'}</span>
        </button>
        <span class="terminal-run-status">{run.status === 'running' ? 'Running…' : run.status}</span>
        {#if run.preview}<button class="terminal-preview" type="button" on:click={() => onOpen(run)} disabled={!run.output_document_id || disabled}>{run.preview}</button>{/if}
        {#if run.error}<p class="terminal-run-error">{run.error}</p>{/if}
      </div>
    {/each}
  </div>
</section>
{/if}
