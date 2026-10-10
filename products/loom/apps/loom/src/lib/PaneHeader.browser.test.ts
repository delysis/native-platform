import { mount, unmount } from 'svelte';
import { expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import PaneHeader from './PaneHeader.svelte';

it('allows a running pane to collapse even while its presentation selector is locked', async () => {
  const target = document.createElement('div'); document.body.append(target);
  const onCollapse = vi.fn(), onSelect = vi.fn();
  const view = mount(PaneHeader, { target, props: {
    title: 'Chat', choices: [['chat', { title: 'Chat' }], ['browser', { title: 'Browser' }]],
    selected: 'chat', selectionDisabled: true, onSelect, onCollapse
  } });
  try {
    await expect.element(page.getByRole('combobox', { name: 'Pane' })).toBeDisabled();
    await expect.element(page.getByRole('button', { name: 'Collapse Chat' })).toBeEnabled();
    await page.getByRole('button', { name: 'Collapse Chat' }).click();
    expect(onCollapse).toHaveBeenCalledOnce();
    expect(onSelect).not.toHaveBeenCalled();
  } finally { await unmount(view); target.remove(); }
});

it('omits the collapse affordance for the only content pane', async () => {
  const target = document.createElement('div'); document.body.append(target);
  const view = mount(PaneHeader, { target, props: {
    title: 'Writing', collapsible: false, onCollapse: vi.fn()
  } });
  try {
    await expect.element(page.getByText('Writing', { exact: true })).toBeVisible();
    await expect.element(page.getByRole('button', { name: 'Collapse Writing' })).not.toBeInTheDocument();
  } finally { await unmount(view); target.remove(); }
});
