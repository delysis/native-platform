<script lang="ts">
  export let title: string;
  export let relativePath: string;
  export let text: string;
  export let hadUnsavedText: boolean;
  export let journalDurable: boolean;
  export let draftWasUncertain: boolean;
  export let saveWasUncertain: boolean;
  export let copyState: 'idle' | 'copied' | 'failed' = 'idle';
  export let onCopy: () => void;
</script>

<section class="missing-document-recovery" role="alert" aria-labelledby="missing-document-title">
  <div>
    <span class="eyebrow">File deleted outside Loom</span>
    <h2 id="missing-document-title">“{title}” is preserved for recovery.</h2>
    <p>
      {#if hadUnsavedText}
        The editor text was captured before Loom left the missing file.
        {#if journalDurable}
          Its latest draft journal is durable.
        {:else}
          Draft durability could not be confirmed, so keep Loom open and copy the text below.
        {/if}
      {:else}
        The last saved text remains in Loom’s local history while the visible file is missing.
      {/if}
    </p>
    {#if draftWasUncertain || saveWasUncertain}
      <p class="missing-document-uncertainty">
        {draftWasUncertain && saveWasUncertain
          ? 'Draft and save results were uncertain when the deletion was detected.'
          : draftWasUncertain
            ? 'A draft result was uncertain when the deletion was detected.'
            : 'A save result was uncertain when the deletion was detected.'}
        The exact editor text preserved here remains the recovery authority.
      </p>
    {/if}
    <p class="missing-document-path">Restore <code>{relativePath}</code> and refocus Loom to reopen it.</p>
  </div>
  <details>
    <summary>Inspect preserved text</summary>
    <textarea readonly aria-label={`Preserved text for ${title}`} value={text}></textarea>
  </details>
  <div class="missing-document-actions">
    <button class="secondary-button" type="button" on:click={onCopy}>Copy preserved text</button>
    <span role="status" aria-live="polite">
      {copyState === 'copied' ? 'Copied' : copyState === 'failed' ? 'Copy failed; select the text above' : ''}
    </span>
  </div>
</section>

<style>
  .missing-document-recovery {
    width: min(880px, calc(100% - 40px));
    display: grid;
    gap: 12px;
    margin: 14px auto 0;
    padding: 14px 16px;
    border: 1px solid color-mix(in srgb, var(--danger) 34%, var(--line));
    border-radius: 10px;
    background: color-mix(in srgb, var(--danger) 5%, var(--paper));
  }
  h2 { margin: 2px 0 5px; font: 500 21px/1.25 Iowan Old Style, Palatino, Georgia, serif; }
  p { margin: 0; color: var(--muted); font-size: 12px; line-height: 1.5; }
  .missing-document-path { margin-top: 5px; }
  .missing-document-uncertainty { margin-top: 5px; color: var(--danger); }
  code { overflow-wrap: anywhere; }
  details { min-width: 0; }
  summary { color: var(--muted); cursor: pointer; font-size: 12px; }
  textarea { width: 100%; min-height: 130px; margin-top: 8px; padding: 10px; resize: vertical; border: 1px solid var(--line); border-radius: 8px; color: var(--ink); background: var(--paper); font: 12px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; }
  .missing-document-actions { display: flex; align-items: center; gap: 10px; }
  [role="status"] { color: var(--muted); font-size: 11px; }
</style>
