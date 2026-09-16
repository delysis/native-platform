<script lang="ts">
  import { onMount } from 'svelte';
  import { normalizeFailure } from './ipc';
  import { reviewContext, publishContext } from './contextPublication';
  import type { ContextPublicationReview, ContextPublicationScope, PublishedContext } from './contextPublication';

  export let scope: ContextPublicationScope;
  export let readonly = false;
  export let beforeReview: () => Promise<boolean>;
  export let onOpen: (published: PublishedContext) => Promise<void>;
  let mounted = false;
  let busy = false;
  let error = '';
  let review: ContextPublicationReview | null = null;
  let published: PublishedContext | null = null;
  let pending: ContextPublicationReview | null = null;
  const key = (value: ContextPublicationScope) => `${value.projectId}/${value.sessionId}/${value.documentId}`;
  let currentScope = key(scope);
  $: if (currentScope !== key(scope)) {
    currentScope = key(scope); review = null; published = null; pending = null; error = ''; busy = false;
  }
  onMount(() => { mounted = true; return () => { mounted = false; }; });

  async function prepare(): Promise<void> {
    if (busy || readonly) return;
    const captured = { ...scope }, expected = key(captured);
    busy = true; error = '';
    try {
      if (!await beforeReview()) throw new Error('Finish saving context before sharing it.');
      if (!mounted || currentScope !== expected || readonly) return;
      const result = await reviewContext(captured);
      if (mounted && currentScope === expected) { review = result; pending = null; }
    } catch (failure) { if (mounted && currentScope === expected) error = normalizeFailure(failure).message; }
    finally { if (mounted && currentScope === expected) busy = false; }
  }

  async function publish(): Promise<void> {
    if (busy || readonly || !review) return;
    const captured = { ...scope }, expected = key(captured), approved = pending ?? review;
    busy = true; error = ''; pending = approved;
    try {
      const result = await publishContext(captured, approved.request);
      if (mounted && currentScope === expected) { published = result; review = null; pending = null; }
    } catch (failure) { if (mounted && currentScope === expected) error = normalizeFailure(failure).message; }
    finally { if (mounted && currentScope === expected) busy = false; }
  }

  async function open(): Promise<void> {
    if (!published || busy) return;
    const expected = currentScope;
    busy = true; error = '';
    try { await onOpen(published); }
    catch (failure) { if (mounted && currentScope === expected) error = normalizeFailure(failure).message; }
    finally { if (mounted && currentScope === expected) busy = false; }
  }
  function size(bytes: number): string { return `${new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(bytes / 1024)} KiB`; }
</script>

<section class="share-context" aria-label="Share context">
  {#if published}
    <p>Shared as <strong>{published.path}</strong>. Edit this document with your cabal.</p>
    <p>Use <code>{published.reference}</code> in a document or prompt. Your scratch context stays local.</p>
    <button type="button" disabled={busy} on:click={() => void open()}>Open shared context</button>
  {:else if review}
    {#if review.published}
      <p>This context already has a shared document. Its later edits are kept there.</p>
    {:else}
      <p>{review.started ? 'Finish publishing the previously approved context' : 'Share this context'} with {review.members.join(', ')}.</p>
      <p class="quiet">It becomes an ordinary document with quoted material excerpts. The listed files are shared in full. Your manuscript and private scratch context stay as they are.</p>
      <pre aria-label="Context to share">{review.material.markdown}</pre>
      {#if review.material.files.length}
        <ul aria-label="Files to share">{#each review.material.files as file (file.id)}<li>{file.name} · {size(file.byte_count)}</li>{/each}</ul>
      {/if}
      <p class="quiet">{review.path}</p>
    {/if}
    <button type="button" disabled={busy || readonly} on:click={() => void publish()}>{busy ? 'Working…' : review.published ? 'Find shared context' : pending || review.started ? 'Finish sharing' : 'Share with cabal'}</button>
    {#if !pending}<button type="button" disabled={busy} on:click={() => { review = null; }}>Close review</button>{/if}
    {#if pending}<p class="quiet">Sharing may have started. Finish uses the same document and approved files.</p><button type="button" disabled={busy || readonly} on:click={() => void prepare()}>Review saved publication</button>{/if}
  {:else}
    <button type="button" disabled={busy || readonly} on:click={() => void prepare()}>{busy ? 'Preparing…' : 'Share context as a document…'}</button>
  {/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
</section>

<style>
  .share-context { border-top:1px solid var(--line); margin-top:8px; padding-top:8px; font-size:12px; }
  p { margin:6px 0; line-height:1.5; overflow-wrap:anywhere; }
  .quiet { color:var(--muted); }
  button { padding:6px 8px; border:1px solid var(--line); border-radius:5px; background:transparent; color:inherit; font:inherit; cursor:pointer; margin:4px 6px 4px 0; }
  button:disabled { opacity:.5; cursor:default; }
  pre { white-space:pre-wrap; overflow:auto; max-height:220px; overflow-wrap:anywhere; font:inherit; border:1px solid var(--line); padding:8px; }
  code { overflow-wrap:anywhere; }
  ul { padding-left:18px; }
  .error { color:var(--danger); }
</style>
