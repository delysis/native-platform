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
    await expect.element(page.getByRole('status', { name: 'Generation Requests' }))
      .toHaveTextContent('0');
    const paragraph = page.getByRole('textbox', { name: 'Manuscript editor' })
      .element().querySelector('p');
    expect(paragraph?.firstChild?.textContent).toBe('hello');
  });

  it('keeps all four alternatives visible while Option cycles the active candidate', async () => {
    const keyboard = userEvent.setup();
    render('hello', [
      { candidateId: 'a', presentationKey: 'a:1', text: ' world', runId: 'run-a', targetByte: 5, insertsOnAccept: true },
      { candidateId: 'b', presentationKey: 'b:1', text: ' there', runId: 'run-b', targetByte: 5, insertsOnAccept: true },
      { candidateId: 'c', presentationKey: 'c:1', text: ' again', runId: 'run-c', targetByte: 5, insertsOnAccept: true },
      { candidateId: 'd', presentationKey: 'd:1', text: ' onward', runId: 'run-d', targetByte: 5, insertsOnAccept: true }
    ]);
    await expect.element(page.getByText(' world', { exact: true }).first()).toBeVisible();

    await keyboard.keyboard('{Alt>}{ArrowDown}');
    const rows = Array.from(document.querySelectorAll<HTMLElement>('.loom-ghost-fan-row'));
    expect(rows).toHaveLength(4);
    expect(rows.every((row) => row.getClientRects().length > 0)).toBe(true);
    await expect.poll(
      () => document.querySelector<HTMLElement>('.loom-ghost-fan-row.active')?.textContent
    ).toContain('there');
    expect(document.querySelectorAll('.loom-ghost-fan-row')).toHaveLength(4);
    await keyboard.cleanup();
  });

  it('keeps the cached session across an autosave identity change and requests only after exhaustion', async () => {
    render('hello', [
      { candidateId: 'a', presentationKey: 'a:1', text: ' world again', runId: 'run-a', targetByte: 5, insertsOnAccept: true }
    ]);
    const context = page.getByRole('status', { name: 'Completion Context' });
    await expect.element(context).toHaveTextContent('browser-session:browser-document:1:visual');

    await userEvent.keyboard('{Alt>}{ArrowRight}{/Alt}');
    await expect.element(page.getByRole('status', { name: 'Serialized Markdown' }))
      .toHaveTextContent('hello world');
    await page.getByRole('button', { name: 'Simulate checkpoint' }).click();
    await expect.element(page.getByRole('status', { name: 'Checkpoint Revision' }))
      .toHaveTextContent('2');
    await expect.element(context).toHaveTextContent('browser-session:browser-document:1:visual');
    await expect.element(page.getByText('again', { exact: true }).first()).toBeVisible();
    await expect.element(page.getByRole('status', { name: 'Generation Requests' }))
      .toHaveTextContent('0');

    await userEvent.keyboard('{Alt>}{ArrowRight}{/Alt}');
    await expect.element(page.getByRole('status', { name: 'Serialized Markdown' }))
      .toHaveTextContent('hello world again');
    await expect.element(page.getByRole('status', { name: 'Generation Requests' }))
      .toHaveTextContent('1');
  });

  it('advances Shuttle through the shared session while ordinary ghost text is off', async () => {
    render('hello', [
      { candidateId: 'a', presentationKey: 'a:1', text: ' world again', runId: 'run-a', targetByte: 5, insertsOnAccept: true }
    ], { autocomplete: false, shuttle: true });
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

  it('keeps the selected MD remainder visible across acceptance and checkpoint, then reverses it', async () => {
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
    await page.getByRole('button', { name: 'Source checkpoint' }).click();
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
});
