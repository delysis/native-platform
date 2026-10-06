<script lang="ts">
  import { onMount } from 'svelte';
  import { chatSession, initializeChat, selectChat, newChat, updateChatDraft, sendChat, stopChat, attachChatFile, pasteChatSource, listConsultSources, openConsultSource, type Message, type Source } from './chatSession';
  export let onDocuments: () => void;
  export let onTitlebarDrag: (event: MouseEvent) => void = () => {};
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
    sourceText = ''; sourceError = ''; sources = [];
    sourceTitle = `Sources for ${attribution.label}`;
    sourceInvocation = attribution.invocation_id;
    sourceTarget = attribution.source_id;
    try {
      const retained = await listConsultSources(sourceInvocation, sourceTarget);
      if (serial === sourceSerial) sources = retained;
    } catch (error) { if (serial === sourceSerial) sourceError = String(error); }
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

<section class="chat-work-area" aria-label="Chat work area">
  <aside aria-label="Chats">
    <header><button type="button" on:click={onDocuments}>Documents</button><button type="button" disabled={!$chatSession.ready} on:click={() => void newChat()}>New chat</button></header>
    <nav aria-label="Saved chats">
      {#each $chatSession.conversations as conversation (conversation.id)}
        <button type="button" class:selected={$chatSession.selected?.id === conversation.id} aria-current={$chatSession.selected?.id === conversation.id ? 'page' : undefined} on:click={() => void selectChat(conversation.id)}>{conversation.title}</button>
      {/each}
    </nav>
  </aside>
  <main>
    <header class="chat-titlebar" role="presentation" on:mousedown={onTitlebarDrag}><h1>{$chatSession.selected?.title ?? 'Chat'}</h1>{#if $chatSession.loading}<span role="status">Opening local chats…</span>{/if}</header>
    {#if $chatSession.error}<p class="error" role="alert">{$chatSession.error}</p>{/if}
    {#if !$chatSession.ready && !$chatSession.loading}<button type="button" on:click={() => void initializeChat()}>Open local chats</button>{/if}
    <div class="messages" aria-label="Chat messages">
      {#each $chatSession.selected?.messages ?? [] as message (message.id)}
        <article aria-label={message.attribution?.label ?? message.role}>
          <strong>{message.attribution?.label ?? message.role}</strong>
          <div class="message-content">{message.content}</div>
          {#if message.attribution}
            <button type="button" on:click={() => void showSources(message)}>Sources</button>
            {#each $chatSession.receipts[message.attribution.invocation_id] ?? [] as receipt}
              {#if receipt.target_id === message.attribution.source_id}
                <small>{receipt.fake_fixture ? 'Fixture output' : receipt.real_engine_invoked ? 'Local model' : 'No model execution'}{receipt.cache_id ? receipt.cache_reused ? ' · cached prefix reused' : ' · prefix retained' : ''}</small>
              {/if}
            {/each}
          {/if}
        </article>
      {/each}
      {#if $chatSession.pending?.conversation === $chatSession.selected?.id}
        {#each Object.entries($chatSession.pending?.text ?? {}) as [label, text]}
          <article aria-label={`Pending reply from ${label}`}><strong>{label}</strong><div class="message-content">{text}</div></article>
        {/each}
      {/if}
    </div>
    {#if sourceTitle}
      <section class="sources" aria-label="Consultation sources">
        <header><h2>{sourceTitle}</h2><button type="button" on:click={() => { sourceSerial++; sourceTitle = ''; }}>Close sources</button></header>
        {#each sources as source}
          <button type="button" on:click={() => void showSource(source)}>Open occurrence {source.representation.attachment_id.slice(0, 8)}</button>
        {/each}
        {#if !sources.length && !sourceError}<p>No retained attachment sources.</p>{/if}
        {#if sourceError}<p class="error" role="alert">{sourceError}</p>{/if}
        {#if sourceText}<pre>{sourceText}</pre>{/if}
      </section>
    {/if}
    {#if $chatSession.selected}
      <form on:submit|preventDefault={() => void sendChat()}>
        <textarea aria-label="Message" placeholder="Write a message, or address a persona with @handle" value={$chatSession.draft} on:input={(event) => updateChatDraft(event.currentTarget.value)} disabled={!!$chatSession.pending}></textarea>
        <div class="composer-controls">
          <button type="button" disabled={!!$chatSession.pending} on:click={() => void attachChatFile()}>Attach file</button>
          <button type="button" disabled={!!$chatSession.pending} on:click={() => pasteOpen = !pasteOpen}>Paste source</button>
          <span>{$chatSession.attachments.length ? `${$chatSession.attachments.length} attached` : ''}</span>
          {#if $chatSession.pending}<button type="button" on:click={() => void stopChat()} disabled={$chatSession.pending.stop}>{$chatSession.pending.stop ? 'Stopping…' : 'Stop'}</button>{:else}<button type="submit" disabled={!$chatSession.draft.trim()}>Send</button>{/if}
        </div>
        {#if pasteOpen}<textarea aria-label="Source text" bind:value={pasted}></textarea><button type="button" disabled={!pasted.trim()} on:click={async () => { await pasteChatSource(pasted); pasted = ''; pasteOpen = false; }}>Attach text</button>{/if}
        {#if $chatSession.candidates.length}<div class="personas" aria-label="Consult a persona">{#each $chatSession.candidates as candidate (candidate.id)}<button type="button" disabled={!!$chatSession.pending} on:click={() => updateChatDraft(`${$chatSession.draft}${$chatSession.draft ? ' ' : ''}@${candidate.handle} `)} title={candidate.label}>@{candidate.handle}</button>{/each}</div>{/if}
      </form>
    {/if}
  </main>
</section>

<style>
  .chat-work-area { position: absolute; inset: 0; z-index: 20; display: grid; grid-template-columns: minmax(160px, 220px) minmax(0, 1fr); background: var(--paper, #faf8f4); color: var(--ink, #25231f); }
  aside { border-right: 1px solid #d7d2c9; overflow: auto; padding: 48px 12px 12px; }
  header, .composer-controls { display: flex; align-items: center; gap: 10px; }
  header { flex-wrap: wrap; } h1 { font-size: 1.1rem; } h2 { font-size: 1rem; }
  nav { display: flex; flex-direction: column; margin-top: 16px; gap: 6px; } nav button { text-align: left; overflow-wrap: anywhere; } nav button.selected { background: #e6e0d5; }
  main { display: flex; flex-direction: column; min-height: 0; padding: 20px clamp(16px, 4vw, 64px); }
  .messages { flex: 1; overflow: auto; min-height: 0; } article { padding: 14px 0; } .message-content { white-space: pre-wrap; overflow-wrap: anywhere; line-height: 1.6; margin: 8px 0; } small { margin-left: 10px; }
  button { min-height: 32px; padding: 6px 10px; border: 1px solid #cec8bc; border-radius: 6px; background: transparent; color: inherit; cursor: pointer; } button:disabled { opacity: .5; cursor: default; }
  form { padding-top: 16px; } textarea { box-sizing: border-box; width: 100%; min-height: 90px; padding: 12px; background: transparent; color: inherit; border: 1px solid #cec8bc; border-radius: 8px; font: inherit; resize: vertical; }
  .composer-controls { margin-top: 8px; flex-wrap: wrap; } .composer-controls span { flex: 1; } .personas { display: flex; gap: 6px; overflow-x: auto; padding-top: 10px; } .error { color: #9e3020; } .sources { max-height: 35%; overflow: auto; border-top: 1px solid #cec8bc; } pre { white-space: pre-wrap; overflow-wrap: anywhere; }
  @media (max-width: 640px) { .chat-work-area { grid-template-columns: 150px minmax(0, 1fr); } main { padding: 12px; } }
</style>
