<script lang="ts">
  import { onMount } from 'svelte';
  import type { ContextAttachment } from './types';
  import { importAccounts, connectImportAccount, disconnectImportAccount, syncImportAccount, chooseImportBatch, importSourceUrl, importPastedSources, cancelImportAccount,
    type ImportAccount, type ImportBatch, type ImportSource } from './ipc';

  export let projectId: string;
  export let sessionId: string;
  export let onUse: (items: ContextAttachment[]) => Promise<boolean>;
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
  let query = 'newer_than:30d';
  let busy = false;
  let message = '';
  let report: ImportBatch | null = null;
  let selected: string[] = [];
  let lastQuery = '';
  let lastSource: ImportSource = 'gmail';
  $: service = source === 'drive' ? 'drive' : 'gmail';
  $: availableAccounts = accounts.filter((item) => item.service === service && item.email);
  $: account = availableAccounts.find((item) => item.email === chosenEmail)?.email ?? availableAccounts[0]?.email;
  $: canNext = Boolean(report?.next_page_token && lastQuery === query && lastSource === source);

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
    busy = true; authorizing = true; message = 'Finish authorization in your browser. This request expires in five minutes.';
    try {
      const result = await connectImportAccount(projectId, sessionId, service, clientId.trim(), clientSecret);
      accounts = [...accounts.filter((item) => item.service !== result.service || item.email !== result.email), result];
      chosenEmail = result.email ?? ''; configuring = false;
      message = `Connected ${result.email}. Sync runs only when you request it.`;
    } catch (error) { message = errorText(error); }
    finally { clientSecret = ''; busy = false; authorizing = false; }
  }
  async function disconnect(): Promise<void> {
    busy = true;
    try {
      await disconnectImportAccount(projectId, sessionId, service, account ?? '');
      await refresh(); report = null; selected = [];
      message = 'Connection removed from this device. Imported files remain in the project. You can also revoke Loom in your Google account settings.';
    } catch (error) { message = errorText(error); }
    finally { busy = false; }
  }
  async function sync(next = false): Promise<void> {
    busy = true; message = 'Importing up to 16 files. Large pages can take a few minutes.';
    try {
      const token = next && canNext ? report?.next_page_token : undefined;
      report = await syncImportAccount(projectId, sessionId, source, account ?? '', query, token ?? null);
      lastQuery = query; lastSource = source; selected = [];
      message = `${report.imported.length} imported; ${report.failures.length} failed.${report.next_page_token ? ' More results are available.' : ' End of results.'}`;
    } catch (error) { message = errorText(error); }
    finally { busy = false; }
  }
  async function local(folder: boolean): Promise<void> {
    busy = true; message = 'Reading local sources…';
    try {
      report = await chooseImportBatch(projectId, sessionId, folder); selected = [];
      message = `${report.imported.length} imported; ${report.failures.length} failed or skipped.`;
    } catch (error) { message = errorText(error); }
    finally { busy = false; }
  }
  async function web(): Promise<void> {
    busy = true; message = 'Importing the public web document…';
    try { report = await importSourceUrl(projectId, sessionId, webUrl.trim()); selected = []; message = 'Web source imported locally. Select it below to add it to context.'; }
    catch (error) { message = errorText(error); }
    finally { busy = false; }
  }
  async function paste(): Promise<void> {
    busy = true;
    try { report = await importPastedSources(projectId, sessionId, pasted, separator); selected = []; message = `${report.imported.length} pasted sources imported; ${report.failures.length} failed.`; }
    catch (error) { message = errorText(error); }
    finally { busy = false; }
  }
  async function useSelected(): Promise<void> {
    busy = true;
    try {
      if (await onUse(report?.imported.filter((item) => selected.includes(item.id)) ?? [])) {
        message = 'Selected sources added to this document’s context.'; selected = [];
      }
    } catch (error) { message = errorText(error); }
    finally { busy = false; }
  }
</script>

