<script lang="ts">
  import { onMount } from 'svelte';
  import { newUlid } from './ulid';
  import type { ContextAttachment } from './types';
  import { importAccounts, connectImportAccount, disconnectImportAccount, addCollection, refreshCollection, chooseImportBatch, importSourceUrl, importPastedSources, cancelImportAccount,
    type ImportAccount, type ImportBatch, type ImportSource } from './ipc';

  import CollectionProgress from './CollectionProgress.svelte';
  import { connectedScope } from './collections';
  import type { MaterialEntry } from './materials';

  export let projectId: string;
  export let sessionId: string;
  export let onOpen: (item: ContextAttachment, projectId: string, sessionId: string) => Promise<void>;
  export let onImported: (items: ContextAttachment[], projectId: string, sessionId: string) => Promise<void>;
  export let onCollectionAdded: (entry: MaterialEntry, projectId: string, sessionId: string) => Promise<void> = async () => {};
  export let onOpenCollection: (entry: MaterialEntry, projectId: string, sessionId: string) => Promise<void> = async () => {};
  let collection: MaterialEntry | null = null;
  async function publishImported(): Promise<void> {
    if (report?.imported.length) await onImported(report.imported, projectId, sessionId);
  }
  let accounts: ImportAccount[] = [];
  let service: 'gmail' | 'drive' = 'gmail';
  let webUrl = '';
  let pasted = '';
  let separator = '';
  let chosenEmail = '';
  let configuring = false;
  let authorizing = false;
  let source: ImportSource = 'gmail';
  let clientId = '';
  let clientSecret = '';
  let query = '';
  let collectionName = '';
  let busy = false;
  let operationId = '';
  let message = '';
  let report: ImportBatch | null = null;
  $: service = source === 'drive' ? 'drive' : 'gmail';
  $: availableAccounts = accounts.filter((item) => item.service === service && item.email);
  $: account = availableAccounts.find((item) => item.email === chosenEmail)?.email;
  $: selectedScope = connectedScope(source, query);
  $: canAddCollection = Boolean(account && collectionName.trim() && selectedScope);

  function errorText(error: unknown): string {
    if (error && typeof error === 'object') {
      const value = error as Record<string, unknown>;
      if (typeof value.message === 'string') return value.message;
    }
    return String(error);
  }
  async function refresh(): Promise<void> {
    try { accounts = await importAccounts(projectId, sessionId); }
    catch (error) { message = errorText(error); }
  }
  onMount(() => { void refresh(); });

  async function connect(): Promise<void> {
    operationId = newUlid();
    busy = true; authorizing = true; message = 'Finish authorization in your browser. This request expires in three minutes.';
    try {
      const result = await connectImportAccount(projectId, sessionId, service, clientId.trim(), clientSecret, operationId);
      accounts = [...accounts.filter((item) => item.service !== result.service || item.email !== result.email), result];
      chosenEmail = result.email ?? ''; configuring = false; report = null;
      message = `Connected ${result.email}. Choose the sources to add.`;
    } catch (error) { message = errorText(error); }
    finally { clientSecret = ''; busy = false; authorizing = false; operationId = ''; }
  }
  async function disconnect(): Promise<void> {
    busy = true;
    try {
      await disconnectImportAccount(projectId, sessionId, service, account ?? '');
      await refresh(); report = null;
      message = 'Connection removed from this device. Imported files remain in the project. You can also revoke Loom in your Google account settings.';
    } catch (error) { message = errorText(error); }
    finally { busy = false; }
  }
  async function addConnectedCollection(): Promise<void> {
    if (!canAddCollection || !selectedScope || !account || busy) return;
    const scope = { projectId, sessionId };
    busy = true; message = 'Adding collection…';
    let added: MaterialEntry | null = null;
    try {
      added = await addCollection(scope.projectId, scope.sessionId, collectionName.trim(), selectedScope, account);
      await refreshCollection(scope.projectId, scope.sessionId, added.id, 'fresh');
      message = '';
    } catch (error) { message = errorText(error); }
    finally {
      if (added) {
        collection = added;
        try { await onCollectionAdded(added, scope.projectId, scope.sessionId); }
        catch (error) { message = errorText(error); }
      }
      busy = false;
    }
  }
  async function openCollection(): Promise<void> {
    if (!collection || busy) return;
    busy = true;
    try { await onOpenCollection(collection, projectId, sessionId); }
    catch (error) { message = errorText(error); }
    finally { busy = false; }
  }
  async function local(folder: boolean): Promise<void> {
    operationId = newUlid();
    busy = true; message = 'Reading local sources…';
    try {
      report = await chooseImportBatch(projectId, sessionId, folder, operationId); await publishImported();
      message = `${report.imported.length} imported; ${report.failures.length} failed or skipped.`;
    } catch (error) { message = errorText(error); }
    finally { busy = false; operationId = ''; }
  }
  async function web(): Promise<void> {
    operationId = newUlid();
    busy = true; message = 'Importing the public web document…';
    try { report = await importSourceUrl(projectId, sessionId, webUrl.trim(), operationId); await publishImported(); message = 'Source added.'; }
    catch (error) { message = errorText(error); }
    finally { busy = false; operationId = ''; }
  }
  async function paste(): Promise<void> {
    operationId = newUlid();
    busy = true;
    try { report = await importPastedSources(projectId, sessionId, pasted, separator, operationId); await publishImported(); message = `${report.imported.length} pasted sources imported; ${report.failures.length} failed.`; }
    catch (error) { message = errorText(error); }
    finally { busy = false; operationId = ''; }
  }
  async function openSource(item: ContextAttachment): Promise<void> {
    if (busy) return;
    busy = true;
    try { await onOpen(item, projectId, sessionId); }
    catch (error) { message = errorText(error); }
    finally { busy = false; }
  }
