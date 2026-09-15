<script lang="ts">
  import type { ContextAttachmentPresentation } from './types';
  export let material: ContextAttachmentPresentation;
  export let disabled = false;
  export let onSave: (id: string, revision: string, excerpt: string | null) => Promise<boolean>;
  let text = material.excerpt ?? '';
  let saved = material.excerpt;
  let busy = false;
  let error = '';
  $: if (material.excerpt !== saved) { saved = material.excerpt; text = saved ?? ''; }
  async function save(excerpt: string | null): Promise<void> {
    if (disabled || busy) return;
    if (excerpt !== null && new TextEncoder().encode(excerpt).byteLength > 256 * 1024) {
      error = 'Use an excerpt smaller than 256 KiB.';
      return;
    }
    busy = true; error = '';
    try {
      if (!await onSave(material.id, material.source_revision, excerpt)) error = 'The source changed or could not be saved. Reopen its context before retrying.';
    } finally { busy = false; }
  }
</script>

<details>
  <summary>{material.excerpt === null ? 'Original source' : 'Edited excerpt'}</summary>
  <p>{material.excerpt === null ? 'Relevant passages from this source are selected for each request.' : 'This excerpt is used as source material. The original remains unchanged.'}</p>
  <label>Excerpt for {material.file_name}
    <textarea bind:value={text} rows="4" disabled={disabled || busy} placeholder="Paste or write the passage to use instead of the original source"></textarea>
  </label>
  <div class="actions">
    <button disabled={disabled || busy || text === material.excerpt} on:click={() => void save(text)}>Use excerpt</button>
    {#if material.excerpt !== null}<button disabled={disabled || busy} on:click={() => void save(null)}>Use original</button>{/if}
  </div>
  {#if error}<p role="status">{error}</p>{/if}
</details>

<style>
  details { min-width: 0; font-size: 12px; overflow-wrap: anywhere; }
  summary { cursor: pointer; padding-block: 4px; }
  p { margin-block: 5px; color: var(--muted); }
  label { display: grid; gap: 4px; }
  textarea { width: 100%; min-width: 0; box-sizing: border-box; resize: vertical; font: inherit; color: var(--ink); background: var(--paper); border: 1px solid var(--line); }
  .actions { display: flex; gap: 4px; flex-wrap: wrap; margin-top: 4px; }
  button { font: inherit; color: var(--ink); background: transparent; border: 1px solid var(--line); border-radius: 4px; min-height: 26px; cursor: pointer; }
  button:disabled { opacity: .5; }
</style>
