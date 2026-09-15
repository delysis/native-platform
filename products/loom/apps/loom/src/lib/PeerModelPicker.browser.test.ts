import { mount, unmount } from 'svelte';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import Harness from './PeerModelPickerBrowserHarness.svelte';
import type { CabalSnapshot } from './cabal';
import type { PeerOffers } from './compute';
const api = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: api.invoke }));
const cabal: CabalSnapshot = {
  id: 'cabal', name: 'Garden', my_key: 'alice', roster_hash: 'roster',
  roster: { payload: { owner: 'alice', epoch: 1, members: [{ key: 'alice', name: 'Alice' }, { key: 'bob', name: 'Bob' }] } },
  peers: [], documents: [], deleted_document_ids: [], problems: [], read_only: false, orphaned_changes: 0, removed_documents: 0,
};
const offer: PeerOffers = {
  host: 'bob', roster_hash: 'roster', grants: [{ id: 'grant', cabal: 'cabal', epoch: 1, peer: 'alice',
    model: { media: ['image', 'audio'], name: 'Shared Gemma', fingerprint: 'fingerprint' }, max_output_tokens: 256, max_seconds: 30, jobs: 8 }],
};
let mounted: ReturnType<typeof mount> | undefined;
afterEach(async () => { if (mounted) await unmount(mounted); mounted = undefined; document.body.replaceChildren(); vi.resetAllMocks(); });
function open() {
  const target = document.createElement('div'); document.body.append(target);
  mounted = mount(Harness, { target, props: { cabal } });
}

it('uses local by default, discovers only the selected friend, and binds the exact selected grant', async () => {
  api.invoke.mockResolvedValue(offer); open();
  expect(api.invoke).not.toHaveBeenCalled();
  await page.getByRole('combobox', { name: 'Run on' }).selectOptions('bob');
  await expect.element(page.getByRole('combobox', { name: 'Friend’s model' })).toBeVisible();
  await expect.element(page.getByRole('combobox', { name: 'Friend’s model' })).toHaveTextContent('text · images · audio');
  expect(api.invoke.mock.calls).toEqual([['plugin:loom|compute_peer_offers', { projectId: 'project', sessionId: 'session', host: 'bob' }]]);
  await expect.element(page.getByLabelText('Selected peer')).toHaveTextContent('"host":"bob","target":null');
  await page.getByRole('combobox', { name: 'Friend’s model' }).selectOptions('grant');
  await expect.element(page.getByLabelText('Selected peer')).toHaveTextContent('"roster_hash":"roster"');
  await expect.element(page.getByLabelText('Selected peer')).toHaveTextContent('"fingerprint":"fingerprint"');
  await page.getByRole('combobox', { name: 'Run on' }).selectOptions('');
  await expect.element(page.getByLabelText('Selected peer')).toHaveTextContent('{"host":"","target":null}');
  expect(api.invoke).toHaveBeenCalledTimes(1);
});

it('discards discovery that arrives after a workspace switch without selecting any remote model', async () => {
  let resolve!: (offers: PeerOffers) => void;
  api.invoke.mockImplementation(() => new Promise(done => { resolve = done; })); open();
  await page.getByRole('combobox', { name: 'Run on' }).selectOptions('bob');
  await expect.element(page.getByRole('button', { name: 'Checking…' })).toBeVisible();
  await page.getByRole('button', { name: 'Switch workspace' }).click(); resolve(offer);
  await expect.element(page.getByLabelText('Selected peer')).toHaveTextContent('{"host":"","target":null}');
  await expect.element(page.getByRole('combobox', { name: 'Friend’s model' })).not.toBeInTheDocument();
  expect(api.invoke).toHaveBeenCalledTimes(1);
});

it('refuses an offer bound to a different membership snapshot', async () => {
  api.invoke.mockResolvedValue({ ...offer, roster_hash: 'old-roster' }); open();
  await page.getByRole('combobox', { name: 'Run on' }).selectOptions('bob');
  await expect.element(page.getByRole('status', { name: 'Peer model status' })).toHaveTextContent('Membership changed');
  await expect.element(page.getByLabelText('Selected peer')).toHaveTextContent('"target":null');
  await expect.element(page.getByRole('combobox', { name: 'Friend’s model' })).not.toBeInTheDocument();
});
