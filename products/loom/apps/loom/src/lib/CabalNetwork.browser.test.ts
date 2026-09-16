import { mount, unmount } from 'svelte';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import CabalNetwork from './CabalNetwork.svelte';
import '../app.css';

const ipc = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: ipc.invoke }));
let component: ReturnType<typeof mount> | null = null;
const initial = { configured: { mode: 'internet' }, active: { mode: 'internet' }, revision: 'first' };
beforeEach(() => { ipc.invoke.mockReset(); });
afterEach(async () => { if (component) await unmount(component); component = null; document.body.replaceChildren(); });
function render() {
  const target = document.createElement('div'); document.body.append(target);
  component = mount(CabalNetwork, { target });
}
async function toggle() { await page.getByText('Connections', { exact: true }).click(); }

it('reads only on disclosure and saves exact reviewed relay settings without restarting networking', async () => {
  ipc.invoke.mockImplementation(async (command, request) => {
    if (command.endsWith('_get')) return initial;
    return { configured: request.connection, active: initial.active, revision: 'second' };
  });
  render(); expect(ipc.invoke).not.toHaveBeenCalled(); await toggle();
  await page.getByRole('combobox', { name: 'Connect through' }).selectOptions('relays');
  await page.getByLabelText('Relay addresses').fill('https://our.example\nhttps://backup.example');
  expect(ipc.invoke.mock.calls).toHaveLength(1);
  await page.getByRole('button', { name: 'Save connections' }).click();
  expect(ipc.invoke).toHaveBeenLastCalledWith('plugin:loom|cabal_network_set', {
    expected: 'first', connection: { mode: 'relays', urls: ['https://our.example', 'https://backup.example'] }
  });
  await expect.element(page.getByText('Saved. Restart Loom to use these connections; your pairing stays.')).toBeVisible();
  expect(ipc.invoke.mock.calls.every(([command]) => command.startsWith('plugin:loom|cabal_network_'))).toBe(true);
});

it('a lost save reply requires a fresh read before another save and never replays the mutation', async () => {
  let configured = initial.configured;
  ipc.invoke.mockImplementation(async (command, request) => {
    if (command.endsWith('_get')) return { ...initial, configured, revision: configured.mode === 'direct' ? 'second' : 'first' };
    configured = request.connection;
    throw new Error('Reply lost');
  });
  render(); await toggle();
  await page.getByRole('combobox', { name: 'Connect through' }).selectOptions('direct');
  await page.getByRole('button', { name: 'Save connections' }).click();
  await expect.element(page.getByRole('alert')).toHaveTextContent('Reply lost');
  expect(document.querySelector('select')).toBeNull();
  await page.getByRole('button', { name: 'Reload settings' }).click();
  await expect.poll(() => document.querySelector('select')?.value).toBe('direct');
  expect(ipc.invoke.mock.calls.filter(([command]) => command.endsWith('_set'))).toHaveLength(1);
});

it('discards an old disclosure reply after closing and reopening settings', async () => {
  let finish!: (value: unknown) => void;
  ipc.invoke.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  render(); await toggle();
  await expect.poll(() => typeof finish).toBe('function');
  await toggle();
  ipc.invoke.mockResolvedValue({ ...initial, configured: { mode: 'direct' }, revision: 'second' });
  await toggle();
  await expect.poll(() => document.querySelector('select')?.value).toBe('direct');
  finish(initial);
  await new Promise(resolve => setTimeout(resolve, 30));
  expect(document.querySelector('select')?.value).toBe('direct');
});
