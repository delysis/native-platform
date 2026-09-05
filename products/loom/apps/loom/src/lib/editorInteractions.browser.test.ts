import { mount, unmount } from 'svelte';
import { afterEach, describe, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import '../app.css';
import EditorBrowserHarness from './EditorBrowserHarness.svelte';
import SourceEditorBrowserHarness from './SourceEditorBrowserHarness.svelte';
import type { CompletionCandidate } from './completionSession';

let mounted: ReturnType<typeof mount> | null = null;

afterEach(async () => {
  if (mounted) await unmount(mounted);
  mounted = null;
  document.body.replaceChildren();
});

function render(
  initialValue: string,
  completionCandidates: CompletionCandidate[] = [],
  modes: {
    autocomplete?: boolean;
    shuttle?: boolean;
    completionFrames?: readonly (readonly CompletionCandidate[])[];
    acceptImageAttachments?: boolean;
    onImageAttachments?: (files: readonly File[]) => Promise<readonly string[]>;
    onImageAttachmentsCommitted?: (count: number) => void;
    onImageAttachmentError?: (message: string) => void;
    resolveImageAssetUrl?: (markdownPath: string) => string | null;
  } = {}
): void {
  const target = document.createElement('div');
  document.body.append(target);
  mounted = mount(EditorBrowserHarness, {
    target,
    props: { initialValue, completionCandidates, ...modes }
  });
}

function renderSource(
  initialValue: string,
  completionCandidates: CompletionCandidate[],
  onImageAttachments?: (files: readonly File[]) => Promise<readonly string[]>,
  onImageAttachmentError?: (message: string) => void,
  onImageAttachmentsCommitted?: (count: number) => void
): void {
  const target = document.createElement('div');
  document.body.append(target);
  mounted = mount(SourceEditorBrowserHarness, {
    target,
    props: {
      initialValue,
      completionCandidates,
      onImageAttachments,
      onImageAttachmentError,
      onImageAttachmentsCommitted
    }
  });
}

function fileTransfer(name: string, type: string): DataTransfer {
  const transfer = new DataTransfer();
  transfer.items.add(new File([new Uint8Array([1, 2, 3, 4])], name, { type }));
  return transfer;
}

function imageTransfer(name = 'Sketch.png'): DataTransfer {
  return fileTransfer(name, 'image/png');
}

function dispatchTransfer(
  target: EventTarget,
  type: 'paste' | 'dragover' | 'drop',
  transfer: DataTransfer
): ClipboardEvent | DragEvent {
  const event = type === 'paste'
    ? new ClipboardEvent(type, { bubbles: true, cancelable: true })
    : new DragEvent(type, { bubbles: true, cancelable: true });
  Object.defineProperty(event, type === 'paste' ? 'clipboardData' : 'dataTransfer', {
    configurable: true,
    value: transfer
  });
  target.dispatchEvent(event);
  return event;
}

describe('real WebKit editor interactions', () => {
  function serializedMarkdown(): string {
    return page.getByRole('status', { name: 'Serialized Markdown' }).element().textContent ?? '';
  }

  function fourChoiceCompletion(targetByte = 5): CompletionCandidate[] {
    return [
      { candidateId: 'a', presentationKey: 'a:1', text: ' world', runId: 'run-a', targetByte, insertsOnAccept: true },
      { candidateId: 'b', presentationKey: 'b:1', text: ' there', runId: 'run-b', targetByte, insertsOnAccept: true },
      { candidateId: 'c', presentationKey: 'c:1', text: ' again', runId: 'run-c', targetByte, insertsOnAccept: true },
      { candidateId: 'd', presentationKey: 'd:1', text: ' onward', runId: 'run-d', targetByte, insertsOnAccept: true }
    ];
  }

  function dispatchOptionDown(target: EventTarget): void {
    dispatchKey(target, 'keydown', 'Alt', 'AltLeft', true);
  }

  function dispatchOptionUp(target: EventTarget): void {
    dispatchKey(target, 'keyup', 'Alt', 'AltLeft', false);
  }

  function dispatchKey(
    target: EventTarget,
    type: 'keydown' | 'keyup',
    key: string,
    code: string,
    altKey: boolean
  ): void {
    target.dispatchEvent(new KeyboardEvent(type, {
      key,
      code,
      altKey,
      bubbles: true,
      cancelable: true
    }));
  }

  function completionFanIsVisible(): boolean {
    const fan = document.querySelector<HTMLElement>(
      '[role="listbox"][aria-label="Completion suggestions"]'
    );
    return Boolean(fan && getComputedStyle(fan).display !== 'none' && fan.getClientRects().length > 0);
  }

  async function paintTwice(): Promise<void> {
    await new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));
    await new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));
  }

  it('renders native audio as a waveform and playback control instead of a paperclip', async () => {
    const id = 'a'.repeat(64);
    const sha = 'b'.repeat(64);
    render(`![Audio: fixture.wav](loom-attachment:${id}/${sha} "loom-waveform:001080ff")`, [], {
      resolveImageAssetUrl: () => null
    });
    await expect.element(page.getByRole('textbox', { name: 'Manuscript editor' })).toBeVisible();
    const audio = document.querySelector('audio');
    expect(audio).not.toBeNull();
    expect(audio?.controls).toBe(true);
    expect(document.querySelectorAll('.audio-waveform i')).toHaveLength(4);
    expect(document.querySelector('.inline-audio-card')?.textContent).not.toContain('📎');
  });

  it('makes the full visual writing body an editable hit target', async () => {
    render('');
    const editorLocator = page.getByRole('textbox', { name: 'Manuscript editor' });
    await expect.element(editorLocator).toBeInTheDocument();
    const editor = editorLocator.element();
    const pane = editor.closest('.editor-pane');
    expect(pane).not.toBeNull();
    const editorRect = editor.getBoundingClientRect();
    const paneRect = pane!.getBoundingClientRect();
    expect(Math.abs(editorRect.left - paneRect.left)).toBeLessThanOrEqual(1);
    expect(Math.abs(editorRect.right - paneRect.right)).toBeLessThanOrEqual(1);

    await editorLocator.click({
      position: { x: editorRect.width - 4, y: editorRect.height - 4 }
    });
    expect(document.activeElement).toBe(editor);
    await userEvent.keyboard('Anywhere');
    await expect.poll(serializedMarkdown).toBe('Anywhere');
  });

  it('makes the full Markdown writing body an editable hit target', async () => {
    renderSource('', []);
    const editorLocator = page.getByRole('textbox');
    await expect.element(editorLocator).toBeInTheDocument();
    const editor = editorLocator.element() as HTMLTextAreaElement;
    const pane = editor.closest('.editor-pane');
    expect(pane).not.toBeNull();
    const editorRect = editor.getBoundingClientRect();
    const paneRect = pane!.getBoundingClientRect();
    expect(Math.abs(editorRect.left - paneRect.left)).toBeLessThanOrEqual(1);
    expect(Math.abs(editorRect.right - paneRect.right)).toBeLessThanOrEqual(1);

    await editorLocator.click({
      position: { x: editorRect.width - 4, y: Math.max(1, editorRect.height - 4) }
    });
    expect(document.activeElement).toBe(editor);
    await userEvent.keyboard('Anywhere');
    await expect.element(page.getByRole('status', { name: 'Source Markdown' }))
      .toHaveTextContent('Anywhere');
  });

  it('inserts dictated text at the preserved visual and Markdown carets', async () => {
    render('hello world');
    const visual = page.getByRole('textbox', { name: 'Manuscript editor' });
    await visual.click();
    const visualElement = visual.element();
    const visualText = visualElement.querySelector('p')?.firstChild;
    expect(visualText).not.toBeNull();
    const range = document.createRange();
    range.setStart(visualText!, 0);
    range.collapse(true);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
    document.dispatchEvent(new Event('selectionchange'));
    await paintTwice();
    await page.getByRole('button', { name: 'Insert transcript' }).click();
    await expect.poll(serializedMarkdown).toBe(' dictatedhello world');

    if (mounted) await unmount(mounted);
    mounted = null;
    document.body.replaceChildren();
    renderSource('hello world', []);
    const source = page.getByRole('textbox');
    await source.click();
    (source.element() as HTMLTextAreaElement).setSelectionRange(0, 0);
    source.element().dispatchEvent(new Event('select', { bubbles: true }));
    await page.getByRole('button', { name: 'Insert transcript' }).click();
    await expect.poll(
      () => page.getByRole('status', { name: 'Source Markdown' }).element().textContent
    ).toBe(' dictatedhello world');
  });

  it('preserves a paused terminal separator for later typing and palette commands', async () => {
    const keyboard = userEvent.setup();
    render('Something');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await editor.click();
    await keyboard.keyboard('{End} ');
    await expect.poll(serializedMarkdown).toBe('Something ');

    // Cross both the editor projection debounce and ProseMirror's history
    // grouping window. The next word must retain the writer-entered separator.
    await new Promise((resolve) => window.setTimeout(resolve, 650));
    await keyboard.keyboard('else');
    await expect.poll(serializedMarkdown).toBe('Something else');
    await new Promise((resolve) => window.setTimeout(resolve, 650));
    await keyboard.keyboard('{Meta>}z{/Meta}');
    await expect.poll(serializedMarkdown).toBe('Something ');
    await keyboard.keyboard('{Meta>}{Shift>}z{/Shift}{/Meta}');
    await expect.poll(serializedMarkdown).toBe('Something else');

    await page.getByRole('button', { name: 'Format text' }).click();
    await page.getByRole('button', { name: 'Title' }).click();
    await expect.poll(serializedMarkdown).toBe('# Something else');
    await page.getByRole('button', { name: 'Body' }).click();
    await expect.poll(serializedMarkdown).toBe('Something else');
    await keyboard.cleanup();
  });

  it('withdraws stale selection evidence until typed document state settles', async () => {
    render('Something');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    const witness = () => JSON.parse(
      page.getByRole('status', { name: 'Visual Selection Witness' }).element().textContent ?? '{}'
    );
    await editor.click();
    await userEvent.keyboard('{Meta>}a{/Meta}Replacement');

    expect(witness()).toMatchObject({ available: false });
    await expect.poll(serializedMarkdown).toBe('Replacement');
    await expect.poll(witness).toMatchObject({
      available: true,
      empty: true,
      allVisibleText: false,
      caretAtEnd: true,
      caretByteOffset: 'Replacement'.length
    });
    expect(witness().epoch).toBeGreaterThan(0);
  });

  it('wires every Aa formatting family to exact Markdown', async () => {
    render('Words');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await editor.click();
    await userEvent.keyboard('{Meta>}a{/Meta}');
    const formatButton = page.getByRole('button', { name: 'Format text' });
    await expect.element(formatButton).toHaveAttribute('aria-expanded', 'false');
    await formatButton.click();
    await expect.element(formatButton).toHaveAttribute('aria-expanded', 'true');

    for (const [label, expected] of [
      ['Title', '# Words'],
      ['Heading', '## Words'],
      ['Subheading', '### Words'],
      ['Body', 'Words']
    ] as const) {
      await page.getByRole('button', { name: label, exact: true }).click();
      await expect.poll(serializedMarkdown).toBe(expected);
    }

    for (const [label, formatted, restored] of [
      ['Bold', '**Words**', 'Words'],
      ['Italic', '*Words*', 'Words'],
      ['Block quote', '> Words', 'Words'],
      ['Bulleted list', '* Words', 'Words'],
      ['Numbered list', '1. Words', 'Words']
    ] as const) {
      await page.getByRole('button', { name: label }).click();
      await expect.poll(serializedMarkdown).toBe(formatted);
      await page.getByRole('button', { name: label }).click();
      await expect.element(page.getByRole('status', { name: 'Formatting Result' }))
        .toHaveTextContent('applied');
      await expect.poll(serializedMarkdown).toBe(restored);
    }

    await userEvent.fill(
      page.getByRole('textbox', { name: 'Link destination' }),
      'https://example.com'
    );
    await page.getByRole('button', { name: 'Link' }).click();
    await expect.poll(serializedMarkdown).toBe('[Words](https://example.com)');
    await page.getByRole('button', { name: 'Remove' }).click();
    await expect.poll(serializedMarkdown).toBe('Words');
  });

  it('preserves a full selection across repeated AX-style inline toggles', async () => {
    render('Words');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    const witness = () => JSON.parse(
      page.getByRole('status', { name: 'Visual Selection Witness' }).element().textContent ?? '{}'
    );
    await editor.click();
    await userEvent.keyboard('{Meta>}a{/Meta}');

    const formatButton = page.getByRole('button', { name: 'Format text' }).element() as HTMLButtonElement;
    formatButton.focus();
    formatButton.click();
    await expect.element(page.getByRole('button', { name: 'Format text' }))
      .toHaveAttribute('aria-expanded', 'true');
    const italicButton = page.getByRole('button', { name: 'Italic' }).element() as HTMLButtonElement;

    for (const expected of ['*Words*', 'Words']) {
      italicButton.focus();
      italicButton.click();
      await expect.poll(serializedMarkdown).toBe(expected);
      await new Promise((resolve) => window.setTimeout(resolve, 1_000));
      expect(document.activeElement).toBe(editor.element());
      expect(document.getSelection()?.toString().trim()).toBe('Words');
      expect(witness()).toMatchObject({
        available: true,
        empty: false,
        allVisibleText: true,
        caretAtEnd: false,
        caretByteOffset: null
      });
      await userEvent.keyboard('{Meta>}a{/Meta}');
    }
  });

  it('applies formatting through click-only semantic activation after a live selection change', async () => {
    render('Words');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await editor.click();

    const formatButton = page.getByRole('button', { name: 'Format text' });
    const formatButtonElement = formatButton.element() as HTMLButtonElement;
    expect(formatButtonElement.tagName).toBe('BUTTON');
    formatButtonElement.click();
    await expect.element(formatButton).toHaveAttribute('aria-expanded', 'true');

    await userEvent.keyboard('{Meta>}a{/Meta}');
    expect(document.getSelection()?.toString().trim()).toBe('Words');
    (page.getByRole('button', { name: 'Bold' }).element() as HTMLButtonElement).click();
    await expect.poll(serializedMarkdown).toBe('**Words**');
    await new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));
    expect(document.activeElement).toBe(editor.element());
    expect(document.getSelection()?.toString().trim()).toBe('Words');
  });

  it('authenticates an Accessibility-style editor-to-palette focus handoff', async () => {
    render('Words');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await editor.click();
    await userEvent.keyboard('{End}');

    const formatButton = page.getByRole('button', { name: 'Format text' });
    const formatButtonElement = formatButton.element() as HTMLButtonElement;
    // AXPress may publish focus on the native button before dispatching its
    // semantic click. This exact sequence must retain the live editor caret
    // without reopening the broader stale-selection authority that the lease
    // guard intentionally removed.
    formatButtonElement.focus();
    expect(document.activeElement).toBe(formatButtonElement);
    formatButtonElement.click();
    await expect.element(formatButton).toHaveAttribute('aria-expanded', 'true');

    const titleButton = page.getByRole('button', { name: 'Title' }).element() as HTMLButtonElement;
    titleButton.focus();
    titleButton.click();
    await expect.poll(serializedMarkdown).toBe('# Words');
    await new Promise((resolve) => window.setTimeout(resolve, 1_000));
    expect(document.activeElement).toBe(editor.element());
    expect(document.getSelection()?.isCollapsed).toBe(true);
    expect(document.getSelection()?.focusOffset).toBe('Words'.length);
    await expect.poll(() => JSON.parse(
      page.getByRole('status', { name: 'Visual Selection Witness' }).element().textContent ?? '{}'
    )).toMatchObject({
      available: true,
      empty: true,
      allVisibleText: false,
      caretAtEnd: true,
      caretByteOffset: '# Words'.length
    });
  });

  it('restores the immutable formatted caret after late WebKit selection reconciliation', async () => {
    render('Words');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await editor.click();
    await userEvent.keyboard('{End}');

    const formatButton = page.getByRole('button', { name: 'Format text' });
    (formatButton.element() as HTMLButtonElement).click();
    await expect.element(formatButton).toHaveAttribute('aria-expanded', 'true');
    (page.getByRole('button', { name: 'Title' }).element() as HTMLButtonElement).click();
    expect(document.getSelection()?.isCollapsed).toBe(true);
    expect(document.getSelection()?.focusOffset).toBe('Words'.length);

    const range = document.createRange();
    range.selectNodeContents(editor.element());
    const selection = document.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
    document.dispatchEvent(new Event('selectionchange'));
    expect(document.getSelection()?.toString().trim()).toBe('Words');

    await expect.poll(serializedMarkdown).toBe('# Words');
    await new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));
    await new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));
    expect(document.activeElement).toBe(editor.element());
    expect(document.getSelection()?.isCollapsed).toBe(true);
    expect(document.getSelection()?.focusOffset).toBe('Words'.length);
  });

  it('never lets a delayed palette repair overwrite later keyboard navigation', async () => {
    render('Words');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await editor.click();
    await userEvent.keyboard('{End}');

    const formatButton = page.getByRole('button', { name: 'Format text' });
    (formatButton.element() as HTMLButtonElement).click();
    await expect.element(formatButton).toHaveAttribute('aria-expanded', 'true');
    (page.getByRole('button', { name: 'Title' }).element() as HTMLButtonElement).click();
    await expect.poll(serializedMarkdown).toBe('# Words');
    expect(document.activeElement).toBe(editor.element());

    await userEvent.keyboard('{ArrowLeft}');
    await paintTwice();

    expect(document.activeElement).toBe(editor.element());
    expect(document.getSelection()?.isCollapsed).toBe(true);
    expect(document.getSelection()?.focusOffset).toBe('Words'.length - 1);
  });

  it('never lets a delayed palette repair overwrite later pointer selection intent', async () => {
    render('Words');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await editor.click();
    await userEvent.keyboard('{End}');

    const formatButton = page.getByRole('button', { name: 'Format text' });
    (formatButton.element() as HTMLButtonElement).click();
    await expect.element(formatButton).toHaveAttribute('aria-expanded', 'true');
    (page.getByRole('button', { name: 'Title' }).element() as HTMLButtonElement).click();
    await expect.poll(serializedMarkdown).toBe('# Words');

    editor.element().dispatchEvent(new PointerEvent('pointerdown', {
      bubbles: true,
      cancelable: true,
      pointerType: 'mouse'
    }));
    const range = document.createRange();
    range.selectNodeContents(editor.element());
    const selection = document.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
    document.dispatchEvent(new Event('selectionchange'));
    await paintTwice();

    expect(document.getSelection()?.toString().trim()).toBe('Words');
  });

  it('invalidates a palette selection lease when external authority replaces the document', async () => {
    render('Words');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await editor.click();
    await userEvent.keyboard('{Meta>}a{/Meta}');
    await page.getByRole('button', { name: 'Format text' }).click();

    await page.getByRole('button', { name: 'Replace manuscript externally' }).click();
    await expect.poll(serializedMarkdown).toBe('External authority');
    await paintTwice();
    (page.getByRole('button', { name: 'Bold' }).element() as HTMLButtonElement).click();

    await expect.element(page.getByRole('status', { name: 'Formatting Result' }))
      .toHaveTextContent('bold:refused');
    expect(serializedMarkdown()).toBe('External authority');
  });

  it('closes the palette and clears its lease when the editor owner changes', async () => {
    render('Words');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await editor.click();
    await userEvent.keyboard('{Meta>}a{/Meta}');
    const formatButton = page.getByRole('button', { name: 'Format text' });
    await formatButton.click();
    await expect.element(formatButton).toHaveAttribute('aria-expanded', 'true');

    await page.getByRole('button', { name: 'Disconnect formatting editor' }).click();
    await expect.element(formatButton).toHaveAttribute('aria-expanded', 'false');
    (page.getByRole('button', { name: 'Invoke direct format command' }).element() as HTMLButtonElement)
      .click();

    await expect.element(page.getByRole('status', { name: 'Formatting Result' }))
      .toHaveTextContent('bold:refused');
    expect(serializedMarkdown()).toBe('Words');
  });

  it('clears the palette lease when the menu is destroyed', async () => {
    render('Words');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await editor.click();
    await userEvent.keyboard('{Meta>}a{/Meta}');
    await page.getByRole('button', { name: 'Format text' }).click();

    await page.getByRole('button', { name: 'Destroy formatting menu' }).click();
    await expect.element(page.getByRole('button', { name: 'Format text' }))
      .not.toBeInTheDocument();
    (page.getByRole('button', { name: 'Invoke direct format command' }).element() as HTMLButtonElement)
      .click();

    await expect.element(page.getByRole('status', { name: 'Formatting Result' }))
      .toHaveTextContent('bold:refused');
    expect(serializedMarkdown()).toBe('Words');
  });

  it('keeps browser Select All inside every structural formatting wrapper', async () => {
    render('Words');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await editor.click();
    const formatButton = page.getByRole('button', { name: 'Format text' });
    (formatButton.element() as HTMLButtonElement).click();
    await expect.element(formatButton).toHaveAttribute('aria-expanded', 'true');

    for (const [label, action, formatted] of [
      ['Block quote', 'blockquote', '> Words'],
      ['Bulleted list', 'bullet_list', '* Words'],
      ['Numbered list', 'ordered_list', '1. Words']
    ] as const) {
      for (const expected of [formatted, 'Words']) {
        await userEvent.keyboard('{Meta>}a{/Meta}');
        (page.getByRole('button', { name: label }).element() as HTMLButtonElement).click();
        await expect.poll(serializedMarkdown).toBe(expected);
        await paintTwice();
        await expect.element(page.getByRole('status', { name: 'Formatting Result' }))
          .toHaveTextContent(`${action}:applied:TextSelection`);
        expect(document.activeElement).toBe(editor.element());
        expect(document.getSelection()?.toString().trim()).toBe('Words');
      }
    }
  });

  it('keeps one terminal prose space outside inline palette formatting', async () => {
    const keyboard = userEvent.setup();
    render('Words ');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await editor.click();
    await keyboard.keyboard('{Meta>}a{/Meta}');
    await page.getByRole('button', { name: 'Format text' }).click();

    for (const [label, formatted] of [
      ['Bold', '**Words** '],
      ['Italic', '*Words* ']
    ] as const) {
      await page.getByRole('button', { name: label }).click();
      await expect.poll(serializedMarkdown).toBe(formatted);
      await page.getByRole('button', { name: label }).click();
      await expect.poll(serializedMarkdown).toBe('Words ');
    }

    await userEvent.fill(
      page.getByRole('textbox', { name: 'Link destination' }),
      'https://example.com'
    );
    await page.getByRole('button', { name: 'Link' }).click();
    await expect.poll(serializedMarkdown).toBe('[Words](https://example.com) ');
    await page.getByRole('button', { name: 'Remove' }).click();
    await expect.poll(serializedMarkdown).toBe('Words ');
    await keyboard.cleanup();
  });

  it('renders a completion at the exact byte after a terminal prose space', async () => {
    render('Something ', [
      { candidateId: 'a', presentationKey: 'a:1', text: 'lingers here', runId: 'run-a', targetByte: 10, insertsOnAccept: true }
    ]);
    await expect.element(page.getByText('lingers here', { exact: true }).first()).toBeVisible();
    await userEvent.keyboard('{Alt>}{ArrowRight}{/Alt}');
    await expect.poll(serializedMarkdown).toBe('Something lingers');
    await expect.element(page.getByText('here', { exact: true }).first()).toBeVisible();
    await expect.element(page.getByRole('status', { name: 'Generation Requests' }))
      .toHaveTextContent('0');
  });

  it('gives a visible list-item completion priority over Tab indentation', async () => {
    render('1. one\n2. two', [
      {
        candidateId: 'list-completion',
        presentationKey: 'list-completion:1',
        text: ' continued',
        runId: 'run-list-completion',
        targetByte: 13,
        insertsOnAccept: true
      }
    ]);
    await expect.element(page.getByText(' continued', { exact: true }).first()).toBeVisible();

    await userEvent.keyboard('{Tab}');

    await expect.poll(serializedMarkdown).toBe('1. one\n2. two continued');
  });

  it('ingests pasted image files and resolves only their rendered visual URL', async () => {
    const received: string[] = [];
    const committed: number[] = [];
    const markdownPath = `../assets/${'a'.repeat(64)}.png`;
    render('', [], {
      onImageAttachments: async (files) => {
        received.push(...files.map((file) => file.name));
        return [`![Sketch](${markdownPath})`];
      },
      onImageAttachmentsCommitted: (count) => committed.push(count),
      resolveImageAssetUrl: (path) => path === markdownPath ? 'asset://resolved-sketch' : null
    });
    const editorLocator = page.getByRole('textbox', { name: 'Manuscript editor' });
    await expect.element(editorLocator).toBeInTheDocument();
    const editor = editorLocator.element();
    await new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));

    const event = dispatchTransfer(editor, 'paste', imageTransfer());

    expect(event.defaultPrevented).toBe(true);
    await expect.poll(serializedMarkdown).toBe(`![Sketch](${markdownPath})`);
    expect(received).toEqual(['Sketch.png']);
    expect(committed).toEqual([1]);
    await expect.element(page.getByRole('status', { name: 'Attachment Commit Witness' }))
      .toHaveTextContent(`1:![Sketch](${markdownPath})`);
    expect(editor.querySelector('img')?.getAttribute('src')).toBe('asset://resolved-sketch');
  });

  it('does not claim image transfer events when an embedding pane owns attachments', async () => {
    let attachmentCalls = 0;
    render('context note', [], {
      acceptImageAttachments: false,
      onImageAttachments: async () => {
        attachmentCalls += 1;
        return [];
      }
    });
    const editorLocator = page.getByRole('textbox', { name: 'Manuscript editor' });
    await expect.element(editorLocator).toBeInTheDocument();
    const editor = editorLocator.element();

    expect(dispatchTransfer(editor, 'dragover', imageTransfer()).defaultPrevented).toBe(false);
    expect(dispatchTransfer(editor, 'drop', imageTransfer()).defaultPrevented).toBe(false);
    expect(dispatchTransfer(editor, 'paste', imageTransfer()).defaultPrevented).toBe(false);
    expect(attachmentCalls).toBe(0);
  });

  it('fails closed on generic or empty-MIME file drops before WebKit default handling', async () => {
    const errors: string[] = [];
    render('hello', [], { onImageAttachmentError: (message) => errors.push(message) });
    const editorLocator = page.getByRole('textbox', { name: 'Manuscript editor' });
    await expect.element(editorLocator).toBeInTheDocument();
    const editor = editorLocator.element();
    const generic = fileTransfer('Notes.pdf', 'application/pdf');
    const unknownImage = fileTransfer('Camera.png', '');

    expect(dispatchTransfer(editor, 'dragover', generic).defaultPrevented).toBe(true);
    expect(dispatchTransfer(editor, 'drop', generic).defaultPrevented).toBe(true);
    expect(dispatchTransfer(editor, 'dragover', unknownImage).defaultPrevented).toBe(true);
    expect(dispatchTransfer(editor, 'drop', unknownImage).defaultPrevented).toBe(true);

    await expect.poll(serializedMarkdown).toBe('hello');
    expect(errors).toHaveLength(2);
    expect(errors.every((message) => message.includes('could not verify'))).toBe(true);
  });

  it('blocks ephemeral browser image markup instead of persisting its URL', async () => {
    const errors: string[] = [];
    render('hello', [], { onImageAttachmentError: (message) => errors.push(message) });
    const editorLocator = page.getByRole('textbox', { name: 'Manuscript editor' });
    await expect.element(editorLocator).toBeInTheDocument();
    const editor = editorLocator.element();
    const transfer = new DataTransfer();
    transfer.setData('text/html', '<img src="blob:https://loom.invalid/ephemeral">');

    const event = dispatchTransfer(editor, 'paste', transfer);

    expect(event.defaultPrevented).toBe(true);
    await expect.poll(serializedMarkdown).toBe('hello');
    expect(errors).toEqual([
      'Loom could not read image bytes from that paste. Copy or drag the original image file instead.'
    ]);
  });

  it('keeps an unresolved visual Markdown image inert while retaining its persisted source', async () => {
    const markdown = '![Remote](https://loom.invalid/untrusted.png)';
    render(markdown);
    const editorLocator = page.getByRole('textbox', { name: 'Manuscript editor' });
    await expect.element(editorLocator).toBeInTheDocument();
    const image = editorLocator.element().querySelector('img');

    expect(image).not.toBeNull();
    expect(image?.getAttribute('src')).toBeNull();
    expect(image?.getAttribute('alt')).toBe('Remote');
    await expect.poll(serializedMarkdown).toBe(markdown);
  });

  it('rejects an attachment result after the visual document identity changes', async () => {
    const committed: number[] = [];
    const errors: string[] = [];
    let resolveAttachment: (snippets: readonly string[]) => void = () => {
      throw new Error('attachment promise was not captured');
    };
    render('hello', [], {
      onImageAttachments: () => new Promise((resolve) => { resolveAttachment = resolve; }),
      onImageAttachmentsCommitted: (count) => committed.push(count),
      onImageAttachmentError: (message) => errors.push(message)
    });
    const editorLocator = page.getByRole('textbox', { name: 'Manuscript editor' });
    await expect.element(editorLocator).toBeInTheDocument();
    const editor = editorLocator.element();
    await new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));
    dispatchTransfer(editor, 'paste', imageTransfer());

    await userEvent.keyboard('!');
    await expect.poll(serializedMarkdown).toBe('hello!');
    resolveAttachment([`![Late](../assets/${'b'.repeat(64)}.png)`]);
    await new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));

    await expect.poll(serializedMarkdown).toBe('hello!');
    expect(committed).toEqual([]);
    expect(errors).toEqual([
      'The image was stored in project assets, but the manuscript changed before it could be inserted.'
    ]);
    await expect.element(page.getByRole('status', { name: 'Attachment Commit Witness' }))
      .toHaveTextContent('none');
  });

  it('acknowledges an async visual attachment only after insertion without stealing later focus', async () => {
    let resolveAttachment: (snippets: readonly string[]) => void = () => {
      throw new Error('attachment promise was not captured');
    };
    const committed: number[] = [];
    const markdownPath = `../assets/${'d'.repeat(64)}.png`;
    render('hello', [], {
      onImageAttachments: () => new Promise((resolve) => { resolveAttachment = resolve; }),
      onImageAttachmentsCommitted: (count) => committed.push(count)
    });
    const editorLocator = page.getByRole('textbox', { name: 'Manuscript editor' });
    await expect.element(editorLocator).toBeInTheDocument();
    const editor = editorLocator.element();
    const outside = document.createElement('input');
    outside.setAttribute('aria-label', 'Attachment outside control');
    document.body.append(outside);
    dispatchTransfer(editor, 'paste', imageTransfer());
    outside.focus();

    resolveAttachment([`![Late](${markdownPath})`]);

    const committedMarkdown = `hello\n\n![Late](${markdownPath})`;
    await expect.poll(serializedMarkdown).toBe(committedMarkdown);
    expect(document.activeElement).toBe(outside);
    expect(committed).toEqual([1]);
    const commitWitness = page.getByRole('status', { name: 'Attachment Commit Witness' });
    await expect.element(commitWitness).toBeInTheDocument();
    await expect.poll(() => commitWitness.element().textContent).toBe(`1:${committedMarkdown}`);
  });

  it('keeps an exact visual ghost visible when attachment storage returns empty or rejects', async () => {
    let attempts = 0;
    const errors: string[] = [];
    render('hello', fourChoiceCompletion().slice(0, 1), {
      onImageAttachments: async () => {
        attempts += 1;
        if (attempts === 1) return [];
        throw new Error('visual attachment storage failed');
      },
      onImageAttachmentError: (message) => errors.push(message)
    });
    const editorLocator = page.getByRole('textbox', { name: 'Manuscript editor' });
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    const editor = editorLocator.element();

    dispatchTransfer(editor, 'paste', imageTransfer('Empty.png'));
    await expect.poll(() => attempts).toBe(1);
    await paintTwice();
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    await expect.poll(serializedMarkdown).toBe('hello');

    dispatchTransfer(editor, 'paste', imageTransfer('Rejected.png'));
    await expect.poll(() => errors.length).toBe(1);
    expect(errors[0]).toContain('visual attachment storage failed');
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    await expect.element(page.getByRole('status', { name: 'Completion Context' }))
      .toHaveTextContent('browser-session:browser-document:1:visual');
    await expect.element(page.getByRole('status', { name: 'Generation Requests' }))
      .toHaveTextContent('0');
  });

  it('splits, sinks, lifts, and exits a numbered list with ordinary writing keys', async () => {
    const keyboard = userEvent.setup();
    render('1. one\n2. two');

    await keyboard.keyboard('{Enter}three');
    await expect.poll(serializedMarkdown).toBe('1. one\n2. two\n3. three');

    await keyboard.keyboard('{Tab}');
    await expect.poll(serializedMarkdown).toBe('1. one\n2. two\n   1. three');

    await keyboard.keyboard('{Shift>}{Tab}{/Shift}');
    await expect.poll(serializedMarkdown).toBe('1. one\n2. two\n3. three');

    await keyboard.keyboard('{Enter}{Enter}after');
    await expect.poll(serializedMarkdown).toBe('1. one\n2. two\n3. three\n\nafter');
    await keyboard.cleanup();
  });

  it('refuses a direct multiline visual ghost without corrupting document structure or history', async () => {
    const keyboard = userEvent.setup();
    render('hello', [
      {
        candidateId: 'multiline',
        presentationKey: 'multiline:1',
        text: ' world.\n\nA new paragraph.',
        runId: 'run-multiline',
        targetByte: 5,
        insertsOnAccept: true
      }
    ]);
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await expect.element(page.getByRole('status', { name: 'Completion Presentation' }))
      .toHaveTextContent('A new paragraph.');

    expect(editor.element().querySelector('.loom-visual-ghost')).toBeNull();
    expect(editor.element().querySelectorAll(':scope > p')).toHaveLength(1);
    await keyboard.keyboard('{Alt>}{ArrowRight}{/Alt}');
    await expect.poll(serializedMarkdown).toBe('hello');

    await keyboard.keyboard('!');
    await expect.poll(serializedMarkdown).toBe('hello!');
    await keyboard.keyboard('{Meta>}z{/Meta}');
    await expect.poll(serializedMarkdown).toBe('hello');
    expect(editor.element().querySelector('.loom-visual-ghost')).toBeNull();
    const paragraphs = editor.element().querySelectorAll(':scope > p');
    expect(paragraphs).toHaveLength(1);
    expect(paragraphs[0]?.textContent).toBe('hello');
    await keyboard.cleanup();
  });

  it('keeps a real editor selection while opening Aa and applying Bold', async () => {
    render('alpha beta gamma');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await editor.click();
    await userEvent.keyboard('{Meta>}a{/Meta}');
    expect(document.getSelection()?.toString().trim()).toBe('alpha beta gamma');

    await page.getByRole('button', { name: 'Format text' }).click();
    expect(document.getSelection()?.toString().trim()).toBe('alpha beta gamma');
    await page.getByRole('button', { name: 'Bold' }).click();

    await expect.element(page.getByRole('status', { name: 'Serialized Markdown' }))
      .toHaveTextContent('**alpha beta gamma**');
  });

  it('restores the editor selection after the link input takes focus', async () => {
    render('linked words');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await editor.click();
    await userEvent.keyboard('{Meta>}a{/Meta}');
    await page.getByRole('button', { name: 'Format text' }).click();
    await userEvent.fill(
      page.getByRole('textbox', { name: 'Link destination' }),
      'https://example.com'
    );
    await page.getByRole('button', { name: 'Link' }).click();

    await expect.element(page.getByRole('status', { name: 'Serialized Markdown' }))
      .toHaveTextContent('[linked words](https://example.com)');
    await paintTwice();
    expect(document.activeElement).toBe(editor.element());
    expect(document.getSelection()?.toString().trim()).toBe('linked words');

    // WebKit can publish a second DOM-only AX reconciliation after the initial
    // 900 ms fallback has already fired. A selectionchange inside the bounded
    // command lease must trigger a fresh repair when there has been no new
    // editor intent; real key/pointer interactions cancel that lease in the
    // adjacent regressions.
    await new Promise((resolve) => window.setTimeout(resolve, 1_200));
    const lateRange = document.createRange();
    lateRange.setStart(editor.element(), 0);
    lateRange.collapse(true);
    const lateSelection = document.getSelection();
    lateSelection?.removeAllRanges();
    lateSelection?.addRange(lateRange);
    document.dispatchEvent(new Event('selectionchange'));
    expect(document.getSelection()?.toString()).toBe('');

    await expect.poll(() => document.getSelection()?.toString().trim()).toBe('linked words');
    expect(document.activeElement).toBe(editor.element());
    await expect.poll(() => JSON.parse(
      page.getByRole('status', { name: 'Visual Selection Witness' }).element().textContent ?? '{}'
    )).toMatchObject({
      available: true,
      empty: false,
      allVisibleText: true,
      caretAtEnd: false,
      caretByteOffset: null
    });
  });

  it('changes an existing link destination through the Aa palette', async () => {
    render('[linked words](https://old.example)');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await editor.click();
    await userEvent.keyboard('{Meta>}a{/Meta}');
    await page.getByRole('button', { name: 'Format text' }).click();
    await userEvent.fill(
      page.getByRole('textbox', { name: 'Link destination' }),
      'https://new.example'
    );
    await page.getByRole('button', { name: 'Link' }).click();

    await expect.poll(serializedMarkdown)
      .toBe('[linked words](https://new.example)');
  });

  it('renders, consumes, and reverses a cached ghost without new inference', async () => {
    render('hello', [
      { candidateId: 'a', presentationKey: 'a:1', text: ' world again', runId: 'run-a', targetByte: 5, insertsOnAccept: true },
      { candidateId: 'b', presentationKey: 'b:1', text: ' there friend', runId: 'run-b', targetByte: 5, insertsOnAccept: true },
      { candidateId: 'c', presentationKey: 'c:1', text: ' from here', runId: 'run-c', targetByte: 5, insertsOnAccept: true },
      { candidateId: 'd', presentationKey: 'd:1', text: ' and onward', runId: 'run-d', targetByte: 5, insertsOnAccept: true }
    ]);
    const ghost = page.getByText(' world again', { exact: true }).first();
    await expect.element(ghost).toBeVisible();

    await userEvent.keyboard('{Alt>}{ArrowRight}{/Alt}');
    await expect.element(page.getByRole('status', { name: 'Serialized Markdown' }))
      .toHaveTextContent('hello world');
    await expect.element(page.getByRole('status', { name: 'Generation Requests' }))
      .toHaveTextContent('0');
    await expect.element(page.getByRole('status', { name: 'Completion Presentation' }))
      .toHaveTextContent('11:a:1:session:6: again');
    await expect.element(page.getByText('again', { exact: true }).first())
      .toBeVisible();

    await userEvent.keyboard('{Alt>}{ArrowLeft}{/Alt}');
    await expect.element(page.getByRole('status', { name: 'Serialized Markdown' }))
      .toHaveTextContent('hello');
    await expect.element(page.getByText(' world again', { exact: true }).first())
      .toBeVisible();
    expect(page.getByRole('listbox', { name: 'Completion suggestions' }).query()).toBeNull();
    await expect.element(page.getByRole('status', { name: 'Generation Requests' }))
      .toHaveTextContent('0');
    const paragraph = page.getByRole('textbox', { name: 'Manuscript editor' })
      .element().querySelector('p');
    expect(paragraph?.firstChild?.textContent).toBe('hello');
  });

  it('grows the visible WYSIWYG ghost on every streamed presentation frame', async () => {
    const candidate = (presentationKey: string, text: string): CompletionCandidate => ({
      candidateId: 'streaming-a',
      presentationKey,
      text,
      runId: 'streaming-run-a',
      targetByte: 5,
      insertsOnAccept: true
    });
    render('hello', [candidate('streaming-a:1', ' w')], {
      completionFrames: [
        [candidate('streaming-a:2', ' world')],
        [candidate('streaming-a:3', ' world again')]
      ]
    });
    const editorLocator = page.getByRole('textbox', { name: 'Manuscript editor' });
    const advance = page.getByRole('button', { name: 'Advance completion stream' });
    await expect.element(page.getByText(' w', { exact: true }).first()).toBeVisible();
    const editor = editorLocator.element();

    await page.getByRole('button', { name: 'Reconcile current selection' }).click();
    await expect.element(page.getByRole('status', { name: 'Completion Context' }))
      .toHaveTextContent('browser-session:browser-document:1:visual');
    await expect.element(page.getByRole('status', { name: 'Generation Requests' }))
      .toHaveTextContent('0');
    await expect.element(page.getByText(' w', { exact: true }).first()).toBeVisible();

    await advance.click();
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    await expect.element(page.getByRole('status', { name: 'Completion Stream Frame' }))
      .toHaveTextContent('1');

    await advance.click();
    await expect.element(page.getByText(' world again', { exact: true }).first()).toBeVisible();
    await expect.element(page.getByRole('status', { name: 'Completion Stream Frame' }))
      .toHaveTextContent('2');
    expect(Array.from(editor.querySelectorAll('.loom-visual-ghost')).map((node) => node.textContent))
      .toEqual([' world again']);
    await expect.poll(serializedMarkdown).toBe('hello');
    await expect.element(page.getByRole('status', { name: 'Generation Requests' }))
      .toHaveTextContent('0');
  });

  it('reports an exact boundary without a null callback for same-selection reconciliation', async () => {
    render('hello', fourChoiceCompletion().slice(0, 1));
    const callbackCount = page.getByRole('status', {
      name: 'Selection Callback Count',
      exact: true
    });
    const nullCount = page.getByRole('status', {
      name: 'Null Selection Callback Count',
      exact: true
    });
    const latest = page.getByRole('status', { name: 'Latest Selection Callback' });
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    await expect.element(latest).toHaveTextContent('5:none');
    await new Promise((resolve) => window.setTimeout(resolve, 80));
    const callbacksBefore = Number(callbackCount.element().textContent ?? '0');
    const nullsBefore = Number(nullCount.element().textContent ?? '0');

    await page.getByRole('button', { name: 'Reconcile current selection' }).click();

    await expect.poll(
      () => Number(callbackCount.element().textContent ?? '0')
    ).toBeGreaterThan(callbacksBefore);
    expect(Number(nullCount.element().textContent ?? '0')).toBe(nullsBefore);
    await expect.element(latest).toHaveTextContent('5:none');
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    await expect.element(page.getByRole('status', { name: 'Completion Context' }))
      .toHaveTextContent('browser-session:browser-document:1:visual');
    await expect.element(page.getByRole('status', { name: 'Generation Requests' }))
      .toHaveTextContent('0');
  });

  it('restores an exact cached ghost after refocus without stealing focus and invalidates real navigation', async () => {
    const keyboard = userEvent.setup();
    render('hello', [
      {
        candidateId: 'idle-a',
        presentationKey: 'idle-a:1',
        text: ' world',
        runId: 'idle-run-a',
        targetByte: 5,
        insertsOnAccept: true
      }
    ]);
    const editorLocator = page.getByRole('textbox', { name: 'Manuscript editor' });
    const outside = document.createElement('input');
    outside.setAttribute('aria-label', 'Idle outside control');
    document.body.append(outside);
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    const editor = editorLocator.element();
    const visualSelectionWitness = () => JSON.parse(
      page.getByRole('status', { name: 'Visual Selection Witness' }).element().textContent ?? '{}'
    );
    const initialGhost = editor.querySelector('.loom-visual-ghost');
    expect(initialGhost).not.toBeNull();

    outside.focus();
    window.dispatchEvent(new Event('focus'));
    document.dispatchEvent(new Event('visibilitychange'));
    await paintTwice();
    expect(document.activeElement).toBe(outside);
    const lifecycleRefreshedGhost = editor.querySelector('.loom-visual-ghost');
    expect(lifecycleRefreshedGhost).not.toBeNull();
    expect(lifecycleRefreshedGhost).not.toBe(initialGhost);
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    editor.focus();
    await expect.poll(() => {
      const selection = document.getSelection();
      const witness = visualSelectionWitness();
      return {
        active: document.activeElement === editor,
        collapsed: selection?.isCollapsed ?? false,
        anchorInside: Boolean(selection?.anchorNode && editor.contains(selection.anchorNode)),
        available: witness.available,
        caretAtEnd: witness.caretAtEnd,
        caretByteOffset: witness.caretByteOffset
      };
    }).toEqual({
      active: true,
      collapsed: true,
      anchorInside: true,
      available: true,
      caretAtEnd: true,
      caretByteOffset: 5
    });
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    await expect.element(page.getByRole('status', { name: 'Generation Requests' }))
      .toHaveTextContent('0');

    // A genuinely different caret remains a navigation boundary and invalidates
    // the production-faithful parent session before any later resume.
    await keyboard.keyboard('{ArrowLeft}');
    await expect.poll(() => editor.querySelector('.loom-visual-ghost')).toBeNull();
    await expect.element(page.getByRole('status', { name: 'Completion Context' }))
      .toHaveTextContent('none');
    await expect.poll(visualSelectionWitness).toMatchObject({
      available: true,
      caretAtEnd: false,
      caretByteOffset: 4
    });
    outside.focus();
    window.dispatchEvent(new Event('focus'));
    document.dispatchEvent(new Event('visibilitychange'));
    await paintTwice();
    expect(document.activeElement).toBe(outside);
    editor.focus();
    await paintTwice();
    expect(editor.querySelector('.loom-visual-ghost')).toBeNull();
    await keyboard.cleanup();
  });

  it('retains one exhausted word only for one immediate exact rollback', async () => {
    const keyboard = userEvent.setup();
    render('hello', [
      { candidateId: 'a', presentationKey: 'a:1', text: ' world', runId: 'run-a', targetByte: 5, insertsOnAccept: true }
    ]);
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();

    await keyboard.keyboard('{Alt>}{ArrowRight}{ArrowLeft}');
    await expect.element(page.getByRole('status', { name: 'Serialized Markdown' }))
      .toHaveTextContent('hello');
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    await keyboard.keyboard('{ArrowLeft}{/Alt}');
    await expect.element(page.getByRole('status', { name: 'Serialized Markdown' }))
      .toHaveTextContent('hello');
    await expect.element(page.getByRole('status', { name: 'Generation Requests' }))
      .toHaveTextContent('1');
    await keyboard.cleanup();
  });

  it('lets Escape deliberately clear exhausted rollback authority', async () => {
    render('hello', [
      { candidateId: 'a', presentationKey: 'a:1', text: ' world', runId: 'run-a', targetByte: 5, insertsOnAccept: true }
    ]);
    await userEvent.keyboard('{Alt>}{ArrowRight}{/Alt}{Escape}');
    await expect.element(page.getByRole('status', { name: 'Serialized Markdown' }))
      .toHaveTextContent('hello world');
    await expect.element(page.getByRole('status', { name: 'Completion Presentation' }))
      .toHaveTextContent('none');
  });

  it('reverses an accepted visual word on the immediately following key event', async () => {
    const keyboard = userEvent.setup();
    render('hello', [
      { candidateId: 'a', presentationKey: 'a:1', text: ' world again', runId: 'run-a', targetByte: 5, insertsOnAccept: true },
      { candidateId: 'b', presentationKey: 'b:1', text: ' there friend', runId: 'run-b', targetByte: 5, insertsOnAccept: true }
    ]);
    await expect.element(page.getByText(' world again', { exact: true }).first()).toBeVisible();

    await keyboard.keyboard('{Alt>}{ArrowRight}{ArrowLeft}{/Alt}');

    await expect.element(page.getByRole('status', { name: 'Serialized Markdown' }))
      .toHaveTextContent('hello');
    await expect.element(page.getByText(' world again', { exact: true }).first()).toBeVisible();
    await expect.element(page.getByRole('status', { name: 'Generation Requests' }))
      .toHaveTextContent('0');
    await keyboard.cleanup();
  });

  it('dismisses a consumed cached continuation instead of re-serving it', async () => {
    render('hello', [
      { candidateId: 'a', presentationKey: 'a:1', text: ' world again', runId: 'run-a', targetByte: 5, insertsOnAccept: true },
      { candidateId: 'b', presentationKey: 'b:1', text: ' there friend', runId: 'run-b', targetByte: 5, insertsOnAccept: true }
    ]);
    await expect.element(page.getByText(' world again', { exact: true }).first()).toBeVisible();

    await userEvent.keyboard('{Alt>}{ArrowRight}{/Alt}{Escape}');

    await expect.element(page.getByRole('status', { name: 'Serialized Markdown' }))
      .toHaveTextContent('hello world');
    await expect.element(page.getByRole('status', { name: 'Completion Presentation' }))
      .toHaveTextContent('none');
    expect(page.getByText('again', { exact: true }).query()).toBeNull();
    await expect.element(page.getByRole('status', { name: 'Generation Requests' }))
      .toHaveTextContent('0');
  });

  it('keeps all four alternatives visible while Option cycles the active candidate', async () => {
    render('hello', fourChoiceCompletion());
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' }).element();

    dispatchOptionDown(editor);
    const fan = page.getByRole('listbox', { name: 'Completion suggestions' });
    await expect.element(fan).toBeVisible();
    const inlineGhost = document.querySelector<HTMLElement>('.loom-visual-ghost');
    expect(inlineGhost).not.toBeNull();
    expect(inlineGhost?.classList.contains('ghost-text-hidden')).toBe(false);
    expect(inlineGhost?.getClientRects().length).toBeGreaterThan(0);
    const fanElement = fan.element();
    expect(fanElement.id).not.toBe('');
    await expect.poll(() => editor.getAttribute('aria-controls')).toBe(fanElement.id);
    const options = page.getByRole('option');
    await expect.element(options).toHaveLength(4);
    const rows = Array.from(document.querySelectorAll<HTMLElement>('.loom-ghost-fan-row'));
    expect(rows).toHaveLength(4);
    expect(rows.every((row) => row.getClientRects().length > 0)).toBe(true);
    expect(rows.every((row) => row.id.length > 0)).toBe(true);
    expect(new Set(rows.map((row) => row.id)).size).toBe(4);
    const initialActiveDescendant = editor.getAttribute('aria-activedescendant');
    expect(initialActiveDescendant).toBe(rows[0].id);
    expect(document.getElementById(initialActiveDescendant ?? '')).toBe(rows[0]);
    await expect.poll(
      () => document.querySelector<HTMLElement>('.loom-ghost-fan-row.active')?.textContent
    ).toContain('world');

    dispatchKey(editor, 'keydown', 'ArrowDown', 'ArrowDown', true);
    dispatchKey(editor, 'keyup', 'ArrowDown', 'ArrowDown', true);
    await expect.poll(
      () => document.querySelector<HTMLElement>('.loom-ghost-fan-row.active')?.textContent
    ).toContain('there');
    const selectedDown = document.querySelector<HTMLElement>(
      '[role="option"][aria-selected="true"]'
    );
    expect(selectedDown?.getAttribute('aria-selected')).toBe('true');
    expect(selectedDown?.getAttribute('aria-label')).toContain('Suggestion 2 of 4: there');
    await expect.poll(() => editor.getAttribute('aria-activedescendant')).toBe(selectedDown?.id);
    expect(selectedDown?.id).not.toBe(initialActiveDescendant);
    expect(document.getElementById(editor.getAttribute('aria-activedescendant') ?? ''))
      .toBe(selectedDown);
    expect(document.querySelectorAll('.loom-ghost-fan-row')).toHaveLength(4);
    dispatchKey(editor, 'keydown', 'ArrowUp', 'ArrowUp', true);
    dispatchKey(editor, 'keyup', 'ArrowUp', 'ArrowUp', true);
    await expect.poll(
      () => document.querySelector<HTMLElement>('.loom-ghost-fan-row.active')?.textContent
    ).toContain('world');
    const selectedUp = document.querySelector<HTMLElement>(
      '[role="option"][aria-selected="true"]'
    );
    expect(selectedUp?.getAttribute('aria-selected')).toBe('true');
    expect(selectedUp?.getAttribute('aria-label')).toContain('Suggestion 1 of 4: world');
    await expect.poll(() => editor.getAttribute('aria-activedescendant')).toBe(selectedUp?.id);
    expect(document.getElementById(editor.getAttribute('aria-activedescendant') ?? ''))
      .toBe(selectedUp);
    dispatchOptionUp(editor);
    await expect.poll(completionFanIsVisible).toBe(false);
    await expect.poll(() => editor.getAttribute('aria-controls')).toBeNull();
    expect(editor.getAttribute('aria-activedescendant')).toBeNull();
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
  });

  it('pins the completion lens without stealing the caret and cycles with plain arrows', async () => {
    render('hello', fourChoiceCompletion());
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' }).element();
    const trigger = page.getByRole('button', { name: 'Pin completion alternatives' });

    await trigger.click();
    await expect.poll(completionFanIsVisible).toBe(true);
    expect(document.activeElement).toBe(editor);
    dispatchKey(editor, 'keydown', 'ArrowDown', 'ArrowDown', false);
    dispatchKey(editor, 'keyup', 'ArrowDown', 'ArrowDown', false);
    await expect.poll(
      () => document.querySelector<HTMLElement>('.loom-ghost-fan-row.active')?.textContent
    ).toContain('there');

    dispatchKey(editor, 'keydown', 'Escape', 'Escape', false);
    dispatchKey(editor, 'keyup', 'Escape', 'Escape', false);
    await expect.poll(completionFanIsVisible).toBe(false);
    await expect.element(page.getByText(' there', { exact: true }).first()).toBeVisible();
  });

  it('restores the selected four-choice fan when held Option-Left reverses fan Return', async () => {
    render('hello', fourChoiceCompletion());
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' }).element();

    dispatchOptionDown(editor);
    await expect.poll(completionFanIsVisible).toBe(true);
    dispatchKey(editor, 'keydown', 'ArrowDown', 'ArrowDown', true);
    dispatchKey(editor, 'keyup', 'ArrowDown', 'ArrowDown', true);
    await expect.poll(
      () => document.querySelector<HTMLElement>('.loom-ghost-fan-row.active')?.textContent
    ).toContain('there');

    dispatchKey(editor, 'keydown', 'Enter', 'Enter', true);
    dispatchKey(editor, 'keyup', 'Enter', 'Enter', true);
    await expect.poll(serializedMarkdown).toBe('hello there');
    await expect.element(page.getByRole('status', { name: 'Completion Presentation' }))
      .toHaveTextContent('b:1:session:6');

    // Option remains physically held throughout. The exhausted presentation
    // is intentionally hidden, but it is still exact rollback authority and
    // must not be mistaken for an Option-up before ArrowLeft consumes it.
    dispatchKey(editor, 'keydown', 'ArrowLeft', 'ArrowLeft', true);
    dispatchKey(editor, 'keyup', 'ArrowLeft', 'ArrowLeft', true);
    await expect.poll(serializedMarkdown).toBe('hello');
    await expect.poll(completionFanIsVisible).toBe(true);
    await expect.element(page.getByRole('option')).toHaveLength(4);
    await expect.poll(
      () => document.querySelector<HTMLElement>('.loom-ghost-fan-row.active')?.textContent
    ).toContain('there');

    dispatchOptionUp(editor);
    await expect.poll(completionFanIsVisible).toBe(false);
  });

  it('restores the selected source fan when held Option-Left reverses fan Return', async () => {
    renderSource('hello', fourChoiceCompletion());
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    const textarea = page.getByRole('textbox', { name: 'Markdown source editor' }).element();

    dispatchOptionDown(textarea);
    await expect.poll(completionFanIsVisible).toBe(true);
    dispatchKey(textarea, 'keydown', 'ArrowDown', 'ArrowDown', true);
    dispatchKey(textarea, 'keyup', 'ArrowDown', 'ArrowDown', true);
    await expect.poll(
      () => document.querySelector<HTMLElement>('.loom-ghost-fan-row.active')?.textContent
    ).toContain('there');

    dispatchKey(textarea, 'keydown', 'Enter', 'Enter', true);
    dispatchKey(textarea, 'keyup', 'Enter', 'Enter', true);
    await expect.element(page.getByRole('status', { name: 'Source Markdown' }))
      .toHaveTextContent('hello there');

    dispatchKey(textarea, 'keydown', 'ArrowLeft', 'ArrowLeft', true);
    dispatchKey(textarea, 'keyup', 'ArrowLeft', 'ArrowLeft', true);
    await expect.element(page.getByRole('status', { name: 'Source Markdown' }))
      .toHaveTextContent('hello');
    await expect.poll(completionFanIsVisible).toBe(true);
    await expect.element(page.getByRole('option')).toHaveLength(4);
    await expect.poll(
      () => document.querySelector<HTMLElement>('.loom-ghost-fan-row.active')?.textContent
    ).toContain('there');

    dispatchOptionUp(textarea);
    await expect.poll(completionFanIsVisible).toBe(false);
  });

  it('docks the visual completion lens in the trailing gutter and repositions it after resize', async () => {
    render('hello', fourChoiceCompletion());
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' }).element();

    dispatchOptionDown(editor);
    const fanLocator = page.getByRole('listbox', { name: 'Completion suggestions' });
    await expect.element(fanLocator).toBeVisible();
    const fan = fanLocator.element();
    await expect.poll(() => fan.dataset.side === 'above' || fan.dataset.side === 'below').toBe(true);
    const initialLeft = Number.parseFloat(fan.style.left);
    expect(Number.isFinite(initialLeft)).toBe(true);
    expect(Math.abs(initialLeft - (window.innerWidth - 12 - fan.getBoundingClientRect().width)))
      .toBeLessThanOrEqual(1);

    fan.style.left = '-999px';
    window.dispatchEvent(new Event('resize'));
    await expect.poll(() => Number.parseFloat(fan.style.left) >= 12).toBe(true);
  });

  it('closes the visual completion fan when its caret scrolls out of the editor viewport', async () => {
    const manuscript = Array.from(
      { length: 80 },
      (_, index) => `Paragraph ${index} keeps the completion anchor below the fold.`
    ).join('\n\n');
    render(manuscript, fourChoiceCompletion(new TextEncoder().encode(manuscript).byteLength));
    const pane = document.querySelector<HTMLElement>('.editor-pane');
    expect(pane).not.toBeNull();
    pane!.style.height = '160px';
    const editorLocator = page.getByRole('textbox', { name: 'Manuscript editor' });
    await expect.element(editorLocator).toBeInTheDocument();
    const editor = editorLocator.element();
    pane!.scrollTop = pane!.scrollHeight;
    pane!.dispatchEvent(new Event('scroll', { bubbles: true }));
    await paintTwice();
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    const initialGhost = document.querySelector<HTMLElement>('.loom-visual-ghost');
    expect(initialGhost).not.toBeNull();
    initialGhost!.scrollIntoView({ block: 'center' });
    pane!.dispatchEvent(new Event('scroll', { bubbles: true }));
    await paintTwice();
    const initialGhostRect = initialGhost!.getBoundingClientRect();
    const initialPaneRect = pane!.getBoundingClientRect();
    expect(
      Math.min(initialGhostRect.bottom, initialPaneRect.bottom) >
        Math.max(initialGhostRect.top, initialPaneRect.top)
    ).toBe(true);

    dispatchOptionDown(editor);
    await expect.poll(completionFanIsVisible).toBe(true);

    pane!.scrollTop = 0;
    pane!.dispatchEvent(new Event('scroll', { bubbles: true }));
    await expect.poll(completionFanIsVisible).toBe(false);
    await expect.poll(() => editor.getAttribute('aria-controls')).toBeNull();
    expect(editor.getAttribute('aria-activedescendant')).toBeNull();
    const ghostRect = document.querySelector<HTMLElement>('.loom-visual-ghost')
      ?.getBoundingClientRect();
    const paneRect = pane!.getBoundingClientRect();
    expect(ghostRect).toBeDefined();
    expect((ghostRect?.top ?? 0) >= paneRect.bottom).toBe(true);

    // A fresh physical Option witness cannot resurrect a fixed popup at a
    // clamped viewport edge while the actual insertion point remains clipped.
    dispatchOptionUp(editor);
    dispatchOptionDown(editor);
    await paintTwice();
    expect(completionFanIsVisible()).toBe(false);
  });

  it('clears a lost Option-up at every interaction boundary and on a fresh key witness', async () => {
    render('hello', fourChoiceCompletion());
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' }).element();

    const reopenFan = async (): Promise<void> => {
      dispatchOptionDown(editor);
      await expect.poll(completionFanIsVisible).toBe(true);
    };
    const expectReleased = async (): Promise<void> => {
      await expect.poll(completionFanIsVisible).toBe(false);
      await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    };

    await reopenFan();
    window.dispatchEvent(new Event('blur'));
    await expectReleased();

    await reopenFan();
    document.dispatchEvent(new Event('visibilitychange'));
    await expectReleased();

    await reopenFan();
    window.dispatchEvent(new Event('pagehide'));
    await expectReleased();

    await reopenFan();
    editor.dispatchEvent(new PointerEvent('pointerdown', {
      bubbles: true,
      cancelable: true,
      pointerId: 1,
      pointerType: 'mouse'
    }));
    await expectReleased();

    await reopenFan();
    editor.dispatchEvent(new KeyboardEvent('keydown', {
      key: 'Shift',
      code: 'ShiftLeft',
      altKey: false,
      bubbles: true,
      cancelable: true
    }));
    await expectReleased();
  });

  it('never lets a global Option event reopen the fan after exact editor focus is lost', async () => {
    render('hello', fourChoiceCompletion());
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' }).element();
    const outside = document.createElement('input');
    outside.setAttribute('aria-label', 'Outside editor control');
    document.body.append(outside);

    dispatchOptionDown(editor);
    await expect.poll(completionFanIsVisible).toBe(true);
    outside.focus();
    await expect.poll(completionFanIsVisible).toBe(false);

    dispatchOptionDown(outside);
    await expect.poll(completionFanIsVisible).toBe(false);
    expect(document.activeElement).toBe(outside);
  });

  it('starts a remounted visual editor with no inherited Option state', async () => {
    render('hello', fourChoiceCompletion());
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' }).element();
    dispatchOptionDown(editor);
    await expect.poll(completionFanIsVisible).toBe(true);

    if (!mounted) throw new Error('expected the visual editor harness to be mounted');
    await unmount(mounted);
    mounted = null;
    document.body.replaceChildren();

    render('hello', fourChoiceCompletion());
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    await expect.poll(completionFanIsVisible).toBe(false);
  });

  it('keeps the cached session across an autosave identity change and requests only after exhaustion', async () => {
    const keyboard = userEvent.setup();
    render('hello', [
      { candidateId: 'a', presentationKey: 'a:1', text: ' world again', runId: 'run-a', targetByte: 5, insertsOnAccept: true }
    ]);
    const context = page.getByRole('status', { name: 'Completion Context' });
    await expect.element(context).toHaveTextContent('browser-session:browser-document:1:visual');
    await expect.element(page.getByText(' world again', { exact: true }).first()).toBeVisible();

    await keyboard.keyboard('{Alt>}{ArrowRight}{/Alt}');
    await expect.element(page.getByRole('status', { name: 'Serialized Markdown' }))
      .toHaveTextContent('hello world');
    await page.getByRole('button', { name: 'Simulate checkpoint' }).click();
    await expect.element(page.getByRole('status', { name: 'Checkpoint Revision' }))
      .toHaveTextContent('2');
    await expect.element(context).toHaveTextContent('browser-session:browser-document:1:visual');
    await expect.element(page.getByText('again', { exact: true }).first()).toBeVisible();
    await expect.element(page.getByRole('status', { name: 'Generation Requests' }))
      .toHaveTextContent('0');

    await keyboard.keyboard('{Alt>}{ArrowRight}{/Alt}');
    await expect.element(page.getByRole('status', { name: 'Serialized Markdown' }))
      .toHaveTextContent('hello world again');
    await expect.element(page.getByRole('status', { name: 'Generation Requests' }))
      .toHaveTextContent('1');
    await keyboard.cleanup();
  });

  it('advances Shuttle through the shared session while autocomplete is also on', async () => {
    render('hello', [
      { candidateId: 'a', presentationKey: 'a:1', text: ' world again', runId: 'run-a', targetByte: 5, insertsOnAccept: true }
    ], { autocomplete: true, shuttle: true });
    const hiddenGhost = page.getByText(' world again', { exact: true }).first();
    await expect.element(hiddenGhost).toBeInTheDocument();
    await expect.element(hiddenGhost).not.toBeVisible();

    await page.getByRole('button', { name: 'Advance Shuttle' }).click();
    await expect.element(page.getByRole('status', { name: 'Serialized Markdown' }))
      .toHaveTextContent('hello world');
    await expect.element(page.getByRole('status', { name: 'Generation Requests' }))
      .toHaveTextContent('0');
    await expect.element(page.getByText('again', { exact: true }).first()).not.toBeVisible();
  });

  it('keeps the selected MD remainder visible across its value echo and a stable rerender, then reverses it', async () => {
    const keyboard = userEvent.setup();
    renderSource('hello', [
      { candidateId: 'a', presentationKey: 'a:1', text: ' world again', runId: 'run-a', targetByte: 5, insertsOnAccept: true },
      { candidateId: 'b', presentationKey: 'b:1', text: ' there friend', runId: 'run-b', targetByte: 5, insertsOnAccept: true },
      { candidateId: 'c', presentationKey: 'c:1', text: ' from here', runId: 'run-c', targetByte: 5, insertsOnAccept: true },
      { candidateId: 'd', presentationKey: 'd:1', text: ' and onward', runId: 'run-d', targetByte: 5, insertsOnAccept: true }
    ]);
    await expect.element(page.getByText(' world again', { exact: true }).first()).toBeVisible();

    await keyboard.keyboard('{Alt>}{ArrowDown}{ArrowRight}{/Alt}');
    await expect.poll(
      () => page.getByRole('status', { name: 'Source Markdown' }).element().textContent
    ).toBe('hello there ');
    await expect.element(page.getByText('friend', { exact: true }).first()).toBeVisible();
    await page.getByRole('button', { name: 'Stable rerender' }).click();
    await expect.element(page.getByText('friend', { exact: true }).first()).toBeVisible();
    await expect.element(page.getByRole('status', { name: 'Source Generation Requests' }))
      .toHaveTextContent('0');

    await keyboard.keyboard('{Alt>}{ArrowLeft}{/Alt}');
    await expect.element(page.getByRole('status', { name: 'Source Markdown' }))
      .toHaveTextContent('hello');
    await expect.element(page.getByText(' there friend', { exact: true }).first()).toBeVisible();
    await expect.element(page.getByRole('status', { name: 'Source Generation Requests' }))
      .toHaveTextContent('0');
    await keyboard.cleanup();
  });

  it('docks the source completion lens and preserves its accessible options after scroll', async () => {
    renderSource('hello', fourChoiceCompletion());
    const textarea = page.getByRole('textbox', { name: 'Markdown source editor' })
      .element() as HTMLTextAreaElement;
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();

    dispatchOptionDown(textarea);
    const fanLocator = page.getByRole('listbox', { name: 'Completion suggestions' });
    await expect.element(fanLocator).toBeVisible();
    const fan = fanLocator.element();
    expect(fan.id).not.toBe('');
    await expect.poll(() => textarea.getAttribute('aria-controls')).toBe(fan.id);
    await expect.poll(() => fan.dataset.side === 'above' || fan.dataset.side === 'below').toBe(true);
    const options = page.getByRole('option');
    await expect.element(options).toHaveLength(4);
    await expect.element(options.first()).toHaveAttribute('aria-selected', 'true');
    const optionElements = Array.from(
      fan.querySelectorAll<HTMLElement>('[role="option"]')
    );
    expect(optionElements.every((option) => option.id.length > 0)).toBe(true);
    expect(new Set(optionElements.map((option) => option.id)).size).toBe(4);
    const initialActiveDescendant = textarea.getAttribute('aria-activedescendant');
    expect(initialActiveDescendant).toBe(optionElements[0].id);
    expect(document.getElementById(initialActiveDescendant ?? '')).toBe(optionElements[0]);

    dispatchKey(textarea, 'keydown', 'ArrowDown', 'ArrowDown', true);
    dispatchKey(textarea, 'keyup', 'ArrowDown', 'ArrowDown', true);
    await expect.poll(
      () => fan.querySelector<HTMLElement>('[role="option"][aria-selected="true"]')?.textContent
    ).toContain('there');
    const selectedDown = fan.querySelector<HTMLElement>(
      '[role="option"][aria-selected="true"]'
    );
    await expect.poll(() => textarea.getAttribute('aria-activedescendant')).toBe(selectedDown?.id);
    expect(selectedDown?.id).not.toBe(initialActiveDescendant);
    expect(document.getElementById(textarea.getAttribute('aria-activedescendant') ?? ''))
      .toBe(selectedDown);
    const initialLeft = Number.parseFloat(fan.style.left);
    expect(Number.isFinite(initialLeft)).toBe(true);
    expect(Math.abs(initialLeft - (window.innerWidth - 12 - fan.getBoundingClientRect().width)))
      .toBeLessThanOrEqual(1);

    fan.style.left = '-999px';
    window.dispatchEvent(new Event('scroll'));
    await expect.poll(() => Number.parseFloat(fan.style.left) >= 12).toBe(true);
    dispatchOptionUp(textarea);
    await expect.poll(completionFanIsVisible).toBe(false);
    await expect.poll(() => textarea.getAttribute('aria-controls')).toBeNull();
    expect(textarea.getAttribute('aria-activedescendant')).toBeNull();
  });

  it('closes the source completion fan when its mirrored caret scrolls out of the textarea', async () => {
    const manuscript = Array.from(
      { length: 80 },
      (_, index) => `Source line ${index} keeps the completion anchor below the fold.`
    ).join('\n');
    renderSource(
      manuscript,
      fourChoiceCompletion(new TextEncoder().encode(manuscript).byteLength)
    );
    const pane = document.querySelector<HTMLElement>('.source-pane');
    expect(pane).not.toBeNull();
    pane!.style.height = '160px';
    const textarea = page.getByRole('textbox', { name: 'Markdown source editor' })
      .element() as HTMLTextAreaElement;
    await paintTwice();
    textarea.scrollTop = textarea.scrollHeight;
    textarea.dispatchEvent(new Event('scroll', { bubbles: true }));
    await paintTwice();
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();

    dispatchOptionDown(textarea);
    await expect.poll(completionFanIsVisible).toBe(true);

    textarea.scrollTop = 0;
    textarea.dispatchEvent(new Event('scroll', { bubbles: true }));
    await paintTwice();
    await expect.poll(completionFanIsVisible).toBe(false);
    await expect.poll(() => textarea.getAttribute('aria-controls')).toBeNull();
    expect(textarea.getAttribute('aria-activedescendant')).toBeNull();
    const ghostRect = document.querySelector<HTMLElement>('.loom-source-ghost-text')
      ?.getClientRects().item(0);
    const textareaRect = textarea.getBoundingClientRect();
    expect(ghostRect).not.toBeNull();
    expect(Boolean(
      ghostRect &&
      (ghostRect.bottom <= textareaRect.top || ghostRect.top >= textareaRect.bottom)
    )).toBe(true);

    dispatchOptionUp(textarea);
    dispatchOptionDown(textarea);
    await paintTwice();
    expect(completionFanIsVisible()).toBe(false);
  });

  it('deindents selected source lines with Shift-Tab without normalizing other bytes', async () => {
    renderSource('\tone\n\t\ttwo\n\tthree', []);
    const textarea = page.getByRole('textbox', { name: 'Markdown source editor' })
      .element() as HTMLTextAreaElement;
    await new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));
    textarea.focus();
    textarea.setSelectionRange(0, 11);

    await userEvent.keyboard('{Shift>}{Tab}{/Shift}');

    await expect.poll(
      () => page.getByRole('status', { name: 'Source Markdown' }).element().textContent
    ).toBe('one\n\ttwo\n\tthree');
    expect(textarea.selectionStart).toBe(0);
    expect(textarea.selectionEnd).toBe(9);
  });

  it('lets Shift-Tab leave an unindented source line', async () => {
    const previous = document.createElement('button');
    previous.textContent = 'Before source editor';
    document.body.append(previous);
    renderSource('plain', []);
    const textarea = page.getByRole('textbox', { name: 'Markdown source editor' })
      .element() as HTMLTextAreaElement;
    textarea.focus();
    const shiftTab = new KeyboardEvent('keydown', {
      key: 'Tab',
      shiftKey: true,
      bubbles: true,
      cancelable: true
    });
    textarea.dispatchEvent(shiftTab);
    expect(shiftTab.defaultPrevented).toBe(false);

    await userEvent.keyboard('{Shift>}{Tab}{/Shift}');

    expect(document.activeElement).not.toBe(textarea);
    await expect.element(page.getByRole('status', { name: 'Source Markdown' }))
      .toHaveTextContent('plain');
  });

  it('inserts dropped image Markdown at the captured source selection', async () => {
    const markdownPath = `../assets/${'c'.repeat(64)}.png`;
    const committed: number[] = [];
    renderSource('before after', [], async (files) => {
      expect(files.map((file) => file.name)).toEqual(['Drop.png']);
      return [`![Drop](${markdownPath})`];
    }, undefined, (count) => committed.push(count));
    const textarea = page.getByRole('textbox', { name: 'Markdown source editor' })
      .element() as HTMLTextAreaElement;
    await new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));
    textarea.focus();
    textarea.setSelectionRange(7, 12);
    const transfer = imageTransfer('Drop.png');

    expect(dispatchTransfer(textarea, 'dragover', transfer).defaultPrevented).toBe(true);
    expect(dispatchTransfer(textarea, 'drop', transfer).defaultPrevented).toBe(true);

    await expect.poll(
      () => page.getByRole('status', { name: 'Source Markdown' }).element().textContent
    ).toBe(`before ![Drop](${markdownPath})`);
    expect(committed).toEqual([1]);
    await expect.element(page.getByRole('status', { name: 'Source Attachment Commit Witness' }))
      .toHaveTextContent(`1:before ![Drop](${markdownPath})`);
  });

  it('acknowledges an async source attachment without reclaiming focus', async () => {
    let resolveAttachment: (snippets: readonly string[]) => void = () => {
      throw new Error('attachment promise was not captured');
    };
    const committed: number[] = [];
    const markdownPath = `../assets/${'e'.repeat(64)}.png`;
    renderSource(
      'hello',
      [],
      () => new Promise((resolve) => { resolveAttachment = resolve; }),
      undefined,
      (count) => committed.push(count)
    );
    const textareaLocator = page.getByRole('textbox', { name: 'Markdown source editor' });
    await expect.element(textareaLocator).toBeInTheDocument();
    const textarea = textareaLocator.element() as HTMLTextAreaElement;
    const outside = document.createElement('input');
    outside.setAttribute('aria-label', 'Source attachment outside control');
    document.body.append(outside);
    dispatchTransfer(textarea, 'paste', imageTransfer());
    outside.focus();

    const committedMarkdown = `hello![Late](${markdownPath})`;
    resolveAttachment([`![Late](${markdownPath})`]);

    await expect.element(page.getByRole('status', { name: 'Source Markdown' }))
      .toHaveTextContent(committedMarkdown);
    expect(document.activeElement).toBe(outside);
    expect(committed).toEqual([1]);
    await expect.element(page.getByRole('status', { name: 'Source Attachment Commit Witness' }))
      .toHaveTextContent(`1:${committedMarkdown}`);
  });

  it('preserves a later source caret when a captured attachment finishes', async () => {
    let resolveAttachment: (snippets: readonly string[]) => void = () => {
      throw new Error('attachment promise was not captured');
    };
    const markdownPath = `../assets/${'f'.repeat(64)}.png`;
    renderSource(
      'hello',
      [],
      () => new Promise((resolve) => { resolveAttachment = resolve; })
    );
    const textareaLocator = page.getByRole('textbox', { name: 'Markdown source editor' });
    await expect.element(textareaLocator).toBeInTheDocument();
    const textarea = textareaLocator.element() as HTMLTextAreaElement;
    await new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));
    textarea.focus();
    dispatchTransfer(textarea, 'paste', imageTransfer());
    textarea.setSelectionRange(0, 0);

    resolveAttachment([`![Late](${markdownPath})`]);

    await expect.element(page.getByRole('status', { name: 'Source Markdown' }))
      .toHaveTextContent(`hello![Late](${markdownPath})`);
    expect(document.activeElement).toBe(textarea);
    expect(textarea.selectionStart).toBe(0);
    expect(textarea.selectionEnd).toBe(0);
  });

  it('keeps an exact source ghost visible when attachment storage returns empty or rejects', async () => {
    let attempts = 0;
    const errors: string[] = [];
    renderSource(
      'hello',
      fourChoiceCompletion().slice(0, 1),
      async () => {
        attempts += 1;
        if (attempts === 1) return [];
        throw new Error('source attachment storage failed');
      },
      (message) => errors.push(message)
    );
    const textarea = page.getByRole('textbox', { name: 'Markdown source editor' }).element();
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();

    dispatchTransfer(textarea, 'paste', imageTransfer('Empty.png'));
    await expect.poll(() => attempts).toBe(1);
    await paintTwice();
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    await expect.element(page.getByRole('status', { name: 'Source Markdown' }))
      .toHaveTextContent('hello');

    dispatchTransfer(textarea, 'paste', imageTransfer('Rejected.png'));
    await expect.poll(() => errors.length).toBe(1);
    expect(errors[0]).toContain('source attachment storage failed');
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    await expect.element(page.getByRole('status', { name: 'Source Generation Requests' }))
      .toHaveTextContent('0');
  });

  it('prevents generic or empty-MIME source file drops from reaching WebKit navigation', async () => {
    const errors: string[] = [];
    renderSource('before', [], undefined, (message) => errors.push(message));
    const textarea = page.getByRole('textbox', { name: 'Markdown source editor' }).element();
    const generic = fileTransfer('Archive.bin', 'application/octet-stream');
    const unknownImage = fileTransfer('Camera.png', '');

    expect(dispatchTransfer(textarea, 'dragover', generic).defaultPrevented).toBe(true);
    expect(dispatchTransfer(textarea, 'drop', generic).defaultPrevented).toBe(true);
    expect(dispatchTransfer(textarea, 'dragover', unknownImage).defaultPrevented).toBe(true);
    expect(dispatchTransfer(textarea, 'drop', unknownImage).defaultPrevented).toBe(true);
    await expect.element(page.getByRole('status', { name: 'Source Markdown' }))
      .toHaveTextContent('before');
    expect(errors).toHaveLength(2);
    expect(errors.every((message) => message.includes('could not verify'))).toBe(true);
  });

  it('reports an HTML-only source image paste instead of persisting its data URL', async () => {
    const errors: string[] = [];
    renderSource('before', [], undefined, (message) => errors.push(message));
    const textarea = page.getByRole('textbox', { name: 'Markdown source editor' });
    await expect.element(textarea).toBeInTheDocument();
    const transfer = new DataTransfer();
    transfer.setData('text/html', '<img src="data:image/png;base64,AAAA">');

    const event = dispatchTransfer(textarea.element(), 'paste', transfer);

    expect(event.defaultPrevented).toBe(true);
    await expect.poll(
      () => page.getByRole('status', { name: 'Source Markdown' }).element().textContent
    ).toBe('before');
    expect(errors).toHaveLength(1);
    expect(errors[0]).toContain('could not read image bytes');
  });

  it('retains exhausted source authority for one exact Option-Left rollback', async () => {
    const keyboard = userEvent.setup();
    renderSource('hello', [
      { candidateId: 'a', presentationKey: 'a:1', text: ' world', runId: 'run-a', targetByte: 5, insertsOnAccept: true }
    ]);
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();

    await keyboard.keyboard('{Alt>}{ArrowRight}{ArrowLeft}{/Alt}');

    await expect.element(page.getByRole('status', { name: 'Source Markdown' }))
      .toHaveTextContent('hello');
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
    await expect.element(page.getByRole('status', { name: 'Source Generation Requests' }))
      .toHaveTextContent('0');
    await keyboard.cleanup();
  });
});
