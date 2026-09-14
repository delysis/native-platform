<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { normalizeFailure } from './ipc';
  import type { SignalDraftEditor } from './signalDraft';
  import SignalIdentity from './SignalIdentity.svelte';
  import { listenSignal, signalRequest, signalDraftPrompt, signalWorkspaces, updateSignalWorkspace, signalWorkspaceIds, type SignalConversation, type SignalEvent, type SignalMessage, type SignalStatus, type SignalWorkspaceLinks } from './signal';

  export let onClose: () => void;
  export let onDraft: (prompt: string) => Promise<string>;
  export let onCabal: (conversation: SignalConversation) => Promise<string>;
  export let onJoin: (conversation: SignalConversation, invitation: string) => Promise<void>;
  export let onWorkspace: (conversation: SignalConversation, id: string) => Promise<void>;
  export let editor: SignalDraftEditor;
  export let modelLabel = 'Local model';
  export let workspaceScope = '';
  let status: SignalStatus = { version: 3, phase: 'unlinked', account_id: null, device_name: null };
  let identityRefresh = 0;
  let conversations: SignalConversation[] = [];
  let selected = editor.conversation;
  let search = '';
  let messages: SignalMessage[] = [];
  let draft = editor.text;
  let proposal = '';
  let error = '';
  let qr = '';
  let busy = true;
  let drafting = false;
  let mounted = false;
  let loading = false;
  let refreshAgain = false;
  let now = Date.now();
  let serial = 0;
  let viewport: HTMLDivElement;
  let composer: HTMLTextAreaElement;
  let uncertain = editor.pending;
  let saving = false;
  let links: SignalWorkspaceLinks | null = null;
  let linksSerial = 0;
  $: conversation = conversations.find(item => item.id === selected);
  $: visible = messages.filter(message => message.expires_at === null || message.expires_at > now);
  $: canDraft = conversation && !conversation.disappearing && !visible.some(message => message.ephemeral) && !drafting && !busy && !uncertain;
  $: if (workspaceScope !== proposalScope) proposal = '';
  let proposalScope = workspaceScope;

  onMount(() => {
    mounted = true;
    void editor.settle().catch(failure => { if (mounted) error = normalizeFailure(failure).message; })
      .finally(() => {
        if (!mounted) return;
        selected = editor.conversation; draft = editor.text; uncertain = editor.pending; busy = false;
        if (status.account_id) void refresh();
      });
    let unlisten: (() => void) | undefined;
    void listenSignal(event => void observe(event)).then(value => { if (mounted) unlisten = value; else value(); });
    void signalRequest({ kind: 'status' }).then(observe).catch(failure => error = normalizeFailure(failure).message);
    const timer = setInterval(() => { now = Date.now(); }, 1000);
    return () => { mounted = false; serial++; linksSerial++; clearInterval(timer); unlisten?.(); };
  });

  async function observe(event: SignalEvent): Promise<void> {
    if (!mounted) return;
    if (event.kind === 'status') {
      identityRefresh++;
      status = event.status;
      if (status.phase !== 'linking') qr = '';
      if (status.account_id) await refresh();
    } else if (event.kind === 'changed') {
      identityRefresh++;
      await refresh();
    } else if (event.kind === 'failure') {
      error = event.message;
      if (event.code === 'signal_offline') status = { ...status, phase: 'offline' };
      if (event.code === 'link_failed') { status = { ...status, phase: 'unlinked' }; qr = ''; }
    } else if (event.kind === 'link') {
      qr = event.qr_code;
      status = { ...status, phase: 'linking' };
    }
  }

  async function refresh(): Promise<void> {
    if (loading) { refreshAgain = true; return; }
    loading = true;
    try {
      const result = await signalRequest({ kind: 'conversations' });
      if (!mounted) return;
      if (result.kind === 'failure') { error = result.message; return; }
      if (result.kind === 'conversations') conversations = result.conversations;
      if (selected) await Promise.all([loadMessages(), loadWorkspaces()]);
    } catch (failure) { if (mounted) error = normalizeFailure(failure).message; }
    finally {
      loading = false;
      if (refreshAgain && mounted) { refreshAgain = false; void refresh(); }
    }
  }

  async function choose(id: string): Promise<void> {
    if (busy || drafting || id === selected) return;
    busy = true;
    try {
      await editor.open(id);
      if (!mounted) return;
      selected = id; draft = editor.text; proposal = ''; error = ''; messages = []; uncertain = editor.pending;
      links = null;
      await Promise.all([loadMessages(), loadWorkspaces()]);
      await tick(); composer?.focus();
    } catch (failure) { error = normalizeFailure(failure).message; }
    finally { busy = false; }
  }

  async function saveDraft(): Promise<void> {
    if (!mounted || editor.conversation !== selected) return;
    editor.text = draft; editor.pending = uncertain; saving = true;
    try { await editor.flush(); }
    finally { saving = false; }
  }

  function changeDraft(text: string): void {
    if (busy) return;
    replaceDraft(text);
  }

  function replaceDraft(text: string): void {
    if (!mounted || editor.conversation !== selected || uncertain || editor.pending) return;
    draft = text;
    void saveDraft().catch(failure => error = normalizeFailure(failure).message);
  }

  async function loadMessages(): Promise<void> {
    const expected = selected, version = ++serial;
    if (!expected) return;
    const following = !viewport || viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight < 64;
    const result = await signalRequest({ kind: 'messages', conversation_id: expected, before: null, limit: 100 });
    if (!mounted || version !== serial || selected !== expected) return;
    if (result.kind === 'messages' && result.conversation_id === expected) {
      messages = result.messages;
      if (following) { await tick(); if (viewport) viewport.scrollTop = viewport.scrollHeight; }
    } else if (result.kind === 'failure') error = result.message;
  }

  async function loadWorkspaces(): Promise<void> {
    const expected = selected, version = ++linksSerial;
    if (!expected) return;
    const result = await signalWorkspaces(expected);
    if (mounted && selected === expected && version === linksSerial) links = result;
  }

  async function forgetWorkspace(id: string): Promise<void> {
    if (!links || busy) return;
    const expected = selected, version = ++linksSerial;
    busy = true; error = '';
    try {
      const result = await updateSignalWorkspace(expected, links.version, id, null);
      if (mounted && selected === expected && version === linksSerial) links = result;
    } catch (failure) { if (mounted) { error = normalizeFailure(failure).message; await loadWorkspaces().catch(() => {}); } }
    finally { if (mounted) busy = false; }
  }

  async function visitWorkspace(value: string, join: boolean): Promise<void> {
    if (!conversation || busy || drafting) return;
    busy = true; error = '';
    try {
      if (join) await onJoin(conversation, value); else await onWorkspace(conversation, value);
      if (mounted) await loadWorkspaces();
    } catch (failure) { if (mounted) error = normalizeFailure(failure).message; }
    finally { if (mounted) busy = false; }
  }

  async function link(): Promise<void> {
    busy = true; error = ''; status = { ...status, phase: 'linking' };
    try { await observe(await signalRequest({ kind: 'link', device_name: 'Loom' })); }
    catch (failure) { error = normalizeFailure(failure).message; status = { ...status, phase: 'unlinked' }; }
    finally { busy = false; }
  }

  async function send(check = false): Promise<void> {
    if (busy || !selected || !draft.trim()) return;
    busy = true; error = '';
    try {
      const result = await editor.send(check);
      if (!mounted) return;
      if (result.kind === 'sent') await loadMessages();
      else if (result.kind === 'not_sent') error = 'This message was not sent. Your draft is ready.';
      else if (result.kind === 'failure') error = result.message;
    } catch (failure) { if (mounted) error = normalizeFailure(failure).message; }
    finally { if (mounted) { draft = editor.text; uncertain = editor.pending; busy = false; } }
  }

  async function draftReply(): Promise<void> {
    if (!conversation || !canDraft) return;
    const expected = selected, scope = workspaceScope;
    drafting = true; error = '';
    try {
      const result = await onDraft(signalDraftPrompt(conversation, visible, draft));
      if (!mounted || expected !== selected || scope !== workspaceScope) return;
      proposalScope = scope; proposal = result;
    }
    catch (failure) { if (mounted) error = normalizeFailure(failure).message; }
    finally { drafting = false; }
  }

  async function invite(): Promise<void> {
    if (!conversation || busy || uncertain) return;
    const expected = selected, scope = workspaceScope;
    busy = true; error = '';
    try {
      const invitation = await onCabal(conversation);
      if (!mounted || expected !== selected || scope !== workspaceScope) return;
      replaceDraft(`${draft}${draft ? '\n\n' : ''}${invitation}`);
      await loadWorkspaces();
      await tick(); composer?.focus();
    } catch (failure) { error = normalizeFailure(failure).message; }
    finally { busy = false; }
  }

  function keydown(event: KeyboardEvent): void {
    if (event.isComposing) return;
    if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
      event.preventDefault(); event.stopPropagation(); if (!uncertain) void send();
    }
  }
  function invitations(text: string): string[] { return [...new Set(text.match(/loom:\/\/cabal\/[A-Za-z0-9_-]+/g) ?? [])].slice(0, 4); }
