import { mount, unmount } from 'svelte';
import { afterEach, describe, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import '../app.css';
import CompletionControllerBrowserHarness from './CompletionControllerBrowserHarness.svelte';

let mounted: ReturnType<typeof CompletionControllerBrowserHarness> | null = null;

afterEach(async () => {
  if (mounted) await unmount(mounted);
  mounted = null;
  document.body.replaceChildren();
});

function render(mode: 'visual' | 'source', loompad = false) {
  const target = document.createElement('div');
  document.body.append(target);
  mounted = mount(CompletionControllerBrowserHarness, { target, props: { mode, loompad } });
  return mounted;
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
  it.each(['visual', 'source'] as const)(
    'inserts one exact %s prefix through a real Loompad chord and rejects stale authority',
    async (mode) => {
      const keyboard = userEvent.setup();
      const harness = render(mode, true);
      const editor = page.getByRole('textbox');
      await editor.click();
      const manuscript = () => page.getByRole('status', { name: 'Controller Markdown' }).element().textContent;
      await expect.poll(() => document.querySelector('[data-direction="w"]')?.getAttribute('aria-label')).toBe('W:  one two');
      await expect.poll(() => document.querySelector(mode === 'visual' ? '.loom-visual-ghost' : '.loom-source-ghost-text')?.textContent).toBe(' one two');
      // Initial focus may report a caret change before the family is installed.
      // The authorized insertion itself must leave that count unchanged.
      const initialInvalidations = page.getByRole('status', { name: 'Controller Invalidations' }).element().textContent;
      expect(harness.attemptLoompad('wrong-candidate', 'candidate-a:presentation', ' one')).toBe(false);
      expect(harness.attemptLoompad('candidate-a', 'stale-key', ' one')).toBe(false);
      expect(harness.attemptLoompad('candidate-a', 'candidate-a:presentation', ' unrelated')).toBe(false);
      expect(manuscript()).toBe('Hello');

      await keyboard.keyboard('[KeyW>][KeyI>3]');
      const expected = mode === 'visual' ? 'Hello one' : 'Hello one ';
      await expect.poll(manuscript).toBe(expected);
      await expect.element(page.getByRole('status', { name: 'Controller Actions' })).toHaveTextContent('1');
      await expect.element(page.getByRole('status', { name: 'Controller Action Kind' })).toHaveTextContent('loompad');
      await expect.element(page.getByRole('status', { name: 'Controller Frozen' })).toHaveTextContent('yes');
      expect(page.getByRole('status', { name: 'Controller Remainder' }).element().textContent).toBe(mode === 'visual' ? ' two' : 'two');
      expect(page.getByRole('status', { name: 'Controller Invalidations' }).element().textContent).toBe(initialInvalidations);
      expect(harness.attemptLoompad('candidate-a', 'candidate-a:presentation', ' one')).toBe(false);
      expect(manuscript()).toBe(expected);
      await keyboard.keyboard('[/KeyI][/KeyW]');
      await keyboard.cleanup();
    }
  );

});
