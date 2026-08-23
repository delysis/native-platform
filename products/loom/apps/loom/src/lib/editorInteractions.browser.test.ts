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
  modes: { autocomplete?: boolean; shuttle?: boolean } = {}
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
  completionCandidates: CompletionCandidate[]
): void {
  const target = document.createElement('div');
  document.body.append(target);
  mounted = mount(SourceEditorBrowserHarness, {
    target,
    props: { initialValue, completionCandidates }
  });
}

describe('real WebKit editor interactions', () => {
  function serializedMarkdown(): string {
    return page.getByRole('status', { name: 'Serialized Markdown' }).element().textContent ?? '';
  }

  function fourChoiceCompletion(): CompletionCandidate[] {
    return [
      { candidateId: 'a', presentationKey: 'a:1', text: ' world', runId: 'run-a', targetByte: 5, insertsOnAccept: true },
      { candidateId: 'b', presentationKey: 'b:1', text: ' there', runId: 'run-b', targetByte: 5, insertsOnAccept: true },
      { candidateId: 'c', presentationKey: 'c:1', text: ' again', runId: 'run-c', targetByte: 5, insertsOnAccept: true },
      { candidateId: 'd', presentationKey: 'd:1', text: ' onward', runId: 'run-d', targetByte: 5, insertsOnAccept: true }
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

    await page.getByText('Aa', { exact: true }).click();
    await page.getByRole('button', { name: 'Title' }).click();
    await expect.poll(serializedMarkdown).toBe('# Something else');
    await page.getByRole('button', { name: 'Body' }).click();
    await expect.poll(serializedMarkdown).toBe('Something else');
    await keyboard.cleanup();
  });

  it('wires every Aa formatting family to exact Markdown', async () => {
    render('Words');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await editor.click();
    await userEvent.keyboard('{Meta>}a{/Meta}');
    await page.getByText('Aa', { exact: true }).click();

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

  it('keeps one terminal prose space outside inline palette formatting', async () => {
    const keyboard = userEvent.setup();
    render('Words ');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await editor.click();
    await keyboard.keyboard('{Meta>}a{/Meta}');
    await page.getByText('Aa', { exact: true }).click();

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

    await page.getByText('Aa', { exact: true }).click();
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
    await page.getByText('Aa', { exact: true }).click();
    await userEvent.fill(
      page.getByRole('textbox', { name: 'Link destination' }),
      'https://example.com'
    );
    await page.getByRole('button', { name: 'Link' }).click();

    await expect.element(page.getByRole('status', { name: 'Serialized Markdown' }))
      .toHaveTextContent('[linked words](https://example.com)');
  });

  it('changes an existing link destination through the Aa palette', async () => {
    render('[linked words](https://old.example)');
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await editor.click();
    await userEvent.keyboard('{Meta>}a{/Meta}');
    await page.getByText('Aa', { exact: true }).click();
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
    const options = page.getByRole('option');
    await expect.element(options).toHaveLength(4);
    const rows = Array.from(document.querySelectorAll<HTMLElement>('.loom-ghost-fan-row'));
    expect(rows).toHaveLength(4);
    expect(rows.every((row) => row.getClientRects().length > 0)).toBe(true);
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
    dispatchOptionUp(editor);
    await expect.poll(completionFanIsVisible).toBe(false);
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();
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
