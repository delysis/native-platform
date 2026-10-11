<script lang="ts">
  import { onMount } from 'svelte';
  import { normalizeFailure } from './ipc';
  import { signalRequest, type SignalCommand, type SignalGroupWorkspaceReview, type SignalWorkspace } from './signal';

  export let conversation: string;
  export let workspaces: SignalWorkspace[] = [];
  export let connected: boolean;
  export let onChanged: () => void;
  let opened = false;
  let mounted = false;
  let busy = false;
  let loaded = false;
  let selected = '';
  let error = '';
  let review: SignalGroupWorkspaceReview | null = null;
  let serial = 0;
  $: canPreview = loaded && connected && !busy && review?.state !== 'unconfirmed' && workspaces.some(item => item.id === selected);

  onMount(() => { mounted = true; return () => { mounted = false; serial++; }; });

  async function perform(command: SignalCommand): Promise<void> {
    if (busy) return;
    const version = ++serial;
    busy = true; error = '';
    try {
      const result = await signalRequest(command);
      if (!mounted || version !== serial) return;
      if (result.kind === 'failure') throw new Error(result.message);
      if (result.kind !== 'group_workspace' || result.conversation_id !== conversation ||
          ('review_id' in command && result.review?.id !== command.review_id) ||
          (command.kind === 'prepare_group_workspace' && result.review?.workspace.id !== command.workspace_id)) {
        throw new Error('Signal returned an unrelated workspace review. Reopen the saved review.');
      }
      review = result.review; loaded = true;
      if (!selected && workspaces.length === 1) selected = workspaces[0].id;
      if (command.kind !== 'group_workspace') onChanged();
    } catch (failure) {
      if (mounted && version === serial) {
        error = normalizeFailure(failure).message;
        // A failed reply can hide a committed edit. Require a fresh saved
        // review before exposing any mutation, including after IPC timeout.
        loaded = false; review = null;
      }
    } finally { if (mounted && version === serial) busy = false; }
  }

  function load(): void { void perform({ kind: 'group_workspace', conversation_id: conversation }); }
  function toggle(): void {
    opened = !opened;
    if (opened) load();
    else { serial++; busy = false; loaded = false; review = null; error = ''; }
  }
  function act(kind: 'publish_group_workspace' | 'check_group_workspace' | 'notify_group_workspace'): void {
    if (review && loaded && connected && !busy) void perform({ kind, conversation_id: conversation, review_id: review.id });
  }
</script>

<div class="group-workspace">
  <button type="button" aria-expanded={opened} on:click={toggle}>Workspace in group description</button>
  {#if opened}
    <section aria-label="Share a group workspace" aria-busy={busy}>
      {#if loaded}
        {#if workspaces.length}
          <label>Workspace
            <select aria-label="Workspace for group description" bind:value={selected} disabled={busy || review?.state === 'unconfirmed'}>
              <option value="">Choose a workspace</option>
              {#each workspaces as workspace (workspace.id)}<option value={workspace.id}>{workspace.title}</option>{/each}
            </select>
          </label>
          <button type="button" disabled={!canPreview} on:click={() => void perform({ kind: 'prepare_group_workspace', conversation_id: conversation, workspace_id: selected })}>Preview description</button>
        {:else}<p>Invite or join a cabal from this conversation to save its workspace here.</p>{/if}
        {#if review}
          <p class="quiet">{review.workspace.title} · This link opens the workspace for people who have already joined it.</p>
          <details><summary>Previous description</summary><p class="description">{review.before || '(Empty)'}</p></details>
          <p class="quiet">Reviewed description</p><p class="description" aria-label="Reviewed group description">{review.after}</p>
          {#if review.state === 'prepared'}
            <button type="button" disabled={!connected || busy} on:click={() => act('publish_group_workspace')}>Publish description</button>
          {:else if review.state === 'unconfirmed'}
            <p role="status">The description update is unconfirmed. Check it before deciding to retry.</p>
            <button type="button" disabled={!connected || busy} on:click={() => act('publish_group_workspace')}>Retry this exact update</button>
          {:else if review.state === 'conflict'}<p role="status">The group description changed. Preview a new addition to preserve those changes.</p>
          {:else}
            <p role="status">Workspace link published.</p>
            {#if review.notification === 'none'}
              <p class="quiet">Notify members so their Signal apps refresh the description.</p>
              <button type="button" disabled={!connected || busy} on:click={() => act('notify_group_workspace')}>Notify group members</button>
            {:else if review.notification === 'unconfirmed'}<p role="status">Notification delivery is unconfirmed. Loom will not send it again. The description is saved.</p>
            {:else}<p class="quiet">Group notification sent.</p>{/if}
          {/if}
          <button type="button" disabled={!connected || busy} on:click={() => act('check_group_workspace')}>Check description</button>
        {/if}
      {:else if !busy}<button type="button" on:click={load}>Reopen saved review</button>{/if}
      {#if !connected}<p class="quiet">Reconnect Signal to preview or publish a description.</p>{/if}
      {#if busy}<p class="quiet" role="status">Updating workspace review…</p>{/if}
      {#if error}<p class="error" role="alert">{error}</p>{/if}
    </section>
  {/if}
</div>

<style>
  .group-workspace { padding:0 8px 6px; }
  button { border:0; border-radius:5px; background:transparent; color:inherit; padding:5px 7px; font:inherit; cursor:pointer; }
  button:hover:not(:disabled) { background:var(--paper-deep); } button:disabled { opacity:.45; cursor:default; }
  section { max-height:48vh; overflow:auto; padding:4px 8px 10px; border-bottom:1px solid var(--line); }
  p { margin:8px 0; line-height:1.5; overflow-wrap:anywhere; }
  .description { white-space:pre-wrap; }
  label { display:flex; flex-direction:column; gap:5px; margin:8px 0; }
  select { max-width:100%; box-sizing:border-box; border:1px solid var(--line); border-radius:5px; color:var(--ink); background:var(--paper); font:inherit; padding:6px; }
  .quiet, summary { color:var(--muted); font-size:11px; } summary { cursor:pointer; } .error { color:var(--danger); }
</style>
