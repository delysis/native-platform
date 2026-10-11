<script lang="ts">
  import PeerModelPicker from './PeerModelPicker.svelte';
  import type { CabalSnapshot } from './cabal';
  import type { PeerTarget } from './compute';
  export let cabal: CabalSnapshot;
  let projectId = 'project';
  let sessionId = 'session';
  let host = '';
  let target: PeerTarget | null = null;
  function switchWorkspace() { projectId = 'other'; sessionId = 'other-session'; host = ''; target = null; }
</script>
<PeerModelPicker {projectId} {sessionId} {cabal} bind:host bind:target localModel="Local Gemma" />
<output aria-label="Selected peer">{JSON.stringify({ host, target })}</output>
<button type="button" on:click={switchWorkspace}>Switch workspace</button>