</script>

<section class="import-sources">
  <p>Bring documents, Slack archives, Claude conversations, LinkedIn exports, and mailboxes into this project.</p>
  <div class="actions">
    <button disabled={busy} on:click={() => void local(false)}>Choose files</button>
    <button disabled={busy} on:click={() => void local(true)}>Choose folder</button>
  </div>
  <details><summary>Paste sources</summary>
    <label>Source text <textarea bind:value={pasted} disabled={busy} rows="5"></textarea></label>
    <label>Separator between sources (optional) <input bind:value={separator} disabled={busy} maxlength="128" /></label>
    <button disabled={busy || !pasted.trim()} on:click={() => void paste()}>Import pasted text</button>
  </details>
  <label>Public document URL <input type="url" bind:value={webUrl} disabled={busy} placeholder="https://…" maxlength="4096" /></label>
  <button disabled={busy || !webUrl.trim()} on:click={() => void web()}>Import URL</button>
  {#if collection}
    <div class="connected-collection">
      <button class="source-link" disabled={busy} on:click={() => void openCollection()}>{collection.name}</button>
      <CollectionProgress {projectId} {sessionId} collectionId={collection.id} />
      <button disabled={busy} on:click={() => { collection = null; collectionName = ''; query = ''; message = ''; }}>Add another collection</button>
    </div>
  {:else}
  <label>Connected source
    <select bind:value={source} disabled={busy} on:change={() => { query = ''; chosenEmail = ''; report = null; }}>
      <option value="gmail">Gmail</option><option value="google_alerts">Google Alerts in Gmail</option>
      <option value="linked_in">LinkedIn notifications in Gmail</option><option value="drive">Google Drive</option>
    </select>
  </label>
  {#if availableAccounts.length && !configuring}
    <label>Account <select bind:value={chosenEmail} disabled={busy} on:change={() => { report = null; }}>
      <option value="">Choose account</option>
      {#each availableAccounts as item}<option value={item.email ?? ''}>{item.email}</option>{/each}
    </select></label>
    <button disabled={busy} on:click={() => configuring = true}>Connect another account</button>
    {#if account}<p>Connected: {account} <button disabled={busy} on:click={() => void disconnect()}>Disconnect</button></p>{/if}
    <label>Name <input bind:value={collectionName} maxlength="160" disabled={busy} placeholder="Research" /></label>
    <label>{source === 'drive' ? 'Drive folder ID' : 'Gmail search'}
      <input bind:value={query} maxlength="2048" disabled={busy} placeholder={source === 'drive' ? 'Folder ID' : 'label:Research newer_than:30d'} />
    </label>
    <button disabled={busy || !canAddCollection} on:click={() => void addConnectedCollection()}>Add collection</button>
  {:else}
    <p>Connect with a Google Desktop app OAuth client. Loom requests read-only access and saves credentials in your system credential store.</p>
    <label>Client ID <input bind:value={clientId} disabled={busy} autocomplete="off" maxlength="512" /></label>
    <label>Client secret <input type="password" bind:value={clientSecret} disabled={busy} autocomplete="off" maxlength="1024" /></label>
    {#if configuring}<button disabled={busy} on:click={() => configuring = false}>Back to accounts</button>{/if}
    <button disabled={busy || !clientId.trim()} on:click={() => void connect()}>Connect {service === 'drive' ? 'Drive' : 'Gmail'}</button>
  {/if}
  {/if}
  {#if busy && operationId}<button on:click={() => { void cancelImportAccount(projectId, sessionId, operationId).then(() => { message = "Stopping… Completed sources remain available."; }).catch((error) => message = errorText(error)); }}>{authorizing ? "Cancel connection" : "Stop import"}</button>{/if}
  <p role="status">{message}</p>
  {#if report}
    <div class="results">
      {#each report.imported as item, index (`${item.id}:${index}`)}
        <div class="result">
          <button class="source-link" disabled={busy} on:click={() => void openSource(item)}>{item.file_name}</button>
          {#if !item.coverage_complete}<small>Partial import</small>{/if}
          {#each item.warnings as warning}<small>{warning}</small>{/each}
        </div>
      {/each}
      {#each report.failures as item}<p class="failure">{item.name}: {item.message}</p>{/each}
    </div>
  {/if}
</section>

<style>
  .import-sources { min-width: 0; margin: 0; color: var(--ink); font-size: 12px; line-height: 1.4; overflow-wrap: anywhere; }
  summary { box-sizing: border-box; min-height: 26px; padding: 4px 8px; border-radius: 4px; cursor: pointer; }
  summary:hover { background: var(--chrome-hover); }
  .import-sources > :not(summary) { max-width: calc(100% - 16px); margin-inline: 8px; }
  p { margin-block: 6px; }
  label { display: grid; min-width: 0; gap: 4px; margin-block: 8px; }
  input, textarea, select, button { min-width: 0; max-width: 100%; box-sizing: border-box; font: inherit; color: inherit; }
  input:not([type=checkbox]), textarea, select { width: 100%; padding: 5px 6px; border: 1px solid var(--line); border-radius: 4px; background: var(--paper); }
  textarea { resize: vertical; }
  button { min-height: 26px; padding: 4px 6px; border: 1px solid var(--line); border-radius: 4px; background: transparent; cursor: pointer; white-space: normal; overflow-wrap: anywhere; }
  button:hover { background: var(--chrome-hover); }
  button:disabled { opacity: .5; cursor: default; }
  .actions { display: flex; flex-wrap: wrap; gap: 4px; }
  .results { max-height: 22rem; min-width: 0; overflow: auto; }
  .result { min-width: 0; padding-block: 4px; }
  .source-link { display: block; text-align: left; border: 0; overflow-wrap: anywhere; }
  small { display: block; color: var(--muted); margin-top: 2px; }
  .failure { color: var(--danger); }
</style>
