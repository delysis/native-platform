<script lang="ts">
  import { afterUpdate, onMount } from 'svelte';
  import { chatSession, initializeChat, selectChat, newChat, updateChatDraft, sendChat, stopChat, attachChatFile, pasteChatSource, listConsultSources, openConsultSource, type Message, type Source } from './chatSession';
  import ChatTurn from './ChatTurn.svelte';
  let historyViewport: HTMLDivElement | undefined;
  let following = true;
  let composing = false;
  let sourceLoading = false;
  $: mention = /(?:^|\s)@([\w-]*)$/.exec($chatSession.draft);
  $: candidates = mention ? $chatSession.candidates.filter(candidate => candidate.handle.startsWith(mention![1])).slice(0, 8) : [];
  afterUpdate(() => { if (following && historyViewport) historyViewport.scrollTop = historyViewport.scrollHeight; });
  function trackScroll(): void { if (historyViewport) following = historyViewport.scrollHeight - historyViewport.scrollTop - historyViewport.clientHeight < 48; }
  function keydown(event: KeyboardEvent): void {
    if (event.key === 'Enter' && !event.shiftKey && !composing && !event.isComposing) { event.preventDefault(); void sendChat(); }
  }
  let sourceText = '';
  let sourceError = '';
  let sourceTitle = '';
  let sources: Source[] = [];
  let sourceInvocation = '';
  let sourceTarget = '';
  let sourceSerial = 0;
  let pasteOpen = false;
  let pasted = '';
  onMount(() => { void initializeChat(); });
  async function showSources(message: Message): Promise<void> {
    const attribution = message.attribution;
    if (!attribution) return;
    const serial = ++sourceSerial;
    sourceText = ''; sourceError = ''; sources = []; sourceLoading = true;
    sourceTitle = `Sources for ${attribution.label}`;
    sourceInvocation = attribution.invocation_id;
    sourceTarget = attribution.source_id;
    try {
      const retained = await listConsultSources(sourceInvocation, sourceTarget);
      if (serial === sourceSerial) sources = retained;
    } catch (error) { if (serial === sourceSerial) sourceError = String(error); }
    finally { if (serial === sourceSerial) sourceLoading = false; }
  }
  async function showSource(source: Source): Promise<void> {
    const serial = ++sourceSerial;
    sourceText = ''; sourceError = '';
    try {
      const preview = await openConsultSource(sourceInvocation, sourceTarget, source);
      if (serial === sourceSerial) sourceText = preview.canonical_text || 'This occurrence has no canonical text.';
    } catch (error) { if (serial === sourceSerial) sourceError = String(error); }
  }
</script>

