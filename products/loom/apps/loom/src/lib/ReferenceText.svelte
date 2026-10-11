<script lang="ts">
  import type { ReferenceDiagnostic } from './referenceDiagnostics';
  export let value = '';
  export let offset = 0;
  export let diagnostics: readonly ReferenceDiagnostic[] = [];
  $: pieces = segments(value, offset, diagnostics);
  function segments(text: string, start: number, items: readonly ReferenceDiagnostic[]): { text: string; message?: string }[] {
    const result: { text: string; message?: string }[] = [];
    let cursor = 0;
    for (const item of items) {
      const from = Math.max(0, item.start - start), to = Math.min(text.length, item.end - start);
      if (from >= to) continue;
      result.push({ text: text.slice(cursor, from) }, { text: text.slice(from, to), message: item.message });
      cursor = to;
    }
    result.push({ text: text.slice(cursor) });
    return result;
  }
</script>
{#each pieces as piece}<span class:reference-unavailable={Boolean(piece.message)}>{piece.text}</span>{/each}
