<script lang="ts">
  import { onMount } from 'svelte';
  import { normalizeFailure } from './ipc';
  import { signalRequest, type SignalEvent, type SignalIdentityMember, type SignalIdentityReview } from './signal';

  export let conversation: string;
  export let refreshToken = 0;
  let opened = false;
  let mounted = false;
  let busy = false;
  let refreshAgain = false;
  let checked = false;
  let error = '';
  let selected = '';
  let members: SignalIdentityMember[] = [];
  let review: SignalIdentityReview | null = null;
  let serial = 0;
  let observedToken = refreshToken;
  $: title = members.find(member => member.id === selected)?.title ?? 'this person';
  $: if (opened && refreshToken !== observedToken) {
    observedToken = refreshToken;
    // Status and incoming messages can invalidate a displayed verification.
    // This only reads saved keys; it never accepts a key or resends a message.
    void load(false);
  }

  onMount(() => { mounted = true; return () => { mounted = false; serial++; }; });

  function receive(event: SignalEvent, expected: string): void {
    if (event.kind === 'failure') throw new Error(event.message);
    if (event.kind !== 'identity' || event.conversation_id !== conversation ||
        (event.review && !event.members.some(member => member.id === event.review?.recipient_id)) ||
        (expected && event.review && event.review.recipient_id !== expected)) {
      throw new Error('Signal returned an unrelated safety number. Refresh it before verifying.');
    }
    if (event.review?.review_id !== review?.review_id || event.review?.state !== review?.state) checked = false;
    members = event.members;
    selected = event.review?.recipient_id ?? (expected && members.some(member => member.id === expected) ? expected : members.length === 1 ? members[0].id : '');
    review = event.review;
  }

  async function load(refresh: boolean): Promise<void> {
    if (busy) { refreshAgain = true; return; }
    const version = ++serial, expected = selected;
    busy = true; error = '';
    if (refresh) checked = false;
    try {
      const event = await signalRequest({ kind: 'identity', conversation_id: conversation, recipient_id: expected || null, refresh });
      if (mounted && version === serial) receive(event, expected);
    } catch (failure) {
      if (mounted && version === serial) { error = normalizeFailure(failure).message; review = null; }
    } finally { if (mounted && version === serial) settle(); }
  }

  async function verify(): Promise<void> {
    if (!checked || !review || busy || review.state === 'pending' || review.state === 'verified') return;
    const expected = review, version = ++serial;
    busy = true; error = ''; checked = false;
    try {
      const event = await signalRequest({ kind: 'verify_identity', conversation_id: conversation, recipient_id: expected.recipient_id, review_id: expected.review_id });
      if (mounted && version === serial) receive(event, expected.recipient_id);
    } catch (failure) {
      if (mounted && version === serial) { error = normalizeFailure(failure).message; review = null; }
    } finally { if (mounted && version === serial) settle(); }
  }

  function settle(): void {
    busy = false;
    if (refreshAgain && opened) { refreshAgain = false; void load(false); }
  }

  function toggle(): void {
    opened = !opened;
    if (opened) void load(false);
    else { serial++; busy = false; refreshAgain = false; checked = false; review = null; }
  }
</script>

<div class="identity">
  <button type="button" aria-expanded={opened} on:click={toggle}>Safety numbers</button>
  {#if opened}
    <section aria-label="Signal safety number" aria-busy={busy}>
      {#if members.length > 1}
        <select aria-label="Whose safety number" value={selected} disabled={busy || review?.state === 'pending'} on:change={event => { selected = event.currentTarget.value; review = null; void load(false); }}>
          <option value="">Choose someone</option>
          {#each members as member (member.id)}<option value={member.id}>{member.title}</option>{/each}
        </select>
      {/if}
      {#if review}
        <p>{title}</p>
        <p class="number" aria-label="Safety number">{review.safety_number.match(/.{1,5}/g)?.join(' ')}</p>
        <img src={review.qr_code} alt={`Safety number QR code for ${title}`} width="184" height="184" />
        {#if review.state === 'changed'}<p class="changed" role="alert">Safety number changed. Compare this new number with {title} before accepting it.</p>
        {:else if review.state === 'verified'}<p role="status">Verified on this Loom device.</p>
        {:else if review.state === 'pending'}<p role="status">Reconnecting to finish verification…</p>
        {:else}<p>Compare all 60 digits, or have {title} scan this code in Signal.</p>{/if}
        <p class="quiet">{review.refreshed_at === null ? 'Saved safety number' : `Refreshed ${new Date(review.refreshed_at).toLocaleString()}`}</p>
        {#if review.state === 'unverified' || review.state === 'changed'}
          <label><input type="checkbox" bind:checked disabled={busy} />I compared this number with {title}.</label>
          <button type="button" disabled={!checked || busy} on:click={() => void verify()}>{review.state === 'changed' ? 'Verify and accept new number' : 'Mark verified'}</button>
        {/if}
      {:else if !busy && selected}<p>No saved safety number yet. Refresh to look it up.</p>{/if}
      {#if selected}<button type="button" disabled={busy || review?.state === 'pending'} on:click={() => void load(true)}>Refresh safety number</button>{/if}
      {#if busy}<p class="quiet" role="status">Reading safety number…</p>{/if}
      {#if error}<p class="changed" role="alert">{error}</p>{/if}
    </section>
  {/if}
</div>

<style>
  .identity { padding:0 8px 6px; }
  button { border:0; border-radius:5px; background:transparent; color:inherit; padding:5px 7px; font:inherit; cursor:pointer; }
  button:hover:not(:disabled) { background:var(--paper-deep); } button:disabled { opacity:.45; cursor:default; }
  section { max-height:52vh; overflow:auto; padding:4px 8px 10px; border-bottom:1px solid var(--line); }
  p { margin:8px 0; line-height:1.5; overflow-wrap:anywhere; }
  .number { font-family:var(--font-mono, monospace); font-size:14px; line-height:1.8; max-width:25ch; letter-spacing:.03em; }
  img { display:block; width:184px; height:184px; max-width:100%; object-fit:contain; }
  label { display:flex; align-items:flex-start; gap:6px; margin:8px 0; line-height:1.5; } input { flex:none; margin-top:3px; }
  select { max-width:100%; box-sizing:border-box; border:1px solid var(--line); border-radius:5px; color:var(--ink); background:var(--paper); font:inherit; padding:6px; }
  .quiet { color:var(--muted); font-size:11px; } .changed { color:var(--danger); }
</style>
