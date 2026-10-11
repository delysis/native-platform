<script lang="ts">
  import type { CabalSnapshot } from './cabal';
  import { normalizeFailure } from './ipc';
  import ComputeHostControls from './ComputeHostControls.svelte';
  import CabalNetwork from './CabalNetwork.svelte';
  import CabalOwnership from './CabalOwnership.svelte';
  import type { ComputeSharing } from './compute';
  export let cabal: CabalSnapshot | null;
  export let projectName: string;
  export let projectId: string;
  export let sessionId: string;
  export let computeSharing: ComputeSharing;
  export let unsaved = false;
  export let onClose: () => void;
  export let onInvite: (name: string) => Promise<string>;
  export let onJoin: (invitation: string, name: string) => Promise<void>;
  export let onRemove: (member: string, rosterHash: string) => Promise<void>;
  export let onTransfer: (member: string, rosterHash: string) => Promise<void>;
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
        <li><div><strong>{member.name}</strong><small>{member.key === cabal.roster.payload.owner ? 'Owner · ' : ''}{member.key === cabal.my_key ? 'You' : cabal.peers.some(peer => peer.cabal === cabal?.id && peer.key === member.key && peer.connected) ? 'Connected' : 'Away'}</small></div>
          {#if owner && member.key !== cabal.my_key}<button type="button" aria-label={`Remove ${member.name}`} disabled={busy} on:click={() => removing = member.key}>−</button>{/if}
        </li>
        {#if removing === member.key}<li class="removal"><p>{member.name} will stop receiving new changes. Their existing copies remain theirs.</p><button type="button" disabled={busy} on:click={() => void run(async () => { if (cabal) await onRemove(member.key, cabal.roster_hash); removing = ''; })} aria-label="Remove member" title="Remove member"><svg aria-hidden="true" viewBox="0 0 16 16"><path d="M3 8h10" /></svg>Remove</button><button type="button" on:click={() => removing = ''}>Keep</button></li>{/if}
      {/each}
    </ul>
    {#if cabal.read_only}<p class="quiet">This device is no longer a member. Your existing text remains here.</p>{/if}
    {#each cabal.problems as problem}<p class="error" role="alert"><strong>{problem.name}</strong><br />{problem.message}</p>{/each}
    {#if cabal.documents.some(item => item.shared.deleted)}<p class="quiet">An open document was removed from this cabal. Its text stays here until you leave it.</p>{/if}
    {#if unsaved || cabal.read_only || cabal.orphaned_changes || cabal.removed_documents}<div class="recovery"><button type="button" disabled={busy} on:click={() => void run(async () => { const paths = await onRecover(); note = `${paths.length} recovery ${paths.length === 1 ? 'copy' : 'copies'} in the workspace`; })} aria-label="Recover my copies" title="Recover my copies"><svg aria-hidden="true" viewBox="0 0 16 16"><path d="M3 6a5 5 0 1 1 0 5M3 2v4h4" /></svg>Recover</button></div>{/if}
    <slot />
    {#if owner}<button class="invite" type="button" disabled={busy} on:click={() => void run(invite)} aria-label="Invite someone" title="Invite someone"><svg aria-hidden="true" viewBox="0 0 16 16"><path d="M8 2v12M2 8h12" /></svg>Invite</button>{/if}
    {#if owner}{#key cabal.id}<CabalOwnership {cabal} {onTransfer} />{/key}{/if}
    {#key cabal.id}<ComputeHostControls {cabal} scope={{ projectId, sessionId, cabalId: cabal.id }} sharing={computeSharing} />{/key}
  {:else}

    <label>Name<input aria-label="Your name" maxlength="64" autocomplete="nickname" bind:value={name} placeholder="Name" title="What your friends call you" /></label>
    <button class="invite" type="button" disabled={busy || !name.trim()} on:click={() => void run(invite)} aria-label="Start this cabal" title="Start this cabal"><svg aria-hidden="true" viewBox="0 0 16 16"><path d="M8 2v12M2 8h12" /></svg>Start</button>
  {/if}
  {#if ticket}<div class="ticket"><label>Invitation<textarea title="Share this privately. The invitation expires after 24 hours; joined devices reconnect automatically." readonly value={ticket} rows="4"></textarea></label><button type="button" on:click={() => void run(copy)} aria-label="Copy invitation" title="Copy invitation"><svg aria-hidden="true" viewBox="0 0 16 16"><path d="M5 5h8v9H5zM3 11H2V2h9v1" /></svg>Copy</button></div>{/if}
  <details><summary aria-label="Join another cabal" title="Join another cabal">Join</summary><label>Invitation<textarea rows="3" bind:value={invitation} placeholder="loom://cabal/…"></textarea></label><label>Name<input aria-label="Your name" maxlength="64" bind:value={name} /></label><button type="button" disabled={busy || !name.trim() || !invitation.trim()} on:click={() => void run(() => onJoin(invitation.trim(), name.trim()))}><svg aria-hidden="true" viewBox="0 0 16 16"><path d="M2 8h9M8 5l3 3-3 3M11 2h3v12h-3" /></svg>Join</button></details>
  {#if note}<p class="quiet" role="status">{note}</p>{/if}
  <CabalNetwork />
  {#if error}<p class="error" role="alert">{error}</p>{/if}
</section>

<style>
  button svg { width:14px; height:14px; margin-right:5px; vertical-align:-2px; fill:none; stroke:currentColor; stroke-width:1.4; stroke-linecap:round; stroke-linejoin:round; }
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
