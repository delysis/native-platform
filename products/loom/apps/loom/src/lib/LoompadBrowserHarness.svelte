<script lang="ts">
  import Loompad from './Loompad.svelte';
  import type { CompletionCandidate } from './completionSession';
  import type { LoompadLength } from './loompad';
  export let choices: CompletionCandidate[];
  export let onChoose: (candidate: CompletionCandidate) => void;
  export let onAccept: (candidate: CompletionCandidate, length: LoompadLength) => void;
  let scope = 'first';
  let focused = true;
  export function update(next: { choices?: CompletionCandidate[]; scope?: string; focused?: boolean }) {
    if (next.choices !== undefined) choices = next.choices;
    if (next.scope !== undefined) scope = next.scope;
    if (next.focused !== undefined) focused = next.focused;
  }
</script>
<section class="editor-stage" style="height:400px;width:480px">
  <textarea class="editor-pane" aria-label="Writing"></textarea>
  <Loompad {choices} {scope} {focused} {onChoose} {onAccept} />
</section>
