import { mount, unmount } from 'svelte';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import EditorBrowserHarness from './EditorBrowserHarness.svelte';

let mounted: ReturnType<typeof mount> | undefined;
afterEach(async () => {
  if (mounted) await unmount(mounted);
  document.body.replaceChildren();
});

it('inserts a source quotation after the clicked reference without splitting its label', async () => {
  const href = `loom-material:material-${'a'.repeat(64)}`;
  const original = `Before [@paper.pdf](${href}) after.\n`;
  const markdown = () => page.getByRole('status', { name: 'Serialized Markdown' }).element().textContent ?? '';
  const target = document.createElement('div');
  document.body.append(target);
  const editor = mount(EditorBrowserHarness, { target, props: { initialValue: original } });
  mounted = editor;
  const link = page.getByRole('link', { name: '@paper.pdf' });
  await link.click();
  expect(editor.insertSourceQuotation(link.element(), '> Exact excerpt.\n')).toBe(true);
  expect(document.querySelectorAll(`a[href="${href}"]`)).toHaveLength(1);
  expect(document.querySelector(`a[href="${href}"]`)?.textContent).toBe('@paper.pdf');
  await expect.poll(markdown).toContain('Exact excerpt.');
  expect(markdown().indexOf('Exact excerpt.')).toBeGreaterThan(markdown().indexOf(href));
  expect(markdown().indexOf(' after.')).toBeGreaterThan(markdown().indexOf('Exact excerpt.'));
  await userEvent.setup().keyboard('{Meta>}z{/Meta}');
  await expect.poll(markdown).toBe(original);
});
