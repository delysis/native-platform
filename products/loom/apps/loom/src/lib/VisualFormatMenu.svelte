<script context="module" lang="ts">
  export interface VisualFormattingEditor {
    captureFormattingSelection(): boolean;
    clearFormattingSelection(): void;
    focusPreservingSelection(): boolean;
    applyFormatting(action: VisualFormatAction, href?: string): boolean;
  }
</script>

<script lang="ts">
  import type { VisualFormatAction, VisualFormatState } from './visualFormatting';

  export let editor: VisualFormattingEditor | null | undefined;
  export let formatting: VisualFormatState;

  let menu: HTMLDetailsElement;
  let href = '';

  function preserveSelection(event: MouseEvent): void {
    editor?.captureFormattingSelection();
    // These controls operate on the editor selection. Prevent WebKit from
    // moving focus before the subsequent click invokes the command.
    event.preventDefault();
  }

  function run(action: VisualFormatAction, destination = ''): void {
    if (!editor?.applyFormatting(action, destination)) return;
    if (action === 'link') href = destination.trim();
  }

  function handleToggle(): void {
    if (menu.open) {
      editor?.captureFormattingSelection();
      href = formatting.linkHref;
    } else {
      editor?.clearFormattingSelection();
    }
  }

  export function close(refocus = true): void {
    menu.open = false;
    if (refocus) editor?.focusPreservingSelection();
    editor?.clearFormattingSelection();
  }

  export function isOpen(): boolean {
    return menu.open;
  }

  export function contains(target: Node): boolean {
    return menu.contains(target);
  }
</script>

<details class="format-menu" bind:this={menu} on:toggle={handleToggle}>
  <summary
    class="titlebar-button format-button"
    title="Format text"
    aria-label="Format text"
    on:mousedown={preserveSelection}
  >Aa</summary>
  <div class="format-popover" aria-label="Text formatting">
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
          on:mousedown={preserveSelection}
          on:click={() => run(style[0] as VisualFormatAction)}
        >{style[1]}</button>
      {/each}
    </div>
    <div class="format-command-row" aria-label="Inline formatting">
      <button class:active={formatting.bold} type="button" aria-label="Bold" title="Bold (⌘B)" on:mousedown={preserveSelection} on:click={() => run('bold')}><strong>B</strong></button>
      <button class:active={formatting.italic} type="button" aria-label="Italic" title="Italic (⌘I)" on:mousedown={preserveSelection} on:click={() => run('italic')}><em>I</em></button>
      <button class:active={formatting.blockquote} type="button" aria-label="Block quote" on:mousedown={preserveSelection} on:click={() => run('blockquote')}>“”</button>
      <button class:active={formatting.bulletList} type="button" aria-label="Bulleted list" on:mousedown={preserveSelection} on:click={() => run('bullet_list')}>•≡</button>
      <button class:active={formatting.orderedList} type="button" aria-label="Numbered list" on:mousedown={preserveSelection} on:click={() => run('ordered_list')}>1≡</button>
    </div>
    <div class="format-link-row">
      <input bind:value={href} aria-label="Link destination" placeholder="https://…" />
      <button type="button" disabled={formatting.selectionEmpty || !href.trim()} on:mousedown={preserveSelection} on:click={() => run('link', href)}>Link</button>
      <button type="button" disabled={formatting.selectionEmpty || !formatting.linkHref} on:mousedown={preserveSelection} on:click={() => run('unlink')}>Remove</button>
    </div>
  </div>
</details>