<details class="import-sources">
  <summary>Import sources</summary>
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
  <label>Connected source
    <select bind:value={source} disabled={busy} on:change={() => { query = source === 'drive' ? '' : 'newer_than:30d'; report = null; selected = []; }}>
      <option value="gmail">Gmail</option><option value="google_alerts">Google Alerts in Gmail</option>
      <option value="linked_in">LinkedIn notifications in Gmail</option><option value="drive">Google Drive</option>
    </select>
  </label>
  {#if account && !configuring}
    <label>Account <select bind:value={chosenEmail} disabled={busy} on:change={() => { report = null; selected = []; }}>
      <option value="">Choose account (default: {availableAccounts[0]?.email})</option>
      {#each availableAccounts as item}<option value={item.email ?? ''}>{item.email}</option>{/each}
    </select></label>
    <button disabled={busy} on:click={() => configuring = true}>Connect another account</button>
    <p>Connected: {account} <button disabled={busy} on:click={() => void disconnect()}>Disconnect</button></p>
    <label>{source === 'drive' ? 'Drive folder ID (empty searches the account)' : 'Gmail search'}
      <input bind:value={query} maxlength="2048" disabled={busy} placeholder={source === 'drive' ? 'Folder ID' : 'label:Research newer_than:30d'} />
    </label>
    <div class="actions"><button disabled={busy} on:click={() => void sync()}>Sync now</button>
      {#if canNext}<button disabled={busy} on:click={() => void sync(true)}>Next page</button>{/if}</div>
  {:else}
    <p>Connect with a Google Desktop app OAuth client. Loom requests read-only access and saves credentials in your system credential store.</p>
    <label>Client ID <input bind:value={clientId} disabled={busy} autocomplete="off" maxlength="512" /></label>
    <label>Client secret <input type="password" bind:value={clientSecret} disabled={busy} autocomplete="off" maxlength="1024" /></label>
    {#if configuring}<button disabled={busy} on:click={() => configuring = false}>Back to accounts</button>{/if}
    <button disabled={busy || !clientId.trim()} on:click={() => void connect()}>Connect {service === 'drive' ? 'Drive' : 'Gmail'}</button>
  {/if}
  {#if authorizing}<button on:click={() => { void cancelImportAccount(projectId, sessionId).catch((error) => message = errorText(error)); }}>Cancel connection</button>{/if}
  <p role="status">{message}</p>
  {#if report}
    <div class="results">
      {#each report.imported as item, index (`${item.id}:${index}`)}
        <label class="result"><input type="checkbox" bind:group={selected} value={item.id} disabled={busy} />
          <span>{item.file_name} <small>{item.text_bytes.toLocaleString()} text bytes{item.coverage_complete ? '' : ' · Partial import'}</small>
            {#each item.warnings as warning}<small>{warning}</small>{/each}</span>
        </label>
      {/each}
      {#each report.failures as item}<p class="failure">{item.name}: {item.message}</p>{/each}
    </div>
    {#if report.imported.length > 0}<button disabled={busy || selected.length === 0 || selected.length > 16} on:click={() => void useSelected()}>Add selected to context ({selected.length}/16)</button>{/if}
  {/if}
</details>

<style>
  .import-sources { margin-block: .8rem; font-size: .85rem; }
  summary { cursor: pointer; }
  p { line-height: 1.45; }
  label { display: grid; gap: .35rem; margin-block: .65rem; }
  input, select, button { font: inherit; color: inherit; }
  input:not([type=checkbox]), select { width: 100%; box-sizing: border-box; padding: .5rem; border: 1px solid #8886; border-radius: 4px; background: transparent; }
  button { padding: .4rem .65rem; border: 1px solid #8886; border-radius: 4px; background: transparent; cursor: pointer; }
  button:disabled { opacity: .5; cursor: default; }
  .actions { display: flex; gap: .5rem; }
  .results { max-height: 22rem; overflow: auto; }
  .result { display: flex; gap: .5rem; align-items: start; }
  small { display: block; opacity: .75; margin-top: .2rem; }
  .failure { color: #bb5544; }
</style>
