<script lang="ts">
  import { tick } from 'svelte';
  import {
    boundedDocumentTitleInput,
    createDocumentRenameCompositionGuard,
    releaseDocumentRenameAndRestoreFocus
  } from './documentContextActions';

  let input: HTMLInputElement;
  let busy = false;
  let witness = 'idle';
  let compositionInput: HTMLInputElement;
  let compositionTitle = 'Rename me';
  let compositionOutcome = 'editing';
  let compositionOpen = true;
  const composition = createDocumentRenameCompositionGuard();

  async function simulateFailedRename(): Promise<void> {
    busy = true;
    await tick();
    const disabledBeforeRelease = input.disabled;
    const restored = await releaseDocumentRenameAndRestoreFocus(
      () => { busy = false; },
      tick,
      () => input
    );
    witness = [
      `disabled-before-release:${disabledBeforeRelease}`,
      `restored:${restored}`,
      `active:${document.activeElement === input}`,
      `selection:${input.selectionStart}-${input.selectionEnd}`
    ].join(';');
  }

  function synchronizeCompositionTitle(): void {
    const bounded = boundedDocumentTitleInput(compositionInput.value);
    if (compositionInput.value !== bounded) compositionInput.value = bounded;
    compositionTitle = bounded;
  }

  function finishRename(source: 'enter' | 'blur' | 'deferred-blur'): void {
    if (!compositionOpen) return;
    synchronizeCompositionTitle();
    compositionOutcome = `committed:${source}:${compositionTitle}`;
    compositionOpen = false;
    composition.reset();
  }

  function handleCompositionKeydown(event: KeyboardEvent): void {
    const compositionOwnsCommand = composition.ownsCommandKey(event);
    if (
      compositionOwnsCommand &&
      (event.key === 'Escape' || event.key === 'Enter')
    ) {
      event.stopPropagation();
      return;
    }
    if (event.key === 'Escape') {
      event.preventDefault();
      compositionOutcome = 'cancelled';
      compositionOpen = false;
      composition.reset();
      return;
    }
    if (event.key === 'Enter') {
      event.preventDefault();
      finishRename('enter');
    }
  }

  function handleCompositionEnd(): void {
    synchronizeCompositionTitle();
    if (composition.finish()) finishRename('deferred-blur');
  }

  function handleCompositionBlur(): void {
    if (compositionOpen && composition.blurShouldCommit()) finishRename('blur');
  }
</script>

<input
  bind:this={input}
  value="Rename me"
  disabled={busy}
  aria-label="Document title"
/>
<button
  type="button"
  on:mousedown|preventDefault
  on:click={() => void simulateFailedRename()}
>Simulate failed rename</button>
<output aria-label="Rename focus witness">{witness}</output>

{#if compositionOpen}
  <input
    bind:this={compositionInput}
    bind:value={compositionTitle}
    aria-label="IME title field"
    on:input={synchronizeCompositionTitle}
    on:compositionstart={() => composition.start()}
    on:compositionend={handleCompositionEnd}
    on:keydown={handleCompositionKeydown}
    on:blur={handleCompositionBlur}
  />
{/if}
<output aria-label="Rename composition outcome">{compositionOutcome}</output>
