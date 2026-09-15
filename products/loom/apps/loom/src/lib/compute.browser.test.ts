import { mount, unmount } from 'svelte';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import ComputeHostControls from './ComputeHostControls.svelte';
import { ComputeSharing, type ComputeGrant, type ComputeGrantRequest, type ComputeHostSnapshot, type ComputeScope, type ComputeModel } from './compute';
import type { CabalSnapshot } from './cabal';
import '../app.css';

let mounted: ReturnType<typeof mount> | undefined;
afterEach(async () => { if (mounted) await unmount(mounted); mounted = undefined; document.body.replaceChildren(); });
const scope: ComputeScope = { projectId: 'project', sessionId: 'session', cabalId: 'cabal' };
const cabal: CabalSnapshot = {
  id: 'cabal', name: 'Garden', my_key: 'alice', roster_hash: 'roster',
  roster: { payload: { owner: 'alice', epoch: 1, members: [{ key: 'alice', name: 'Alice' }, { key: 'bob', name: 'Bob' }] } },
  peers: [], documents: [], deleted_document_ids: [], problems: [], read_only: false, orphaned_changes: 0, removed_documents: 0,
};
function fixture(media: ComputeModel['media'] = []) {
  let host: ComputeHostSnapshot = { model: { media, name: 'Little model', fingerprint: 'model' }, idle: true, grants: [], problem: null };
  const api = {
    snapshot: vi.fn(async () => ({ ...host })),
    grant: vi.fn(async (_scope: ComputeScope, request: ComputeGrantRequest): Promise<ComputeGrant> => {
      const grant = { id: request.id, cabal: 'cabal', epoch: 1, peer: request.member_key,
        model: { media, name: 'Little model', fingerprint: request.model_fingerprint }, jobs: request.jobs,
        max_output_tokens: request.max_output_tokens, max_seconds: request.max_seconds };
      host = { ...host, grants: [{ grant, jobs_remaining: grant.jobs, current: true }] }; return grant;
    }),
    revoke: vi.fn(async (_scope: ComputeScope, id: string) => { host = { ...host, grants: host.grants.filter(item => item.grant.id !== id) }; }),
  };
  return { api, sharing: new ComputeSharing(api), changeModel: () => { host = { ...host, model: { media: [], name: 'Different model', fingerprint: 'new-model' } }; } };
}
async function open(sharing: ComputeSharing) {
  const target = document.createElement('div'); target.style.width = '250px'; document.body.append(target);
  mounted = mount(ComputeHostControls, { target, props: { cabal, scope, sharing } });
  await page.getByText('Share idle compute', { exact: true }).click();
  return target;
}
async function review() {
  await page.getByRole('combobox', { name: 'Friend' }).selectOptions('bob');
  await page.getByRole('spinbutton', { name: 'Jobs', exact: true }).fill('3');
  await page.getByRole('button', { name: 'Review grant', exact: true }).click();
}

it('reviews one friend and exact limits before granting, then shows the budget and revokes it', async () => {
  const { api, sharing } = fixture(['image', 'audio']); const target = await open(sharing); await review();
  expect(api.grant).not.toHaveBeenCalled();
  await expect.element(page.getByLabelText('Review compute grant')).toHaveTextContent('3 jobs, each up to 256 tokens and 30s');
  await expect.element(page.getByLabelText('Review compute grant')).toHaveTextContent('Inputs: text · images · audio.');
  await page.getByRole('button', { name: 'Share with Bob' }).click();
  await expect.element(page.getByRole('list', { name: 'Compute grants' })).toHaveTextContent('3 of 3 jobs left');
  expect(api.grant).toHaveBeenCalledTimes(1);
  expect(api.grant.mock.calls[0][1]).toMatchObject({ member_key: 'bob', model_fingerprint: 'model', roster_hash: 'roster', jobs: 3 });
  expect(target.scrollWidth).toBeLessThanOrEqual(target.clientWidth);
  await page.getByRole('button', { name: 'Revoke grant', exact: true }).click();
  await expect.element(page.getByRole('list', { name: 'Compute grants' })).not.toBeInTheDocument();
  expect(api.revoke).toHaveBeenCalledTimes(1);
});

it('disables an old review when polling observes a different loaded model', async () => {
  const { api, sharing, changeModel } = fixture(); await open(sharing); await review(); changeModel();
  await expect.element(page.getByRole('button', { name: 'Share with Bob' })).toBeDisabled();
  await expect.element(page.getByLabelText('Review compute grant')).toHaveTextContent('Little model');
  expect(api.grant).not.toHaveBeenCalled();
});

it('keeps an uncertain grant across unmount, and revokes that same ID without another grant', async () => {
  const { api, sharing } = fixture(); api.grant.mockRejectedValue(new Error('Lost reply'));
  await open(sharing); await review(); await page.getByRole('button', { name: 'Share with Bob' }).click();
  await expect.element(page.getByLabelText('Pending compute grant')).toHaveTextContent('has not been confirmed');
  const id = api.grant.mock.calls[0][1].id;
  await unmount(mounted!); mounted = undefined; document.body.replaceChildren();
  await open(sharing);
  await expect.element(page.getByLabelText('Pending compute grant')).toHaveTextContent('Bob · Little model');
  await page.getByRole('button', { name: 'Revoke pending grant' }).click();
  await expect.element(page.getByLabelText('Pending compute grant')).not.toBeInTheDocument();
  expect(api.revoke.mock.calls).toEqual([[scope, id]]); expect(api.grant).toHaveBeenCalledTimes(1);
});
