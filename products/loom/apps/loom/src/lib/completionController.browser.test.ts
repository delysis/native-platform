import { mount, unmount } from 'svelte';
import { afterEach, describe, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import '../app.css';
import CompletionControllerBrowserHarness from './CompletionControllerBrowserHarness.svelte';

let mounted: ReturnType<typeof mount> | null = null;

afterEach(async () => {
  if (mounted) await unmount(mounted);
  mounted = null;
  document.body.replaceChildren();
});

function render(mode: 'visual' | 'source'): void {
  const target = document.createElement('div');
  document.body.append(target);
  mounted = mount(CompletionControllerBrowserHarness, { target, props: { mode } });
}

describe('completion controller and editor callback ordering', () => {
  it.each(['visual', 'source'] as const)(
    'keeps the %s remainder through two Option-Right insertions',
    async (mode) => {
      const keyboard = userEvent.setup();
      render(mode);
      const editor = page.getByRole('textbox');
      await editor.click();
      await expect.element(page.getByText('one two', { exact: true }).first()).toBeVisible();

      await keyboard.keyboard('{Alt>}{ArrowRight}{/Alt}');
      await expect.element(page.getByRole('status', { name: 'Controller Markdown' }))
        .toHaveTextContent('Hello one');
      await expect.element(page.getByRole('status', { name: 'Controller Remainder' }))
        .toHaveTextContent('two');
      await expect.element(page.getByText('two', { exact: true }).first()).toBeVisible();

      await keyboard.keyboard('{Alt>}{ArrowRight}{/Alt}');
      await expect.element(page.getByRole('status', { name: 'Controller Markdown' }))
        .toHaveTextContent('Hello one two');
      await keyboard.cleanup();
    }
  );
});
