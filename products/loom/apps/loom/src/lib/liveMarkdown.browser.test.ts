import { mount, unmount } from 'svelte';
import { afterEach, describe, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import '../app.css';
import EditorBrowserHarness from './EditorBrowserHarness.svelte';
import { canRoundTripMarkdownExactly, parseVisualMarkdown } from './markdownSafety';

let mounted: ReturnType<typeof mount> | undefined;
afterEach(async () => {
  if (mounted) await unmount(mounted);
  mounted = undefined;
  document.body.replaceChildren();
});
async function open(source = '') {
  const target = document.createElement('div');
  document.body.append(target);
  mounted = mount(EditorBrowserHarness, { target, props: { initialValue: source, autocomplete: false } });
  const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
  await editor.click();
}
function markdown() {
  return page.getByRole('status', { name: 'Serialized Markdown' }).element().textContent;
}

describe('live Markdown typing', () => {
  it('renders emphasis, links and code as their closing delimiters are typed', async () => {
    await open();
    await userEvent.keyboard('A **bold** and *gentle* [[link](https://example.com) with `*literal*`.');
    await expect.poll(markdown).toBe('A **bold** and *gentle* [link](https://example.com) with `*literal*`.');
    expect(document.querySelector('.ProseMirror strong')?.textContent).toBe('bold');
    expect(document.querySelector('.ProseMirror em')?.textContent).toBe('gentle');
    expect(document.querySelector('.ProseMirror a')?.getAttribute('href')).toBe('https://example.com');
    expect(document.querySelector('.ProseMirror code')?.textContent).toBe('*literal*');
  });

  it('opens a fence on Enter, preserves literal code, and closes into ordinary prose', async () => {
    await open();
    await userEvent.keyboard('```text{Enter}**literal**{Enter}```{Enter}After');
    await expect.poll(markdown).toBe('```text\n**literal**\n```\n\nAfter');
    expect(document.querySelector('.ProseMirror pre > code')?.textContent).toBe('**literal**');
    expect(document.querySelector('.ProseMirror strong')).toBeNull();
    expect(document.querySelector('.ProseMirror p')?.textContent).toBe('After');
  });

  it('renders deep headings while leaving unsupported authored syntax intact', async () => {
    await open();
    await userEvent.keyboard('#### Detail{Enter}~~unimplemented~~');
    expect(document.querySelector('.ProseMirror h4')?.textContent).toBe('Detail');
    expect(document.querySelector('.ProseMirror p')?.textContent).toBe('~~unimplemented~~');
    await expect.poll(markdown).toContain('unimplemented');
    expect(parseVisualMarkdown(markdown()!).lastChild?.textContent).toBe('~~unimplemented~~');
    expect(canRoundTripMarkdownExactly('| a | b |\n| - | - |\n| c | d |')).toBe(false);
  });
});
