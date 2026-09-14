<script lang="ts">
  import { onMount } from 'svelte';
  import type { CompletionCandidate } from './completionSession';
  import { emptyLoompadChord, LOOMPAD_KEYS, loompadKey, loompadPrefix, type LoompadLength } from './loompad';

  export let choices: readonly CompletionCandidate[] = [];
  export let selectedRunId = '';
  export let scope = '';
  export let focused = true;
  export let blocked = false;
  export let visual = true;
  export let onChoose: (candidate: CompletionCandidate) => void = () => {};
  export let onAccept: (candidate: CompletionCandidate, length: LoompadLength) => void = () => {};
  export let onExit: () => void = () => {};
  let chord = emptyLoompadChord();
  let modifierHeld = false;
  let slots: Array<CompletionCandidate | null> = [null, null, null, null];
  let page = 0;
  let previousScope: string | undefined;

  // Preserve each live run's key, including a partially consumed continuation.
  function reconcile(next: readonly CompletionCandidate[], nextScope: string): void {
    if (nextScope !== previousScope) {
      previousScope = nextScope;
      slots = [null, null, null, null];
      page = 0;
      chord = emptyLoompadChord();
    }
    const heldIndex = LOOMPAD_KEYS.indexOf(chord.choices.at(-1) as typeof LOOMPAD_KEYS[number]);
    const heldRun = heldIndex < 0 ? null : slots[page * 4 + heldIndex]?.runId;
    const incoming = new Map(next.map(candidate => [candidate.runId, candidate]));
    const distinct = new Map<string, CompletionCandidate>();
    // Keep a surviving representative on its key; full branches remain cached
    // in the parent and can diverge again after this exact word is consumed.
    for (const candidate of [...slots.flatMap(slot => slot && incoming.has(slot.runId) ? [incoming.get(slot.runId)!] : []), ...next]) {
      const word = loompadPrefix(candidate.text, 'word', visual);
      if (word && !distinct.has(word)) distinct.set(word, candidate);
    }
    const byRun = new Map([...distinct.values()].map(candidate => [candidate.runId, candidate]));
    const pageAnchor = slots.slice(page * 4, page * 4 + 4).find(slot => slot && byRun.has(slot.runId));
    const kept: Array<CompletionCandidate | null> = [];
    for (let offset = 0; offset < slots.length; offset += 4) {
      const group = slots.slice(offset, offset + 4).map(slot => slot ? byRun.get(slot.runId) ?? null : null);
      if (group.some(Boolean)) kept.push(...group);
    }
    for (const candidate of byRun.values()) {
      if (kept.some(slot => slot?.runId === candidate.runId)) continue;
      let empty = kept.indexOf(null);
      if (empty < 0) { empty = kept.length; kept.push(null, null, null, null); }
      kept[empty] = candidate;
    }
    slots = kept.length ? kept : [null, null, null, null];
    const anchored = pageAnchor ? slots.findIndex(slot => slot?.runId === pageAnchor.runId) : -1;
    page = anchored >= 0 ? Math.floor(anchored / 4) : Math.min(page, slots.length / 4 - 1);
    if (heldIndex >= 0 && heldRun !== slots[page * 4 + heldIndex]?.runId) chord = emptyLoompadChord();
  }
  $: reconcile(choices, scope);
  $: visibleSlots = slots.slice(page * 4, page * 4 + 4);
  $: if (!focused || blocked) reset();

  function reset(): void { chord = emptyLoompadChord(); modifierHeld = false; }
  function keydown(event: KeyboardEvent): void {
    const target = event.target;
    if (!focused || blocked || event.defaultPrevented || event.isComposing || event.metaKey || event.ctrlKey ||
        !(target instanceof Element) || !target.closest('.editor-stage') || target.closest('input,button,select')) return;
    if (event.key === 'Alt') { modifierHeld = true; return; }
    if (!event.altKey) return;
    modifierHeld = true;
    if (event.code === 'Space') {
      event.preventDefault(); event.stopPropagation();
      if (!event.repeat) {
        chord = emptyLoompadChord();
        const pages = slots.length / 4;
        page = (page + (event.shiftKey ? pages - 1 : 1)) % pages;
      }
      return;
    }
    if (event.key === 'Escape') {
      event.preventDefault(); event.stopPropagation(); reset(); onExit(); return;
    }
    const result = loompadKey(chord, event.code, true, event.repeat, event.altKey);
    if (!result.handled) return;
    event.preventDefault(); event.stopPropagation(); chord = result.state;
    const candidate = result.choice === null ? null : visibleSlots[result.choice];
    if (candidate && result.accept) onAccept(candidate, 'word');
  }
  function keyup(event: KeyboardEvent): void {
    if (event.key === 'Alt' || !event.altKey) { reset(); return; }
    chord = loompadKey(chord, event.code, false, false, event.altKey).state;
  }
  onMount(() => {
    window.addEventListener('keydown', keydown, true);
    window.addEventListener('keyup', keyup, true);
    window.addEventListener('blur', reset);
    return () => {
      window.removeEventListener('keydown', keydown, true);
      window.removeEventListener('keyup', keyup, true);
      window.removeEventListener('blur', reset);
    };
  });
</script>

{#if modifierHeld && focused && !blocked && visibleSlots.some(Boolean)}
<div class="loompad" role="group" aria-label="Loompad">
  {#each visibleSlots as candidate, index}
    {#if candidate}
    <button class="loompad-choice" class:selected={candidate?.runId === selectedRunId}
      class:held={chord.choices.at(-1) === LOOMPAD_KEYS[index]}
      data-direction={LOOMPAD_KEYS[index].slice(3).toLowerCase()}
      type="button" disabled={!focused || blocked || !candidate?.text}
      aria-label={`${LOOMPAD_KEYS[index].slice(3)}: ${candidate ? loompadPrefix(candidate.text, 'word', visual) ?? '' : 'Waiting'}`}
      on:mouseenter={() => { if (candidate) onChoose(candidate); }} on:mousedown|preventDefault on:click={() => { if (candidate) onAccept(candidate, 'word'); }}>
      <kbd class="loompad-key">{LOOMPAD_KEYS[index].slice(3)}</kbd>
      <span>{candidate ? loompadPrefix(candidate.text, 'word', visual) ?? '…' : '…'}</span>
    </button>
    {/if}
  {/each}
</div>
{/if}
