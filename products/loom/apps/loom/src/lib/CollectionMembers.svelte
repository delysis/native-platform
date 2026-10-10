<script lang="ts">
  import { onMount } from 'svelte';
  import { collectionMembers } from './ipc';
  import { collectionErrorText } from './collections';
  import type { CollectionMember } from './types';

  export let projectId: string;
  export let sessionId: string;
  export let collectionId: string;
  export let onOpenMember: (member: CollectionMember) => void;
  // The caller can invalidate a displayed list after observed publications.
  export let revision: string | number = 0;

  let members: CollectionMember[] = [];
  let nextOffset: number | null = null;
  let snapshotId: string | null = null;
  let busy = false;
  let error = '';
  let mounted = false;
  let loadedKey = '';
  let request = 0;
  $: key = `${projectId}:${sessionId}:${collectionId}:${revision}`;
  $: if (mounted && loadedKey !== key) {
    loadedKey = key;
    members = [];
    nextOffset = null;
    snapshotId = null;
    void load(0);
  }
  async function load(offset: number): Promise<void> {
    const current = ++request;
    busy = true;
    error = '';
    try {
      const result = await collectionMembers(projectId, sessionId, collectionId, offset, offset ? snapshotId : null);
      if (!mounted || current !== request) return;
      // All pages and selected sources belong to the displayed immutable version.
      snapshotId = result.snapshot_id;
      const byOccurrence = new Map((offset ? members : []).map(member => [member.occurrence_id, member]));
      for (const member of result.members) byOccurrence.set(member.occurrence_id, member);
      members = [...byOccurrence.values()];
      nextOffset = result.next_offset;
    } catch (cause) {
      if (mounted && current === request) error = collectionErrorText(cause);
    } finally {
      if (mounted && current === request) busy = false;
    }
  }
  onMount(() => {
    mounted = true;
    return () => { mounted = false; ++request; };
  });
</script>

<div class="collection-members" aria-busy={busy}>
  {#each members as member (member.occurrence_id)}
    <button class="member" on:click={() => onOpenMember(member)}>{member.name}</button>
  {/each}
  {#if error}<p role="alert">{error} <button disabled={busy} on:click={() => void load(nextOffset ?? 0)}>Retry</button></p>
  {:else if !busy && !members.length}<p class="empty">No retained sources yet.</p>{/if}
  {#if nextOffset !== null}<button disabled={busy} on:click={() => void load(nextOffset!)}>More sources</button>{/if}
</div>

<style>
  .collection-members { min-width: 0; }
  button { font: inherit; color: var(--ink); border: 1px solid var(--line); border-radius: 4px; background: transparent; padding: 4px 6px; cursor: pointer; }
  .member { display: block; width: 100%; text-align: left; border: 0; overflow-wrap: anywhere; }
  button:hover { background: var(--chrome-hover); }
  button:disabled { opacity: .5; }
  p { margin: 6px 0; }
  .empty { color: var(--muted); }
  [role=alert] { color: var(--danger); }
</style>
