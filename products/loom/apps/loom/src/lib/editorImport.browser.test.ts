import { mount, unmount } from 'svelte';
import { describe, expect, it, afterEach } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import '../app.css';
import Harness from './EditorImportBrowserHarness.svelte';
let mounted: ReturnType<typeof mount>;
afterEach(async () => { if (mounted) await unmount(mounted); document.body.replaceChildren(); });
function render() { const target = document.createElement('div'); document.body.append(target); mounted = mount(Harness, { target }); }
describe('shared editor import integration', () => {
  it('routes a macOS native payload to blank context space without manuscript fallthrough and round trips both panes', async () => {
    render();
    await page.getByRole('button', { name: 'Native payload to context' }).click();
    await expect.element(page.getByRole('status', { name: 'Drop owner' })).toHaveTextContent('context');
    await expect.element(page.getByRole('status', { name: 'Manuscript markdown' })).toHaveTextContent('Opening.');
    await expect.element(page.getByRole('textbox', { name: 'Context editor', exact: true })).toHaveTextContent('Editable café • 界 🖋.');
    await page.getByRole('button', { name: 'Native payload to manuscript' }).click();
    await expect.element(page.getByRole('status', { name: 'Manuscript markdown' })).toHaveTextContent('Editable café • 界 🖋.');
    await page.getByRole('button', { name: 'Toggle both editors' }).click();
    await expect.element(page.getByRole('textbox', { name: 'Context source' })).toBeVisible();
    await expect.element(page.getByRole('textbox', { name: 'Manuscript source' })).toBeVisible();
    await page.getByRole('button', { name: 'Toggle both editors' }).click();
    await expect.element(page.getByRole('textbox', { name: 'Context editor', exact: true })).toHaveTextContent('Editable café • 界 🖋.');
    await expect.element(page.getByRole('textbox', { name: 'Manuscript editor', exact: true })).toHaveTextContent('Editable café • 界 🖋.');
  });
  it('focuses and edits through the lower blank area of each visual surface', async () => {
    render();
    const keyboard = userEvent.setup();
    for (const name of ['Context editor', 'Manuscript editor']) {
      const editor = page.getByRole('textbox', { name, exact: true });
      await editor.click({ position: { x: 100, y: 200 } });
      expect(document.activeElement).toBe(editor.element());
      await keyboard.keyboard('Blank surface entry.');
      await expect.element(editor).toHaveTextContent('Blank surface entry.');
    }
    await keyboard.cleanup();
  });
});
