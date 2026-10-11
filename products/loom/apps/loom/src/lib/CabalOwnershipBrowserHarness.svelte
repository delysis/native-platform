<script lang="ts">
  import type { Writable } from 'svelte/store';
  import type { CabalSnapshot } from './cabal';
  import CabalOwnership from './CabalOwnership.svelte';
  export let snapshot: Writable<CabalSnapshot>;
  export let onTransfer: (member: string, rosterHash: string) => Promise<void>;
</script>
<CabalOwnership cabal={$snapshot} {onTransfer} />
