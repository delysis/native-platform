<script lang="ts">
  import ChatMarkdown from './ChatMarkdown.svelte';
  import { presentChatText } from './chatPresentation';
  import type { TerminalRun } from './types';

  export let run: TerminalRun;
  export let output: string;
  export let pinned = false;
  export let onOpen: (() => void) | undefined = undefined;
  export let onPin: (() => void) | undefined = undefined;
  $: content = presentChatText(output);
</script>

<article class="chat-turn" aria-label="Conversation turn">
  {#if run.presentation?.input}
    <div class="query" role="group" aria-label="You">{run.presentation.input}</div>
  {/if}
  <div class="response" role="group" aria-label="Response">
    {#each run.events ?? [] as event}
      {#if event.detail}
        <details class="activity">
          <summary>{event.label}</summary>
          <p>{event.detail}</p>
        </details>
      {:else}
        <p class="activity event-note">{event.label}</p>
      {/if}
    {/each}
    {#each content.thoughts as thought, index}
      <details class="thinking">
        <summary>Thinking{content.thoughts.length > 1 ? ` ${index + 1}` : ''}{!thought.complete && run.status === 'running' ? '…' : ''}</summary>
        <div class="thought-text"><ChatMarkdown text={thought.text} /></div>
        {#if !thought.complete && run.status !== 'running'}<p class="interrupted">Thinking ended before a response.</p>{/if}
      </details>
    {/each}
    {#if content.answer}<div class="output"><ChatMarkdown text={content.answer} /></div>{/if}
    {#if run.status === 'running'}
      <p class="status" role="status"><span class="working-dot" aria-hidden="true"></span>Working…</p>
    {:else if run.status === 'cancelled'}
      <p class="status" role="status">Stopped</p>
    {:else if run.status === 'failed'}
      <p class="failure" role="status">Couldn’t finish{run.error ? `: ${run.error}` : '.'}</p>
    {/if}
    {#if onOpen || onPin}
      <div class="turn-actions">
        {#if onOpen}<button aria-label="Open output document" on:click={onOpen}>Open</button>{/if}
        {#if onPin}<button aria-label={pinned ? 'Unpin output from workspace' : 'Pin output in workspace'} aria-pressed={pinned} on:click={onPin}>{pinned ? 'Unpin' : 'Pin'}</button>{/if}
      </div>
    {/if}
  </div>
</article>

<style>
  .chat-turn { min-width:0; margin:0 0 20px; }
  .query { width:fit-content; max-width:90%; margin:0 0 16px auto; padding:9px 13px; border-radius:15px 15px 4px 15px; background:var(--chrome-hover); white-space:pre-wrap; overflow-wrap:anywhere; line-height:1.5; }
  .response { min-width:0; }
  .output { min-width:0; }
  .thinking,.activity { margin:0 0 10px; color:var(--muted); font-size:.86em; }
  summary { cursor:pointer; width:fit-content; max-width:100%; padding:4px 0; overflow-wrap:anywhere; }
  summary:hover { color:var(--ink); }
  summary:focus-visible { outline:2px solid var(--moss); outline-offset:3px; border-radius:3px; }
  .event-note { padding:4px 0; }
  .thought-text { max-height:240px; overflow:auto; margin:5px 0; padding:3px 0 3px 12px; border-left:2px solid var(--line-soft); }
  .activity p { margin:4px 0 4px 15px; white-space:pre-wrap; overflow-wrap:anywhere; }
  .interrupted { font-size:.9em; }
  .status,.failure { margin:8px 0; font-size:.85em; color:var(--muted); overflow-wrap:anywhere; }
  .failure { color:var(--danger); }
  .working-dot { display:inline-block; width:6px; height:6px; margin-right:7px; border-radius:50%; background:currentColor; }
  .turn-actions { display:flex; gap:10px; margin-top:5px; opacity:0; }
  .chat-turn:hover .turn-actions, .chat-turn:focus-within .turn-actions { opacity:1; }
  .turn-actions button { min-height:26px; padding:2px 0; border:0; background:none; color:var(--muted); cursor:pointer; font-size:.75rem; }
  .turn-actions button:hover { color:var(--ink); }
  @media (hover:none) { .turn-actions { opacity:1; } }
</style>
