<script lang="ts">
  import { formatByteCount, type ConfiguredModelDownload } from './modelDownload';

  export let downloads: Record<string, ConfiguredModelDownload>;
  export let disabled = false;
  export let error = '';
  export let onStart: (definition: ConfiguredModelDownload) => void;
  export let onSettings: () => void;
</script>

<section class="configured-downloads model-download-content" aria-labelledby="configured-downloads-title">
  <div class="section-heading">
    <h3 id="configured-downloads-title">Configured downloads</h3>
    <button class="bare-button compact" type="button" on:click={onSettings}>Open settings file</button>
  </div>
  {#if error}
    <p class="download-error" role="alert">{error}</p>
  {:else if Object.keys(downloads).length === 0}
    <p class="download-boundary">Name custom model or projector downloads under <code>[downloads.name]</code> in <code>.mine.toml</code>.</p>
  {:else}
    <p class="download-boundary">Settings never start a transfer. Each command verifies the configured checksum before installing the file.</p>
    {#each Object.entries(downloads) as [name, definition] (name)}
      <article class="download-card">
        <header>
          <div><strong>{name}</strong><span>{definition.file_name}</span></div>
          <span>{definition.expected_bytes === null ? `Up to ${formatByteCount(definition.max_bytes)}` : formatByteCount(definition.expected_bytes)}</span>
        </header>
        <details class="model-technical">
          <summary>Download identity</summary>
          <dl class="model-evidence">
            <div><dt>URL</dt><dd><code>{definition.url}</code></dd></div>
            <div><dt>SHA-256</dt><dd><code>{definition.sha256}</code></dd></div>
            <div><dt>Hard ceiling</dt><dd>{definition.max_bytes.toLocaleString()} bytes</dd></div>
          </dl>
        </details>
        <footer>
          <button class="secondary-button compact" type="button" {disabled} aria-label={`Download and verify ${name}`} on:click={() => onStart(definition)}>Download and verify</button>
        </footer>
      </article>
    {/each}
  {/if}
</section>

<style>
  .configured-downloads { border-top: 1px solid var(--line); }
  .section-heading { flex-wrap: wrap; }
  .download-boundary { margin-bottom: 12px; }
  .download-card summary { color: var(--muted); cursor: pointer; font-size: 11px; margin-top: 10px; }
  .download-card dl { margin-top: 8px; padding: 0; }
  .download-card code { overflow-wrap: anywhere; }
</style>
