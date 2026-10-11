<script lang="ts">
  import { onMount } from 'svelte';
  import type { CabalSnapshot } from './cabal';
  import { ComputeSharing, modelModalities, type ComputeScope, type ComputeHostSnapshot, type ComputeGrantReview, type PendingComputeGrant } from './compute';
  import { normalizeFailure } from './ipc';
  export let cabal: CabalSnapshot;
  export let scope: ComputeScope;
  export let sharing: ComputeSharing;
  let snapshot: ComputeHostSnapshot | null = null;
  let pending: PendingComputeGrant | null = null;
  let busy = false;
  let refreshing = false;
  let mounted = false;
  let error = '';
  let refreshError = '';
  let memberKey = '';
  let jobs = 16;
  let tokens = 256;
  let seconds = 30;
  let review: ComputeGrantReview | null = null;
  $: members = cabal.roster.payload.members.filter(member => member.key !== cabal.my_key);
  $: reviewCurrent = Boolean(review && review.request.roster_hash === cabal.roster_hash && review.request.model_fingerprint === snapshot?.model?.fingerprint && !cabal.read_only && !snapshot?.problem);

  function observe(): void { pending = sharing.attempt(scope); busy = sharing.busy(scope); }
  async function refresh(): Promise<void> {
    if (refreshing) return;
    refreshing = true;
    try {
      const result = await sharing.snapshot(scope);
      if (mounted) { snapshot = result; refreshError = ''; }
    } catch (failure) { if (mounted) refreshError = normalizeFailure(failure).message; }
    finally { refreshing = false; }
  }
  onMount(() => {
    mounted = true;
    const unsubscribe = sharing.subscribe(observe);
    void refresh();
    const timer = setInterval(() => void refresh(), 2000);
    return () => { mounted = false; clearInterval(timer); unsubscribe(); };
  });
  function prepare(): void {
    const member = members.find(item => item.key === memberKey), model = snapshot?.model;
    if (!member || !model || cabal.read_only || ![jobs, tokens, seconds].every(Number.isInteger)
      || jobs < 1 || jobs > 256 || tokens < 1 || tokens > 2048 || seconds < 1 || seconds > 120) return;
    review = { memberName: member.name, modelName: model.name, modalities: modelModalities(model), epoch: cabal.roster.payload.epoch,
      request: { id: crypto.randomUUID(), member_key: member.key, roster_hash: cabal.roster_hash,
        model_fingerprint: model.fingerprint, jobs, max_output_tokens: tokens, max_seconds: seconds } };
  }
  async function change(operation: () => Promise<void>): Promise<void> {
    error = '';
    try { await operation(); if (mounted) { review = null; await refresh(); } }
    catch (failure) { if (mounted) error = normalizeFailure(failure).message; }
  }
</script>

