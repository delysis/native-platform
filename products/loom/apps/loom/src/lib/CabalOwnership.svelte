<script lang="ts">
  import type { CabalSnapshot } from './cabal';
  import { normalizeFailure } from './ipc';

  export let cabal: CabalSnapshot;
  export let onTransfer: (member: string, rosterHash: string) => Promise<void>;
  let selected = '';
  let review: { member: string; name: string; rosterHash: string } | null = null;
  let busy = false;
  let error = '';
  $: currentOwner = cabal.my_key === cabal.roster.payload.owner;
  $: stale = review !== null && review.rosterHash !== cabal.roster_hash;

  function prepare(): void {
    const member = cabal.roster.payload.members.find(member => member.key === selected && member.key !== cabal.my_key);
    if (!currentOwner || !member || busy) return;
    review = { member: member.key, name: member.name, rosterHash: cabal.roster_hash };
    error = '';
  }

  async function transfer(): Promise<void> {
    const reviewed = review;
    if (!reviewed || busy || stale || !currentOwner) return;
    busy = true; error = '';
    try { await onTransfer(reviewed.member, reviewed.rosterHash); }
    catch (failure) { error = normalizeFailure(failure).message; }
    finally { busy = false; review = null; }
  }
</script>

{#if currentOwner}
  <details>
    <summary>Hand over the keys</summary>
    <p>Choose a friend to look after invitations and membership. Everyone keeps their writing and pairing.</p>
    <label>Next owner
      <select bind:value={selected} disabled={busy}>
        <option value="">Choose a member</option>
        {#each cabal.roster.payload.members.filter(member => member.key !== cabal.my_key) as member (member.key)}
          <option value={member.key}>{member.name} · {member.key.slice(0, 12)}</option>
        {/each}
      </select>
    </label>
    <button type="button" disabled={busy || !selected} on:click={prepare}>Review handoff</button>
    {#if review}
      <div class="review" aria-label="Ownership handoff review">
        <p><strong>{review.name}</strong> will own this cabal and be able to invite or remove members, including you. You stay a member and can keep writing.</p>
        <p class="identity">Device <code>{review.member}</code></p>
        {#if stale}<p role="status">Membership changed. Review the handoff again.</p>{/if}
        <button type="button" disabled={busy || stale} on:click={() => void transfer()}>Give {review.name} the keys</button>
        <button type="button" disabled={busy} on:click={() => review = null}>Keep the keys</button>
      </div>
    {/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
  </details>
{/if}

<style>
  details { margin:12px; border-top:1px solid var(--line); padding-top:12px; }
  summary { cursor:pointer; color:var(--muted); }
  p { line-height:1.6; margin:10px 0; }
  label { display:block; margin:10px 0; font-size:11px; }
  select { display:block; width:100%; margin-top:6px; padding:8px; color:var(--ink); background:var(--paper); border:1px solid var(--line); border-radius:5px; font:inherit; }
  button { border:0; border-radius:5px; background:var(--paper-deep); padding:6px 8px; color:inherit; font:inherit; cursor:pointer; }
  button:disabled { opacity:.45; cursor:default; }
  .review { border-top:1px solid var(--line); margin-top:12px; }
  .identity { color:var(--muted); font-size:11px; }
  code { display:block; overflow-wrap:anywhere; }
  .error { color:var(--danger); overflow-wrap:anywhere; }
</style>
