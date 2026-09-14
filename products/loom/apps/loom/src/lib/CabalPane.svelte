<script lang="ts">
  import type { CabalSnapshot } from './cabal';
  import { normalizeFailure } from './ipc';
  export let cabal: CabalSnapshot | null;
  export let projectName: string;
  export let onClose: () => void;
  export let onInvite: (name: string) => Promise<string>;
  export let onJoin: (invitation: string, name: string) => Promise<void>;
  export let onRemove: (member: string, rosterHash: string) => Promise<void>;
  export let onRecover: () => Promise<string[]>;
  let name = '';
  let invitation = '';
  let ticket = '';
  let busy = false;
  let error = '';
  let note = '';
  let removing = '';
  $: owner = cabal?.my_key === cabal?.roster.payload.owner;
  async function run(action: () => Promise<void>): Promise<void> {
    if (busy) return; busy = true; error = ''; note = '';
    try { await action(); } catch (failure) { error = normalizeFailure(failure).message; }
    finally { busy = false; }
  }
  async function invite(): Promise<void> { ticket = await onInvite(name.trim() || 'Loom'); }
  async function copy(): Promise<void> { await navigator.clipboard.writeText(ticket); note = 'Invitation copied'; }
</script>

<section class="cabal-pane" aria-label="Cabal">
  <header><strong>{cabal?.name ?? projectName}</strong><span>cabal</span><button type="button" aria-label="Close cabal" on:click={onClose}>×</button></header>
  {#if cabal}
    <ul aria-label="Cabal members">
      {#each cabal.roster.payload.members as member (member.key)}
        <li><div><strong>{member.name}</strong><small>{member.key === cabal.my_key ? 'You' : cabal.peers.some(peer => peer.cabal === cabal?.id && peer.key === member.key && peer.connected) ? 'Connected' : 'Away · changes will catch up'}</small></div>
          {#if owner && member.key !== cabal.my_key}<button type="button" aria-label={`Remove ${member.name}`} disabled={busy} on:click={() => removing = member.key}>−</button>{/if}
        </li>
        {#if removing === member.key}<li class="removal"><p>{member.name} will stop receiving new changes. Their existing copies remain theirs.</p><button type="button" disabled={busy} on:click={() => void run(async () => { if (cabal) await onRemove(member.key, cabal.roster_hash); removing = ''; })}>Remove member</button><button type="button" on:click={() => removing = ''}>Keep</button></li>{/if}
      {/each}
    </ul>
    {#if !cabal.roster.payload.members.some(member => member.key === cabal?.my_key)}<p class="quiet">This device is no longer a member. Your existing text remains here.</p>{/if}
    {#if cabal.orphaned_changes}<div class="recovery"><p>Some edits are outside the current membership history.</p><button type="button" disabled={busy} on:click={() => void run(async () => { const paths = await onRecover(); note = `${paths.length} recovery ${paths.length === 1 ? 'copy' : 'copies'} in the workspace`; })}>Recover my copies</button></div>{/if}
    {#if owner}<button class="invite" type="button" disabled={busy} on:click={() => void run(invite)}>Invite someone</button>{/if}
  {:else}
    <p class="intro">A little shared mind. Make this workspace a place your people can write together.</p>
    <label>Your name<input maxlength="64" autocomplete="nickname" bind:value={name} placeholder="What your friends call you" /></label>
    <button class="invite" type="button" disabled={busy || !name.trim()} on:click={() => void run(invite)}>Start this cabal</button>
  {/if}
  {#if ticket}<div class="ticket"><label>One friend, one invitation<textarea readonly value={ticket} rows="4"></textarea></label><button type="button" on:click={() => void run(copy)}>Copy invitation</button><p class="quiet">Share this privately with your friend. They only need to join once.</p></div>{/if}
  <details><summary>Join another cabal</summary><label>Invitation<textarea rows="3" bind:value={invitation} placeholder="loom://cabal/…"></textarea></label><label>Your name<input maxlength="64" bind:value={name} /></label><button type="button" disabled={busy || !name.trim() || !invitation.trim()} on:click={() => void run(() => onJoin(invitation.trim(), name.trim()))}>Join</button></details>
  {#if note}<p class="quiet" role="status">{note}</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
</section>

<style>
  .cabal-pane { font-size:12px; height:100%; overflow:auto; }
  header { display:flex; align-items:center; gap:8px; padding:9px 12px; border-bottom:1px solid var(--line); }
  header strong { font-weight:500; flex:1; overflow-wrap:anywhere; } header span, small, .quiet { color:var(--muted); }
  button { border:0; border-radius:5px; background:transparent; padding:6px 8px; color:inherit; font:inherit; cursor:pointer; }
  button:hover:not(:disabled) { background:var(--paper-deep); } button:disabled { opacity:.45; cursor:default; }
  p { line-height:1.6; margin:12px; } label { display:block; margin:12px; font-size:11px; }
  input, textarea { box-sizing:border-box; display:block; width:100%; margin-top:6px; padding:8px; font:inherit; color:var(--ink); background:transparent; border:1px solid var(--line); border-radius:5px; }
  textarea { resize:vertical; line-height:1.5; } .invite { margin:0 12px 12px; background:var(--paper-deep); }
  ul { list-style:none; padding:4px 12px; margin:0; } li { display:flex; gap:8px; align-items:center; padding:8px 0; } li > div { flex:1; } li strong { font-weight:500; } small { display:block; font-size:10px; margin-top:3px; }
  .removal { display:block; border-block:1px solid var(--line); } .removal p { margin:0 0 6px; }
  details { margin:12px; border-top:1px solid var(--line); padding-top:12px; } details label { margin-inline:0; } summary { cursor:pointer; color:var(--muted); }
  .ticket, .recovery { border-block:1px solid var(--line); } .ticket button, .recovery button { margin:0 12px 10px; }
  .error { color:var(--danger); overflow-wrap:anywhere; }
</style>
