<script lang="ts">
  import { onMount } from 'svelte';
  import { authorizeCollection, cancelCollection, collectionStatus, importAccounts, refreshCollection, type ImportAccount } from './ipc';
  import { collectionErrorText, collectionProgressText } from './collections';
  import type { CollectionStatus } from './types';

  export let projectId: string;
  export let sessionId: string;
  export let collectionId: string;
  export let onChanged: (status: CollectionStatus) => void = () => {};

  let status: CollectionStatus | null = null;
  let error = '';
  let busy = false;
  let mounted = false;
  let loadedKey = '';
  let request = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let choosingAccount = false;
  let accounts: ImportAccount[] = [];
  let accountEmail = '';
  $: key = `${projectId}:${sessionId}:${collectionId}`;
  $: if (mounted && loadedKey !== key) {
    loadedKey = key;
    status = null;
    error = '';
    busy = false;
    choosingAccount = false;
    accounts = [];
    accountEmail = '';
    void read();
  }

  function clearPoll(): void {
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
  }
  function schedule(): void {
    clearPoll();
    if (mounted && status?.phase === 'running') timer = setTimeout(() => void read(), 1500);
  }
  function accept(value: CollectionStatus): void {
    status = value;
    error = '';
    onChanged(value);
    schedule();
  }
  async function read(): Promise<void> {
    clearPoll();
    const current = ++request;
    try {
      const value = await collectionStatus(projectId, sessionId, collectionId);
      if (mounted && current === request) accept(value);
    } catch (cause) {
      if (mounted && current === request) { error = collectionErrorText(cause); schedule(); }
    }
  }
  async function act(action: 'fresh' | 'resume' | 'stop'): Promise<void> {
    if (busy || !status) return;
    if (action !== 'stop' && !status.refresh_authorized) return;
    const job = status.job_id;
    if (action === 'stop' && !job) return;
    clearPoll();
    const current = ++request;
    busy = true;
    try {
      const value = action === 'stop'
        ? await cancelCollection(projectId, sessionId, collectionId, job!)
        : await refreshCollection(projectId, sessionId, collectionId, action);
      if (mounted && current === request) accept(value);
    } catch (cause) {
      if (mounted && current === request) { error = collectionErrorText(cause); schedule(); }
    } finally {
      if (mounted && current === request) busy = false;
    }
  }
  async function chooseAccount(): Promise<void> {
    if (busy || !status) return;
    const current = ++request;
    busy = true;
    try {
      const all = await importAccounts(projectId, sessionId);
      if (!mounted || current !== request) return;
      const service = status.scope.kind === 'drive_folder' ? 'drive' : 'gmail';
      accounts = all.filter(account => account.service === service && account.email);
      accountEmail = '';
      choosingAccount = true;
    } catch (cause) {
      if (mounted && current === request) error = collectionErrorText(cause);
    } finally {
      if (mounted && current === request) busy = false;
    }
  }
  async function connect(): Promise<void> {
    if (busy || !status || !accountEmail) return;
    const current = ++request;
    busy = true;
    try {
      const value = await authorizeCollection(projectId, sessionId, collectionId, status.definition_fingerprint, accountEmail);
      if (mounted && current === request) { choosingAccount = false; accept(value); }
    } catch (cause) {
      if (mounted && current === request) error = collectionErrorText(cause);
    } finally {
      if (mounted && current === request) busy = false;
    }
  }
  onMount(() => {
    mounted = true;
    return () => {
      mounted = false;
      ++request;
      clearPoll(); // Detach the observer; the durable backend job keeps its owner.
    };
  });
</script>

<div class="collection-progress">
  {#if status}
    <div class="progress-line">
      <span role="status">{collectionProgressText(status)}</span>
      {#if status.phase === 'running'}
        <button disabled={busy || !status.job_id} on:click={() => void act('stop')}>Stop</button>
      {:else if status.refresh_authorized}
        <button disabled={busy} on:click={() => void act(status?.resumable ? 'resume' : 'fresh')}>{status.resumable ? 'Resume' : 'Refresh'}</button>
        {#if status.resumable}
          <button disabled={busy} on:click={() => void act('fresh')}>Start again</button>
        {/if}
      {/if}
    </div>
    {#if !status.refresh_authorized}
      <p class="muted">{status.local_readable && status.retained_count > 0 ? 'Retained sources are available. Reconnect to refresh.' : 'Reconnect to add sources.'}</p>
      {#if choosingAccount}
        <p class="muted">{status.scope.kind === 'drive_folder' ? `Drive folder: ${status.scope.id}` : `Mail: ${status.scope.query}`}</p>
        {#if accounts.length}
          <label>Account <select bind:value={accountEmail} disabled={busy}>
            <option value="">Choose an account</option>
            {#each accounts as account}<option value={account.email ?? ''}>{account.email}</option>{/each}
          </select></label>
          <button disabled={busy || !accountEmail} on:click={() => void connect()}>Connect</button>
        {:else}
          <p class="muted">Connect an account in Add sources, then return here.</p>
          <button disabled={busy} on:click={() => void chooseAccount()}>Check connections</button>
        {/if}
      {:else}
        <button disabled={busy} on:click={() => void chooseAccount()}>Reconnect</button>
      {/if}
    {/if}
    {#if status.failures.length}
      <details><summary>{status.failures.length} {status.failures.length === 1 ? 'source needs attention' : 'sources need attention'}</summary>
        {#each status.failures as failure}<p>{failure.name}: {failure.message}</p>{/each}
      </details>
    {/if}
  {/if}
  {#if error}<p role="alert">{error} <button disabled={busy} on:click={() => void read()}>Retry</button></p>{/if}
</div>

<style>
  .collection-progress { font: inherit; color: var(--ink); }
  .progress-line { display: flex; align-items: baseline; gap: 8px; flex-wrap: wrap; }
  p { margin: 4px 0; overflow-wrap: anywhere; }
  .muted { color: var(--muted); }
  button { font: inherit; color: inherit; background: transparent; border: 1px solid var(--line); border-radius: 4px; padding: 3px 6px; cursor: pointer; }
  button:hover { background: var(--chrome-hover); }
  button:disabled { opacity: .5; cursor: default; }
  summary { cursor: pointer; margin-top: 4px; }
  [role=alert] { color: var(--danger); }
</style>
