<script lang="ts">
  import { convertFileSrc } from '@tauri-apps/api/core';
  import type { ContextAttachmentPresentation } from './types';
  import MaterialExcerpt from './MaterialExcerpt.svelte';
  export let title: string;
  export let instructions: string;
  export let attachments: ContextAttachmentPresentation[];
  export let disabled = false;
  export let error = '';
  export let onInstructions: (text: string) => void;
  export let onFlush: () => void;
  export let onRemove: (id: string) => void;
  export let onSave: (id: string, revision: string, excerpt: string | null) => Promise<boolean>;
</script>

<details class="materials">
  <summary>Materials{attachments.length ? ` (${attachments.length})` : ''}</summary>
  <p>For <strong>{title}</strong></p>
  {#if error}
    <p role="status">{error}</p>
    <p>Your document remains editable. Saved context has not been rewritten.</p>
  {:else}
    <label>Instructions
      <textarea value={instructions} rows="3" disabled={disabled} on:input={(event) => onInstructions(event.currentTarget.value)} on:blur={onFlush} placeholder="How to use the selected material"></textarea>
    </label>
    {#each attachments as material (material.id)}
      <article>
        <div class="heading"><strong>{material.file_name}</strong><button aria-label={`Remove ${material.file_name} from materials`} disabled={disabled} on:click={() => onRemove(material.id)}>×</button></div>
        {#if !material.coverage_complete}<small>Partially extracted</small>{/if}
        {#if material.presentation_kind === 'file'}<small>Original retained; no model-readable content.</small>{/if}
        {#if material.text_bytes > 0}<MaterialExcerpt {material} {disabled} {onSave} />{/if}
        {#each material.media as media (media.sha256)}
          {#if media.preview_token}
            {#if media.kind === 'image'}<img src={convertFileSrc(media.preview_token, 'loom-asset')} alt={material.file_name} />
            {:else if media.kind === 'audio'}<audio src={convertFileSrc(media.preview_token, 'loom-asset')} controls preload="none" aria-label={`Play ${material.file_name}`}></audio>{/if}
          {/if}
        {/each}
      </article>
    {/each}
  {/if}
</details>

<style>
  .materials { font-size: 12px; min-width: 0; overflow-wrap: anywhere; }
  summary { min-height: 26px; box-sizing: border-box; padding: 4px 8px; cursor: pointer; }
  summary:hover { background: var(--chrome-hover); }
  .materials > :not(summary) { margin: 6px 8px; }
  p, label, article { min-width: 0; }
  label { display: grid; gap: 4px; }
  textarea { min-width: 0; width: 100%; box-sizing: border-box; resize: vertical; font: inherit; color: var(--ink); background: var(--paper); border: 1px solid var(--line); }
  article { border-top: 1px solid var(--line); padding-block: 6px; }
  .heading { display: flex; align-items: start; gap: 4px; }
  .heading strong { flex: 1; min-width: 0; }
  button { flex: none; border: 0; color: var(--muted); background: transparent; cursor: pointer; min-width: 26px; min-height: 26px; }
  img, audio { display: block; width: 100%; max-width: 100%; margin-top: 4px; }
  small { color: var(--muted); }
</style>