</script>

<section class="signal-pane" aria-label="Signal">
  <header><strong>Signal</strong><span class="connection" role="status">{status.phase === 'connected' ? '' : status.phase}</span><button type="button" aria-label="Close Signal" on:click={onClose}>×</button></header>
  {#if status.phase === 'unlinked' || status.phase === 'linking'}
    <div class="link-device">
      {#if qr}<img src={qr} alt="Signal linked device QR code" width="232" height="232" />
        <p>On your phone, open Signal → Settings → Linked devices → Link new device.</p>
        <button type="button" on:click={() => void signalRequest({ kind: 'cancel_link' }).then(observe)}>Cancel</button>
      {:else}<p>Your people, beside your writing.</p><button type="button" on:click={() => void link()} disabled={busy || status.phase === 'linking'}>{status.phase === 'linking' ? 'Connecting…' : 'Link Signal'}</button>{/if}
    </div>
  {:else}
    <div class="conversation-picker">
      <input aria-label="Find a Signal conversation" placeholder="Find someone…" bind:value={search} />
      <select aria-label="Signal conversation" value={selected} on:change={event => void choose(event.currentTarget.value)} disabled={busy || drafting}>
        <option value="">Choose a conversation</option>
        {#each conversations.filter(item => item.title.toLocaleLowerCase().includes(search.toLocaleLowerCase())) as item (item.id)}<option value={item.id}>{item.title}</option>{/each}
      </select>
    </div>
    {#if conversation}
      {#key selected}<SignalIdentity conversation={selected} refreshToken={identityRefresh} />{/key}
      {#if conversation.description}<p class="description">{conversation.description}</p>{/if}
      {#if links?.workspaces.length || signalWorkspaceIds(conversation.description ?? '').length}
        <div class="workspaces" aria-label="Conversation workspaces">
          {#each links?.workspaces ?? [] as workspace (workspace.id)}
            <div><button type="button" disabled={busy || drafting} on:click={() => void visitWorkspace(workspace.id, false)}>{workspace.title}</button><button type="button" aria-label={`Forget workspace ${workspace.title}`} disabled={busy} on:click={() => void forgetWorkspace(workspace.id)}>×</button></div>
          {/each}
          {#each signalWorkspaceIds(conversation.description ?? '').filter(id => !links?.workspaces.some(item => item.id === id)) as id}
            <button type="button" disabled={busy || drafting} on:click={() => void visitWorkspace(id, false)}>Open workspace</button>
          {/each}
        </div>
      {/if}
      <div class="messages" bind:this={viewport} role="log" aria-label={`Messages with ${conversation.title}`} aria-live="polite">
        {#each visible as message (message.id)}
          <article class:outgoing={message.outgoing}>
            <div class="byline"><span>{message.outgoing ? 'You' : message.sender_name}</span><time datetime={new Date(message.timestamp).toISOString()}>{new Date(message.timestamp).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}</time></div>
            <p>{message.deleted ? 'Message deleted' : message.text}</p>
            {#if !message.deleted}
              {#each invitations(message.text) as invitation}<button type="button" disabled={busy || drafting} on:click={() => void visitWorkspace(invitation, true)}>Join cabal</button>{/each}
              {#each signalWorkspaceIds(message.text) as id}<button type="button" disabled={busy || drafting} on:click={() => void visitWorkspace(id, false)}>Open workspace</button>{/each}
            {/if}
            {#if message.attachment_count}<small>{message.attachment_count} attachment{message.attachment_count === 1 ? '' : 's'} · Open in Signal</small>{/if}
            {#if message.edited}<small>Edited</small>{/if}
          </article>
        {/each}
        {#if !visible.length}<p class="quiet">New messages will appear here after Signal syncs this device.</p>{/if}
      </div>
      {#if proposal}<div class="proposal"><p>{proposal}</p><button type="button" disabled={busy || !!uncertain} on:click={() => { changeDraft(proposal); proposal = ''; }}>Use draft</button><button type="button" on:click={() => proposal = ''}>Discard</button></div>{/if}
      <div class="compose">
        <textarea bind:this={composer} aria-label="Signal message draft" placeholder="Write a message…" rows="3" value={draft} on:input={event => changeDraft(event.currentTarget.value)} on:keydown={keydown} disabled={busy || !!uncertain}></textarea>
        {#if saving}<span class="quiet" role="status">Saving draft…</span>{/if}
        <div class="actions">
          <button type="button" title={`Draft locally with ${modelLabel} using the last 20 messages. The draft is retained in this workspace.`} disabled={!canDraft} on:click={() => void draftReply()}>{drafting ? 'Drafting…' : 'Draft locally'}</button>
          <button type="button" disabled={busy || drafting || !!uncertain} on:click={() => void invite()}>Invite to cabal</button>
          {#if uncertain}<button type="button" disabled={busy} on:click={() => void send(true)}>Check send</button>
          {:else}<button type="button" class="send" disabled={busy || drafting || !draft.trim() || status.phase !== 'connected'} on:click={() => void send()}>Send</button>{/if}
        </div>
        {#if conversation.disappearing}<p class="quiet">Disappearing messages stay out of retained model drafts.</p>{/if}
      </div>
    {:else}<p class="quiet empty">Choose someone to talk and make things with.</p>{/if}
  {/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
</section>

<style>
  .signal-pane { display:flex; flex-direction:column; height:100%; min-height:0; font-size:12px; }
  header { display:flex; align-items:center; gap:8px; padding:9px 12px; border-bottom:1px solid var(--line); }
  header strong { font-weight:500; } .connection { margin-left:auto; color:var(--muted); font-size:11px; }
  button { border:0; border-radius:5px; background:transparent; color:inherit; padding:5px 7px; font:inherit; cursor:pointer; }
  button:hover:not(:disabled) { background:var(--paper-deep); } button:disabled { opacity:.45; cursor:default; }
  .link-device { margin:auto; padding:24px; text-align:center; max-width:290px; } .link-device img { display:block; max-width:100%; height:auto; margin:0 auto 16px; border-radius:6px; }
  .link-device p { line-height:1.6; } .link-device button { background:var(--paper-deep); }
  .conversation-picker { display:flex; flex-direction:column; gap:6px; padding:10px 12px; }
  input, select, textarea { box-sizing:border-box; width:100%; border:1px solid var(--line); border-radius:5px; color:var(--ink); background:transparent; font:inherit; padding:7px 8px; }
  .description { margin:0; padding:0 12px 10px; color:var(--muted); white-space:pre-wrap; overflow-wrap:anywhere; }
  .workspaces { display:flex; flex-wrap:wrap; gap:3px; padding:0 8px 8px; border-bottom:1px solid var(--line); }
  .workspaces > div { display:flex; align-items:center; background:var(--paper-deep); border-radius:5px; max-width:100%; }
  .workspaces button:first-child { min-width:0; overflow-wrap:anywhere; text-align:left; }
  .messages { flex:1; min-height:80px; overflow:auto; padding:4px 12px 12px; }
  article { margin:12px 18px 12px 0; } article.outgoing { margin:12px 0 12px 18px; }
  .byline { display:flex; align-items:baseline; gap:8px; font-size:10px; color:var(--muted); } time { margin-left:auto; }
  article p { white-space:pre-wrap; overflow-wrap:anywhere; line-height:1.6; margin:3px 0; } small { color:var(--muted); font-size:10px; }
  .compose { border-top:1px solid var(--line); padding:10px; } textarea { resize:vertical; min-height:70px; max-height:240px; line-height:1.5; }
  .actions { display:flex; gap:2px; margin-top:5px; align-items:center; flex-wrap:wrap; font-size:11px; } .send { margin-left:auto; }
  .proposal { padding:10px 12px; border-top:1px solid var(--line); max-height:200px; overflow:auto; } .proposal p { white-space:pre-wrap; line-height:1.6; }
  .quiet { color:var(--muted); font-size:11px; line-height:1.5; } .empty { margin:24px 12px; }
  .error { padding:8px 12px; margin:0; color:var(--danger); font-size:11px; line-height:1.5; overflow-wrap:anywhere; }
</style>
