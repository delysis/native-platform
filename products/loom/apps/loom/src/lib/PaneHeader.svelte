<script lang="ts">
  export let title: string;
  export let choices: Array<[string, { title?: string | null }]> = [];
  export let selected = '';
  export let selectionDisabled = false;
  export let onSelect: (id: string) => void = () => {};
  export let onCollapse: () => void;
</script>

<header class="pane-header">
  {#if choices.length > 1}
    <select aria-label="Pane" value={selected} disabled={selectionDisabled} on:change={(event) => onSelect(event.currentTarget.value)}>
      {#each choices as [id, config]}<option value={id}>{config.title ?? id}</option>{/each}
    </select>
  {:else}<span>{title}</span>{/if}
  <button type="button" aria-label={`Collapse ${title}`} title={`Collapse ${title}`} on:click={onCollapse}>
    <svg aria-hidden="true" viewBox="0 0 16 16"><path d="m4 4 8 8M12 4l-8 8" /></svg>
  </button>
</header>

<style>
  .pane-header { flex:none; display:flex; align-items:center; gap:6px; min-height:30px; padding:0 6px 0 10px; }
  span, select { min-width:0; flex:1; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; color:var(--muted); font-size:11px; }
  select { border:0; background:transparent; }
  button { flex:none; display:grid; place-items:center; width:26px; height:26px; padding:5px; border:0; border-radius:5px; background:transparent; color:var(--muted); cursor:pointer; }
  button:hover { background:var(--line-soft); color:var(--ink); }
  button:focus-visible { outline:2px solid var(--moss); outline-offset:-2px; }
  svg { width:16px; height:16px; fill:none; stroke:currentColor; stroke-width:1.5; stroke-linecap:round; }
</style>
