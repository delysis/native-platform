<script lang="ts">
  import LoomEditor from './LoomEditor.svelte';
  export let initial = 'A little shared mind.';
  let text = initial;
  let pending = false;
  let composing = false;
  let editor: LoomEditor;
  export function receive(next: string): boolean {
    if (pending || composing) return false;
    text = next; return true;
  }
  export function flush(): boolean { return editor.flushPending(); }
  export function value(): string { return text; }
  export function hasPending(): boolean { return pending; }
</script>

<main style="width:720px;padding:24px">
  <LoomEditor bind:this={editor} value={text} collaborative autofocus
    onGhostPresentationRejected={() => {}}
    onChange={value => { text = value; pending = false; }}
    onImmediateDocumentMutation={() => pending = true}
    onCompositionChange={value => composing = value} />
  <output aria-label="Shared Markdown">{text}</output>
</main>