<details class="compute-sharing">
  <summary aria-label="Share idle compute" title="Lend your model. Your own writing and model work take priority.">Compute</summary>
  {#if snapshot?.problem}<p role="alert" class="error">{snapshot.problem}</p>{/if}
  {#if snapshot?.model}<p class="model"><strong>{snapshot.model.name}</strong><small>{modelModalities(snapshot.model)}<br />{snapshot.idle ? 'Idle' : 'Busy'}</small></p>
  {:else}<p class="quiet" title="Load a text-completion model to share it.">Unloaded</p>{/if}
  {#if snapshot?.grants.length}
    <ul aria-label="Compute grants">
      {#each snapshot.grants as item (item.grant.id)}
        <li><strong>{cabal.roster.payload.members.find(member => member.key === item.grant.peer)?.name ?? 'Former member'}</strong><small>{item.grant.model.name} · {item.jobs_remaining} of {item.grant.jobs} jobs left<br />Up to {item.grant.max_output_tokens} tokens · {item.grant.max_seconds}s per job</small>
          {#if !item.current}<small>Membership changed · grant inactive</small>
          {:else if !item.jobs_remaining}<small>Budget used</small>
          {:else if item.grant.model.fingerprint !== snapshot.model?.fingerprint}<small>Waiting for this model</small>{/if}
          <button type="button" disabled={busy} on:click={() => void change(() => sharing.revoke(scope, item.grant.id))} aria-label="Revoke grant" title="Revoke grant"><svg aria-hidden="true" viewBox="0 0 16 16"><path d="M3 8h10" /></svg>Revoke</button>
        </li>
      {/each}
    </ul>
  {/if}
  {#if pending}
    <div class="review" aria-label="Pending compute grant"><p>{pending.memberName} · {pending.modelName}<br />{pending.modalities}<br />{pending.request.jobs} jobs · {pending.request.max_output_tokens} tokens · {pending.request.max_seconds}s</p>
      <p class="quiet">{busy ? 'Saving this grant…' : 'This grant has not been confirmed. Check it or retry the same grant.'}</p>
      <button type="button" disabled={busy || refreshing} on:click={() => void refresh()} aria-label="Check grant" title="Check grant">Check</button>
      <button type="button" disabled={busy} on:click={() => void change(() => sharing.grant(scope))} aria-label="Retry grant" title="Retry grant">Retry</button>
      <button type="button" disabled={busy} on:click={() => void change(() => sharing.revoke(scope, pending!.request.id))} aria-label="Revoke pending grant" title="Revoke pending grant"><svg aria-hidden="true" viewBox="0 0 16 16"><path d="M3 8h10" /></svg>Revoke</button>
    </div>
  {:else if review}
    <div class="review" aria-label="Review compute grant"><p><strong>{review.memberName}</strong> may use <strong>{review.modelName}</strong> for {review.request.jobs} jobs, each up to {review.request.max_output_tokens} tokens and {review.request.max_seconds}s.<br />Inputs: {review.modalities}.</p>

      {#if !reviewCurrent}<p class="error">The model or membership changed. Review a new grant.</p>{/if}
      <button type="button" disabled={busy || !reviewCurrent} on:click={() => void change(() => sharing.grant(scope, review!))} aria-label={`Share with ${review.memberName}`} title="The grant resumes after restart with this model loaded; revoke it here at any time."><svg aria-hidden="true" viewBox="0 0 16 16"><path d="M8 11V2M5 5l3-3 3 3M3 9v5h10V9" /></svg>Share</button>
      <button type="button" disabled={busy} on:click={() => review = null}>Back</button>
    </div>
  {:else if snapshot?.model && !cabal.read_only && !snapshot.problem}
    <form on:submit|preventDefault={prepare}>
      <label>Friend<select bind:value={memberKey} disabled={busy}><option value="">Member</option>{#each members as member (member.key)}<option value={member.key}>{member.name}</option>{/each}</select></label>
      <div class="limits"><label>Jobs<input type="number" min="1" max="256" step="1" bind:value={jobs} required disabled={busy} /></label><label>Tokens<input aria-label="Tokens per job" type="number" min="1" max="2048" step="1" bind:value={tokens} required disabled={busy} /></label><label>Seconds<input aria-label="Seconds per job" type="number" min="1" max="120" step="1" bind:value={seconds} required disabled={busy} /></label></div>
      <button type="submit" disabled={busy || !memberKey} aria-label="Review grant" title="Review grant"><svg aria-hidden="true" viewBox="0 0 16 16"><path d="M3 2h10v12H3zM5 5h6M5 8h6M5 11h4" /></svg>Review</button>
    </form>
  {/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if refreshError}<p class="error" role="alert">{refreshError}</p>{/if}
</details>

<style>
  button svg { width:14px; height:14px; margin-right:5px; vertical-align:-2px; fill:none; stroke:currentColor; stroke-width:1.4; stroke-linecap:round; stroke-linejoin:round; }
  details { margin:12px; border-top:1px solid var(--line); padding-top:12px; font-size:11px; }
  summary { cursor:pointer; color:var(--muted); } p { line-height:1.6; margin:10px 0; overflow-wrap:anywhere; }
  strong { font-weight:500; } small { display:block; color:var(--muted); line-height:1.5; margin-top:3px; }
  .quiet { color:var(--muted); } .error { color:var(--danger); }
  label { display:block; margin:10px 0; } select, input { display:block; box-sizing:border-box; width:100%; margin-top:5px; padding:7px; background:var(--paper); color:var(--ink); border:1px solid var(--line); border-radius:5px; font:inherit; }
  .limits { display:flex; flex-wrap:wrap; gap:8px; } .limits label { flex:1; min-width:65px; }
  button { border:0; border-radius:5px; background:var(--paper-deep); padding:7px 9px; margin:3px 3px 3px 0; color:inherit; font:inherit; cursor:pointer; }
  button:disabled { opacity:.45; cursor:default; } ul { list-style:none; margin:0; padding:0; } li { padding:10px 0; border-top:1px solid var(--line); }
  .review { border-block:1px solid var(--line); padding:4px 0 8px; }
</style>