<section class="workspace-pane chat-work-area" aria-label="Chat work area">
  {#if $chatSession.loading}<p class="status" role="status">Opening local chats…</p>{/if}
  {#if $chatSession.error}<p class="error" role="alert">{$chatSession.error}</p>{/if}
  {#if !$chatSession.ready && !$chatSession.loading}<button class="bare-button" type="button" on:click={() => void initializeChat()}>Open local chats</button>{/if}
  <div class="history" aria-label="Chat messages" bind:this={historyViewport} on:scroll={trackScroll}>
    {#each $chatSession.selected?.messages ?? [] as message (message.id)}
      <ChatTurn input={message.role === 'user' ? message.content : undefined} output={message.role === 'user' ? '' : message.content} label={message.attribution?.label ?? 'Response'}>
        {#if message.attribution}
          <div class="message-actions">
            <button class="bare-button" type="button" on:click={() => void showSources(message)}>Sources</button>
          </div>
        {/if}
      </ChatTurn>
    {/each}
    {#if $chatSession.pending?.conversation === $chatSession.selected?.id}
      {#each Object.entries($chatSession.pending?.text ?? {}) as [label, text]}
        <ChatTurn output={text} label={`Pending reply from ${label}`} />
      {/each}
      <p class="status" role="status">{$chatSession.pending?.stop ? 'Stopping…' : 'Working…'}</p>
    {/if}
  </div>
  {#if sourceTitle}
    <section class="sources" aria-label="Consultation sources">
      <header><span>{sourceTitle}</span><button class="bare-button" type="button" aria-label="Close sources" on:click={() => { sourceSerial++; sourceTitle = ''; }}>×</button></header>
      {#each sources as source}<button class="bare-button" type="button" on:click={() => void showSource(source)}>Open occurrence {source.representation.attachment_id.slice(0, 8)}</button>{/each}
      {#if sourceLoading}<p class="status">Loading sources…</p>{:else if !sources.length && !sourceError}<p class="status">No retained attachment sources.</p>{/if}
      {#if sourceError}<p class="error" role="alert">{sourceError}</p>{/if}
      {#if sourceText}<pre>{sourceText}</pre>{/if}
    </section>
  {/if}
  {#if $chatSession.selected}
    <form on:submit|preventDefault={() => { if (!composing) void sendChat(); }}>
      {#if candidates.length}
        <div class="mention-menu" aria-label="Consult a persona">
          {#each candidates as candidate (candidate.id)}<button class="bare-button" type="button" on:click={() => updateChatDraft($chatSession.draft.replace(/@[\w-]*$/, `@${candidate.handle} `))}>@{candidate.handle}<span>{candidate.label}</span></button>{/each}
        </div>
      {/if}
      <textarea rows="1" aria-label="Message" placeholder="Message" value={$chatSession.draft} on:input={(event) => updateChatDraft(event.currentTarget.value)} on:keydown={keydown} on:compositionstart={() => composing = true} on:compositionend={() => composing = false} disabled={!!$chatSession.pending}></textarea>
      <div class="composer-actions">
        <button class="bare-button" type="button" aria-label="Attach file" title="Attach file" disabled={!!$chatSession.pending} on:click={() => void attachChatFile()}><svg aria-hidden="true" viewBox="0 0 16 16"><path d="m5 9 5-5a2 2 0 0 1 3 3l-6 6a3 3 0 0 1-4-4l6-6" /></svg></button>
        <button class="bare-button" type="button" aria-label="Paste source" title="Paste source" aria-expanded={pasteOpen} disabled={!!$chatSession.pending} on:click={() => pasteOpen = !pasteOpen}><svg aria-hidden="true" viewBox="0 0 16 16"><path d="M6 3h7v10H6ZM3 10V2h7" /></svg></button>
        {#if $chatSession.attachments.length}<span class="status">{$chatSession.attachments.length} attached</span>{/if}
        {#if $chatSession.pending}<button class="bare-button send" type="button" aria-label="Stop" on:click={() => void stopChat()} disabled={$chatSession.pending.stop}>■</button>{:else}<button class="bare-button send" type="submit" aria-label="Send" disabled={composing || !$chatSession.draft.trim()}>↑</button>{/if}
      </div>
      {#if pasteOpen}<textarea aria-label="Source text" bind:value={pasted}></textarea><button class="bare-button" type="button" disabled={!pasted.trim()} on:click={async () => { await pasteChatSource(pasted); pasted = ''; pasteOpen = false; }}>Attach text</button>{/if}
    </form>
  {:else if $chatSession.ready}
    <button class="bare-button" type="button" on:click={() => void newChat()}>New chat</button>
  {/if}
</section>

<style>
  .workspace-pane { display:flex; flex-direction:column; min-width:0; min-height:0; height:100%; overflow:hidden; color:inherit; }
  .history { min-height:0; flex:1; overflow:auto; padding:12px 14px 0; font-size:14px; }
  .history :global(.chat-turn) { width:100%; max-width:780px; margin-left:auto; margin-right:auto; }
  form { position:relative; display:flex; flex-direction:column; gap:0; padding:5px; border-top:1px solid var(--line-soft); }
  .composer-actions { display:flex; align-items:center; gap:5px; }
  button { min-height:30px; padding:4px 8px; border-radius:6px; cursor:pointer; }
  .composer-actions button { min-height:30px; padding:2px 6px; }
  .send { margin-left:auto; width:30px; }
  svg { width:16px; height:16px; fill:none; stroke:currentColor; stroke-width:1.3; stroke-linecap:round; stroke-linejoin:round; }
  textarea { width:100%; box-sizing:border-box; min-width:0; min-height:28px; max-height:120px; padding:7px 9px; resize:vertical; background:var(--paper-deep); color:inherit; font:inherit; border:1px solid var(--line-soft); border-radius:8px; }
  .error { color:var(--danger); font-size:.8rem; padding:4px 8px; }
  .status { color:var(--muted); font-size:.8rem; padding:4px 8px; }
  .message-actions { opacity:0; } :global(.chat-turn:hover) .message-actions, :global(.chat-turn:focus-within) .message-actions { opacity:1; }
  .message-actions button { font-size:.75rem; padding:2px 0; color:var(--muted); }
  .mention-menu { position:absolute; bottom:100%; left:5px; right:5px; max-height:240px; overflow:auto; padding:4px; background:var(--paper); border:1px solid var(--line); border-radius:8px; box-shadow:var(--shadow); }
  .mention-menu button { display:flex; align-items:center; gap:12px; width:100%; text-align:left; } .mention-menu span { color:var(--muted); font-size:.8em; }
  .sources { max-height:35%; overflow:auto; padding:8px 14px; border-top:1px solid var(--line-soft); font-size:13px; }
  .sources header { display:flex; align-items:center; justify-content:space-between; gap:8px; } pre { white-space:pre-wrap; overflow-wrap:anywhere; }
  @media (hover:none) { .message-actions { opacity:1; } }
</style>
