<script lang="ts">
  import LoomEditor from './LoomEditor.svelte';
  import SourceEditor from './SourceEditor.svelte';
  import { normalizeImportedMarkdown } from './importedMarkdown';
  import { canRoundTripMarkdownExactly } from './markdownSafety';
  import { nativeDropPoint, nativeDropScope } from './nativeAttachmentDrop';
  let visual = true;
  let context = '';
  let manuscript = 'Opening.';
  let contextSurface: HTMLElement;
  let manuscriptSurface: HTMLElement;
  let contextEditor: LoomEditor;
  let manuscriptEditor: LoomEditor;
  let outcome = '';
  function drop(scope: 'context' | 'inline') {
    const bounds = (scope === 'context' ? contextSurface : manuscriptSurface).getBoundingClientRect();
    const point = nativeDropPoint({ x: bounds.left + 30, y: bounds.bottom - 30 }, 'MacIntel', 2);
    const owner = nativeDropScope(point, [
      { scope: 'context', bounds: contextSurface.getBoundingClientRect() },
      { scope: 'inline', bounds: manuscriptSurface.getBoundingClientRect() }
    ]);
    const markdown = normalizeImportedMarkdown('# Imported document\n\nEditable café • 界 🖋.\n');
    if (owner === 'context') contextEditor.insertAttachmentMarkdown(markdown, point.x, point.y);
    if (owner === 'inline') manuscriptEditor.insertAttachmentMarkdown(markdown, point.x, point.y);
    outcome = owner ?? 'rejected';
  }
  function toggle() {
    contextEditor?.flushPending();
    manuscriptEditor?.flushPending();
    if (!visual && (!canRoundTripMarkdownExactly(context) || !canRoundTripMarkdownExactly(manuscript))) { outcome = 'unsafe'; return; }
    visual = !visual;
  }
</script>
<button onclick={toggle}>Toggle both editors</button>
<button onclick={() => drop('context')}>Native payload to context</button>
<button onclick={() => drop('inline')}>Native payload to manuscript</button>
<div bind:this={contextSurface} class="context-editor-surface" style="height:230px">
  {#if visual}<LoomEditor onGhostPresentationRejected={() => {}} bind:this={contextEditor} value={context} onChange={value => context = value} label="Context editor" />
  {:else}<SourceEditor element={undefined} value={context} onValueInput={element => context = element.value} label="Context source" />{/if}
</div>
<div bind:this={manuscriptSurface} class="context-editor-surface" style="height:300px">
  {#if visual}<LoomEditor onGhostPresentationRejected={() => {}} bind:this={manuscriptEditor} value={manuscript} onChange={value => manuscript = value} label="Manuscript editor" />
  {:else}<SourceEditor element={undefined} value={manuscript} onValueInput={element => manuscript = element.value} label="Manuscript source" />{/if}
</div>
<output aria-label="Drop owner">{outcome}</output>
<output aria-label="Context markdown">{context}</output>
<output aria-label="Manuscript markdown">{manuscript}</output>
