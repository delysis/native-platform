<script lang="ts">
  import { tick } from 'svelte';
  import type { SetupChoices } from './workspaceTemplate';

  export let status = 'Your model is downloading. You can keep writing.';
  export let busy = false;
  export let error = '';
  export let onApply: (choices: SetupChoices) => void;
  export let onSkip: () => void;
  export let onTransfers: (trigger: HTMLElement) => void;
  export let onSettings: () => void;

  let step = 0;
  let chat = false;
  let suggestions = true;
  let heading: HTMLHeadingElement;

  async function advance(next: number): Promise<void> {
    step = next;
    await tick();
    heading?.focus();
  }
</script>

<aside class="setup-flow" aria-label="Set up your space">
  <div class="transfer-status">
    <p role="status">{status}</p>
    <button type="button" class="bare-button compact" on:click={(event) => onTransfers(event.currentTarget)}>Download details</button>
  </div>
  <div class="setup-question">
    <div class="step-label">Make this space yours · {step + 1} of 2</div>
    <h2 bind:this={heading} tabindex="-1">{step === 0 ? 'A page, or a page with chat?' : 'Suggestions as you write?'}</h2>
    {#if step === 0}
      <div class="choices" role="group" aria-label="Workspace">
        <button type="button" aria-pressed={!chat} class:selected={!chat} on:click={() => chat = false} disabled={busy}>
          <strong>Just the page</strong><span>A quiet place to write.</span>
        </button>
        <button type="button" aria-pressed={chat} class:selected={chat} on:click={() => chat = true} disabled={busy}>
          <strong>Chat beside it</strong><span>Talk through ideas alongside your writing.</span>
        </button>
      </div>
    {:else}
      <div class="choices" role="group" aria-label="Inline assistance">
        <button type="button" aria-pressed={suggestions} class:selected={suggestions} on:click={() => suggestions = true} disabled={busy}>
          <strong>Offer suggestions</strong><span>Preview continuations when you pause. You decide what stays.</span>
        </button>
        <button type="button" aria-pressed={!suggestions} class:selected={!suggestions} on:click={() => suggestions = false} disabled={busy}>
          <strong>Let me ask</strong><span>Write uninterrupted; ask for help when you want it.</span>
        </button>
      </div>
    {/if}
    {#if error}<p class="setup-error" role="alert">{error}</p><button type="button" class="bare-button compact" on:click={onSettings} disabled={busy}>Open settings file</button>{/if}
    <div class="setup-actions">
      <button type="button" class="bare-button compact" on:click={onSkip} disabled={busy}>Skip setup</button>
      <div>
        {#if step > 0}<button type="button" class="bare-button compact" on:click={() => void advance(0)} disabled={busy}>Back</button>{/if}
        <button type="button" class="primary-button compact" disabled={busy} on:click={() => step === 0 ? void advance(1) : onApply({ chat, suggestions })}>
          {busy ? 'Saving…' : step === 0 ? 'Next' : 'Use these settings'}
        </button>
      </div>
    </div>
    <p class="settings-note">Change these later in Settings.</p>
  </div>
</aside>

<style>
  .setup-flow { flex: 0 0 auto; display: grid; grid-template-columns: minmax(10rem, 1fr) minmax(24rem, 2fr); gap: 2rem; padding: 1rem max(1rem, 4vw); border-top: 1px solid var(--line); background: var(--paper); color: var(--ink); max-height: 45vh; overflow: auto; }
  .transfer-status { align-self: center; font-size: .85rem; max-width: 24rem; }
  .transfer-status p { margin: 0 0 .4rem; }
  .step-label, .settings-note { font-size: .72rem; color: var(--muted); }
  h2 { font-family: inherit; font-size: 1rem; margin: .25rem 0 .65rem; font-weight: 550; }
  h2:focus { outline: none; }
  .choices { display: grid; grid-template-columns: 1fr 1fr; gap: .6rem; }
  .choices button { text-align: left; padding: .65rem .8rem; border: 1px solid var(--line); border-radius: .5rem; background: transparent; color: inherit; cursor: pointer; }
  .choices button.selected { border-color: var(--ink); background: var(--moss-soft); }
  .choices strong { display: block; font-size: .85rem; font-weight: 550; }
  .choices span { display: block; font-size: .75rem; color: var(--muted); margin-top: .2rem; }
  .setup-actions { display: flex; justify-content: space-between; align-items: center; margin-top: .6rem; gap: .8rem; }
  .setup-actions > div { display: flex; gap: .5rem; }
  .settings-note { margin: .35rem 0 0; }
  .setup-error { font-size: .8rem; margin: .5rem 0; }
  @media (max-width: 760px) { .setup-flow { grid-template-columns: 1fr; gap: .6rem; } .transfer-status { max-width: none; display: flex; gap: 1rem; align-items: center; } .transfer-status p { margin: 0; } }
</style>
