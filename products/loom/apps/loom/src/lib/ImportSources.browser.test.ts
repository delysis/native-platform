import { mount, unmount } from 'svelte';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import ImportSources from './ImportSources.svelte';
import '../app.css';

const ipc = vi.hoisted(() => ({ accounts: vi.fn(), connect: vi.fn() }));
vi.mock('./ipc', async (original) => ({
  ...await original<typeof import('./ipc')>(), importAccounts: ipc.accounts, connectImportAccount: ipc.connect
}));
let mounted: ReturnType<typeof mount> | undefined;
afterEach(async () => {
  if (mounted) await unmount(mounted);
  mounted = undefined; document.body.replaceChildren(); vi.resetAllMocks();
});
async function render(googleClientConfigured: boolean) {
  ipc.accounts.mockResolvedValue([]);
  ipc.connect.mockResolvedValue({ service: 'gmail', email: 'fixture@example.test' });
  const target = document.createElement('div'); document.body.append(target);
  const onSettings = vi.fn();
  mounted = mount(ImportSources, { target, props: {
    projectId: 'project', sessionId: 'session', documentTitle: 'My writing',
    googleClientConfigured, onSettings, onUse: vi.fn()
  } });
  await page.getByText('Import sources', { exact: true }).click();
  return { onSettings };
}

it('replaces credential fields with the settings source and leaves import actions available', async () => {
  const { onSettings } = await render(false);
  expect(document.querySelector('input[type="password"]')).toBeNull();
  expect((page.getByRole('button', { name: 'Connect Gmail', exact: true }).element() as HTMLButtonElement).disabled).toBe(true);
  await expect.element(page.getByRole('button', { name: 'Choose files', exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Open settings file', exact: true }).click();
  expect(onSettings).toHaveBeenCalledOnce();
  expect(ipc.connect).not.toHaveBeenCalled();
});

it('does not authorize from configuration alone and sends no client secret through the browser', async () => {
  await render(true);
  expect(ipc.connect).not.toHaveBeenCalled();
  await page.getByRole('button', { name: 'Connect Gmail', exact: true }).click();
  expect(ipc.connect).toHaveBeenCalledExactlyOnceWith('project', 'session', 'gmail');
  await expect.element(page.getByRole('status')).toHaveTextContent('Connected fixture@example.test');
});
