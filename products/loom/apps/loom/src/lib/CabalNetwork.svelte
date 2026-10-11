<script lang="ts">
  import { onDestroy } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { normalizeFailure } from './ipc';

  type Connection = { mode: 'internet' | 'direct' } | { mode: 'relays'; urls: string[] };
  interface Settings { configured: Connection; active: Connection | null; revision: string }
  let settings: Settings | null = null;
  let mode: Connection['mode'] = 'internet';
  let relays = '';
  let busy = false;
  let error = '';
  let note = '';
  let serial = 0;
  onDestroy(() => { serial++; });

  function accept(value: Settings): void {
    settings = value;
    mode = value.configured.mode;
    relays = value.configured.mode === 'relays' ? value.configured.urls.join('\n') : '';
  }
  async function refresh(): Promise<void> {
    const request = ++serial;
    busy = true; error = ''; note = ''; settings = null;
    try {
      const value = await invoke<Settings>('plugin:loom|cabal_network_get');
      if (request === serial) accept(value);
    } catch (failure) {
      if (request === serial) error = normalizeFailure(failure).message;
    } finally { if (request === serial) busy = false; }
  }
  function toggle(event: Event): void {
    if ((event.currentTarget as HTMLDetailsElement).open) void refresh();
    else { serial++; settings = null; busy = false; error = ''; note = ''; }
  }
  async function save(): Promise<void> {
    if (busy || !settings) return;
    const request = ++serial;
    const connection: Connection = mode === 'relays'
      ? { mode, urls: relays.split(/\r?\n/u).map(url => url.trim()).filter(Boolean) }
      : { mode };
    busy = true; error = ''; note = '';
    try {
      const value = await invoke<Settings>('plugin:loom|cabal_network_set', { expected: settings.revision, connection });
      if (request !== serial) return;
      accept(value);
      note = value.active && JSON.stringify(value.active) !== JSON.stringify(value.configured)
        ? 'Saved. Restart Loom to use these connections; your pairing stays.'
        : 'Connection settings saved.';
    } catch (failure) {
      if (request === serial) { settings = null; error = normalizeFailure(failure).message; }
    } finally { if (request === serial) busy = false; }
  }
</script>

<details class="connections" on:toggle={toggle}>
  <summary>Connections</summary>
  {#if settings}
    <label>Connect through
      <select bind:value={mode} disabled={busy}>
        <option value="internet">Community relays</option>
        <option value="direct">Direct connections</option>
        <option value="relays">Our own relays</option>
      </select>
    </label>
    {#if mode === 'relays'}
      <label>Relay addresses<textarea bind:value={relays} rows="3" maxlength="2052" placeholder="https://relay.example.org" disabled={busy}></textarea></label>
      <p>One to four HTTPS addresses, one per line. Use the same relays as your friends. Public discovery stays off.</p>
    {:else if mode === 'direct'}
      <p>Use addresses shared in invitations and saved pairings. No relays or public discovery; some networks cannot connect directly.</p>
    {:else}
      <p>Use Iroh’s public discovery and community relays when a direct connection is unavailable.</p>
    {/if}
    <button type="button" disabled={busy} on:click={() => void save()}>Save connections</button>
    {#if settings.active && JSON.stringify(settings.active) !== JSON.stringify(settings.configured)}<p>Changes apply after restarting Loom. Saved pairings remain linked.</p>{/if}
  {:else if busy}<p role="status">Reading connection settings…</p>{/if}
  {#if note}<p role="status">{note}</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p><button type="button" on:click={() => void refresh()}>Reload settings</button>{/if}
</details>

<style>
  .connections { margin:12px; border-top:1px solid var(--line); padding-top:12px; }
  summary { color:var(--muted); cursor:pointer; }
  label { display:block; margin-block:12px; font-size:11px; }
  select, textarea { display:block; width:100%; box-sizing:border-box; margin-top:6px; padding:8px; border:1px solid var(--line); border-radius:5px; background:var(--paper); color:var(--ink); font:inherit; }
  textarea { resize:vertical; line-height:1.5; }
  p { color:var(--muted); line-height:1.6; margin-block:10px; }
  button { border:0; border-radius:5px; padding:6px 8px; background:var(--paper-deep); color:inherit; font:inherit; cursor:pointer; }
  button:disabled { opacity:.45; cursor:default; }
  .error { color:var(--danger); overflow-wrap:anywhere; }
</style>
