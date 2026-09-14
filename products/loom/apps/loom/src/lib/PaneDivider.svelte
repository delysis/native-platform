<script lang="ts">
  export let edge: 'left' | 'right' | 'top';
  export let label: string;
  export let size: number;
  export let min = 160;
  export let max = 600;
  export let onResize: (size: number) => void;
  let drag: { id: number; coordinate: number; size: number } | null = null;
  $: horizontal = edge === 'top';
  $: direction = edge === 'right' ? 1 : -1;
  function resize(value: number): void { onResize(Math.round(Math.max(min, Math.min(Math.max(min, max), value)))); }
  function start(event: PointerEvent): void {
    if (event.button !== 0) return;
    event.preventDefault();
    const target = event.currentTarget as HTMLElement;
    target.focus();
    target.setPointerCapture(event.pointerId);
    drag = { id: event.pointerId, coordinate: horizontal ? event.clientY : event.clientX, size };
  }
  function move(event: PointerEvent): void {
    if (drag?.id !== event.pointerId) return;
    resize(drag.size + direction * ((horizontal ? event.clientY : event.clientX) - drag.coordinate));
  }
  function stop(event: PointerEvent): void {
    if (drag?.id !== event.pointerId) return;
    drag = null;
    const target = event.currentTarget as HTMLElement;
    if (target.hasPointerCapture(event.pointerId)) target.releasePointerCapture(event.pointerId);
  }
  function key(event: KeyboardEvent): void {
    const delta = horizontal
      ? event.key === 'ArrowUp' ? -1 : event.key === 'ArrowDown' ? 1 : 0
      : event.key === 'ArrowLeft' ? -1 : event.key === 'ArrowRight' ? 1 : 0;
    if (!delta && event.key !== 'Home' && event.key !== 'End') return;
    event.preventDefault();
    resize(event.key === 'Home' ? min : event.key === 'End' ? max : size + delta * direction * (event.shiftKey ? 40 : 10));
  }
</script>

<!-- A focusable ARIA window splitter uses separator with an adjustable value. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex a11y_no_noninteractive_element_interactions -->
<div class="pane-divider" class:dragging={drag !== null} class:horizontal class:left={edge === 'left'} class:right={edge === 'right'}
  role="separator" tabindex="0" aria-label={label} aria-orientation={horizontal ? 'horizontal' : 'vertical'}
  aria-valuemin={min} aria-valuemax={Math.max(min, max)} aria-valuenow={size}
  on:pointerdown={start} on:pointermove={move} on:pointerup={stop} on:pointercancel={stop} on:lostpointercapture={() => drag = null} on:keydown={key}></div>

<style>
  .pane-divider { position:absolute; z-index:20; top:0; bottom:0; width:7px; cursor:col-resize; touch-action:none; user-select:none; outline:none; }
  .left { left:0; } .right { right:0; }
  .horizontal { left:0; right:0; bottom:auto; width:auto; height:7px; cursor:row-resize; }
  .pane-divider:hover, .pane-divider:focus-visible, .dragging { background:color-mix(in srgb, var(--moss) 35%, transparent); }
</style>
