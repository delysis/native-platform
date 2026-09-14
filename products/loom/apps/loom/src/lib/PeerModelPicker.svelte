<script lang="ts">
  import { onDestroy } from 'svelte';
  import { peerOffers, type PeerOffers, type PeerTarget } from './compute';
  import type { CabalSnapshot } from './cabal';
  import { normalizeFailure } from './ipc';
  export let projectId: string;
  export let sessionId: string;
  export let cabal: CabalSnapshot | null = null;
  export let localModel = 'Local model';
  export let host = '';
  export let target: PeerTarget | null = null;
  export let disabled = false;
  let offers: PeerOffers | null = null;
  let loading = false;
  let error = '';
  let serial = 0;
  let scope = '';
  $: members = cabal?.roster.payload.members.filter(member => member.key !== cabal?.my_key) ?? [];
  $: if (scope !== `${projectId}/${sessionId}`) {
    scope = `${projectId}/${sessionId}`;
    serial++; offers = null; error = ''; loading = false;
  }
  $: stale = Boolean(host && (!members.some(member => member.key === host) || (target && target.roster_hash !== cabal?.roster_hash)));
  onDestroy(() => { serial++; });
  async function discover(): Promise<void> {
    const request = ++serial;
    const selectedHost = host;
    const selectedScope = scope;
    target = null; offers = null; error = ''; loading = Boolean(host);
    if (!host) return;
    try {
      const reply = await peerOffers(projectId, sessionId, selectedHost);
      if (request !== serial || selectedScope !== scope || host !== selectedHost) return;
      if (!cabal || reply.host !== host || reply.roster_hash !== cabal.roster_hash ||
        reply.grants.some(grant => grant.cabal !== cabal?.id || grant.peer !== cabal.my_key || grant.epoch !== cabal.roster.payload.epoch)) {
        throw new Error('Membership changed. Check this friend’s models again.');
      }
      offers = reply;
      if (!reply.grants.length) error = 'No model is shared with this device yet.';
    } catch (failure) {
      if (request === serial && selectedScope === scope) error = normalizeFailure(failure).message;
    } finally { if (request === serial && selectedScope === scope) loading = false; }
  }
  function choose(event: Event): void {
    const id = (event.currentTarget as HTMLSelectElement).value;
    const grant = offers?.grants.find(grant => grant.id === id);
    target = grant && offers ? { host, grant: structuredClone(grant), roster_hash: offers.roster_hash } : null;
  }
</script>

<div class="peer-model-picker">
  <select aria-label="Run on" bind:value={host} on:change={() => void discover()} {disabled}>
    <option value="">{localModel}</option>
    {#each members as member}<option value={member.key}>{member.name}’s models</option>{/each}
    {#if host && !members.some(member => member.key === host)}<option value={host}>Unavailable friend</option>{/if}
  </select>
  {#if host}
    {#if offers?.grants.length}
      <select aria-label="Friend’s model" value={target?.grant.id ?? ''} on:change={choose} disabled={disabled || loading || stale}>
        <option value="">Choose a model</option>
        {#each offers.grants as grant}<option value={grant.id}>{grant.model.name} · up to {Math.min(512, grant.max_output_tokens)} tokens</option>{/each}
      </select>
    {:else if target}<span>{target.grant.model.name}</span>{/if}
    <button type="button" on:click={() => void discover()} disabled={disabled || loading}>{loading ? 'Checking…' : 'Check models'}</button>
    {#if target}<small>Resolved text goes to this friend.</small>{/if}
    {#if error || stale}<span role="status" aria-label="Peer model status">{stale ? 'Membership changed. Check models again.' : error}</span>{/if}
  {/if}
</div>

<style>
  .peer-model-picker { min-width:0; flex:1; display:flex; flex-wrap:wrap; align-items:center; gap:0.4rem; font-size:0.75rem; }
  select { max-width:16rem; min-height:1.8rem; background:transparent; color:inherit; border:1px solid var(--border, #8885); border-radius:4px; }
  button { font:inherit; }
  small { opacity:0.7; }
</style>
