<script context="module" lang="ts">
  export interface VisualFormattingEditor {
    captureFormattingSelection(focusTransitionFrom?: EventTarget | null): boolean;
    clearFormattingSelection(): void;
    focusPreservingSelection(): boolean;
    applyFormatting(action: VisualFormatAction, href?: string): boolean;
    formattingDiagnostic?(): string;
  }
</script>

<script lang="ts">
  import { onDestroy } from 'svelte';
  import type { VisualFormatAction, VisualFormatState } from './visualFormatting';

  export let editor: VisualFormattingEditor | null | undefined;
  export let formatting: VisualFormatState;
  export let onCommandResult: (
    action: VisualFormatAction,
    applied: boolean,
    diagnostic: string
  ) => void = () => {};

  let menu: HTMLDivElement;
  let open = false;
  let href = '';
  let leaseEditor: VisualFormattingEditor | null = null;
  let observedEditor: VisualFormattingEditor | null = editor ?? null;

  function captureSelection(focusTransitionFrom: EventTarget | null = null): boolean {
    const currentEditor = editor ?? null;
    if (!currentEditor) return false;
    if (leaseEditor && leaseEditor !== currentEditor) {
      leaseEditor.clearFormattingSelection();
      leaseEditor = null;
    }
    if (!currentEditor.captureFormattingSelection(focusTransitionFrom)) {
      if (leaseEditor === currentEditor) releaseSelection();
      return false;
    }
    leaseEditor = currentEditor;
    return true;
  }

  function releaseSelection(): void {
    const capturedEditor = leaseEditor;
    leaseEditor = null;
    capturedEditor?.clearFormattingSelection();
  }

  function preserveSelection(event: PointerEvent): void {
    captureSelection();
    // These controls operate on the editor selection. Prevent WebKit from
    // moving focus before the subsequent click invokes the command.
    event.preventDefault();
  }

  function preserveSelectionFromFocus(event: FocusEvent): void {
    captureSelection(event.relatedTarget);
  }

  function run(action: VisualFormatAction, destination = ''): void {
    const currentEditor = editor ?? null;
    const applied = Boolean(
      currentEditor &&
      currentEditor === leaseEditor &&
      currentEditor.applyFormatting(action, destination)
    );
    onCommandResult(
      action,
      applied,
      currentEditor?.formattingDiagnostic?.() ?? 'editor_unavailable'
    );
    if (!applied) return;
    if (action === 'link') href = destination.trim();
  }

  function toggleOpen(): void {
    if (open) {
      close();
      return;
    }
    if (!captureSelection()) return;
    href = formatting.linkHref;
    open = true;
  }

  export function close(refocus = true): void {
    open = false;
    const capturedEditor = leaseEditor;
    if (refocus && capturedEditor && capturedEditor === editor) {
      capturedEditor.focusPreservingSelection();
    }
    releaseSelection();
  }

  export function isOpen(): boolean {
    return open;
  }

  export function contains(target: Node): boolean {
    return menu.contains(target);
  }

  // Component ownership is the outer lifetime of an editor selection lease.
  // Never leave the old owner's palette open or let its captured selection
  // follow this component to a replacement editor.
  $: {
    const currentEditor = editor ?? null;
    if (currentEditor !== observedEditor) {
      open = false;
      href = '';
      releaseSelection();
      observedEditor = currentEditor;
    }
  }

  onDestroy(releaseSelection);
</script>

<div class="format-menu" bind:this={menu} on:focusin={preserveSelectionFromFocus}>
  <button
    class="titlebar-button format-button"
    type="button"
    title="Format text"
    aria-label="Format text"
    aria-controls="visual-format-popover"
    aria-expanded={open}
    on:pointerdown={preserveSelection}
    on:click={toggleOpen}
  >Aa</button>
  {#if open}
  <div id="visual-format-popover" class="format-popover" aria-label="Text formatting">
    <div class="format-style-grid" aria-label="Paragraph style">
      {#each [
        ['body', 'Body'],
        ['title', 'Title'],
        ['heading', 'Heading'],
        ['subheading', 'Subheading']
      ] as style}
        <button
          class:active={formatting.block === style[0]}
          type="button"
          on:pointerdown={preserveSelection}
          on:click={() => run(style[0] as VisualFormatAction)}
        >{style[1]}</button>
      {/each}
    </div>
    <div class="format-command-row" aria-label="Inline formatting">
      <button class:active={formatting.bold} type="button" aria-label="Bold" title="Bold (⌘B)" on:pointerdown={preserveSelection} on:click={() => run('bold')}><strong>B</strong></button>
      <button class:active={formatting.italic} type="button" aria-label="Italic" title="Italic (⌘I)" on:pointerdown={preserveSelection} on:click={() => run('italic')}><em>I</em></button>
      <button class:active={formatting.blockquote} type="button" aria-label="Block quote" on:pointerdown={preserveSelection} on:click={() => run('blockquote')}>“”</button>
      <button class:active={formatting.bulletList} type="button" aria-label="Bulleted list" on:pointerdown={preserveSelection} on:click={() => run('bullet_list')}>•≡</button>
      <button class:active={formatting.orderedList} type="button" aria-label="Numbered list" on:pointerdown={preserveSelection} on:click={() => run('ordered_list')}>1≡</button>
    </div>
    <div class="format-link-row">
      <input bind:value={href} aria-label="Link destination" placeholder="https://…" />
      <button type="button" disabled={formatting.selectionEmpty || !href.trim()} on:pointerdown={preserveSelection} on:click={() => run('link', href)}>Link</button>
      <button type="button" disabled={formatting.selectionEmpty || !formatting.linkHref} on:pointerdown={preserveSelection} on:click={() => run('unlink')}>Remove</button>
    </div>
  </div>
  {/if}
</div>
