import { mount, unmount } from 'svelte';
import { writable } from 'svelte/store';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import type { CabalSnapshot } from './cabal';
import Harness from './CabalOwnershipBrowserHarness.svelte';
import '../app.css';

let component: ReturnType<typeof mount> | null = null;
afterEach(async () => { if (component) await unmount(component); component = null; document.body.replaceChildren(); });
function fixture(): CabalSnapshot {
  return { id:'garden', name:'Garden', my_key:'sage', roster_hash:'reviewed-membership',
    roster:{payload:{owner:'sage',epoch:0,members:[{key:'sage',name:'Sage'},{key:'fern',name:'Fern'},{key:'moss',name:'Moss'}]}},
    orphaned_changes:0, removed_documents:0, peers:[], documents:[], deleted_document_ids:[], problems:[], read_only:false };
}
function render(onTransfer = vi.fn(async (_member: string, _roster: string) => {})) {
  const snapshot = writable(fixture());
  const target = document.createElement('div'); document.body.append(target);
  component = mount(Harness, { target, props:{ snapshot, onTransfer } });
  return { snapshot, onTransfer };
}
async function review() {
  await page.getByText('Hand over the keys', { exact:true }).click();
  await page.getByLabelText('Next owner').selectOptions('fern');
  await page.getByRole('button', { name:'Review handoff', exact:true }).click();
}

it('requires an explicit review and sends its exact recipient and membership once', async () => {
  const { onTransfer } = render();
  await review();
  expect(onTransfer).not.toHaveBeenCalled();
  await expect.element(page.getByText('will own this cabal', { exact:false })).toBeVisible();
  // Changing the selector cannot retarget an already displayed review.
  await page.getByLabelText('Next owner').selectOptions('moss');
  await page.getByRole('button', { name:'Give Fern the keys', exact:true }).click();
  expect(onTransfer).toHaveBeenCalledExactlyOnceWith('fern','reviewed-membership');
  await expect.element(page.getByRole('button', { name:'Give Fern the keys', exact:true })).not.toBeInTheDocument();
});

it('membership changes invalidate the displayed review and require another review', async () => {
  const { snapshot, onTransfer } = render();
  await review();
  snapshot.update(current => ({ ...current, roster_hash:'new-membership' }));
  await expect.element(page.getByRole('button', { name:'Give Fern the keys', exact:true })).toBeDisabled();
  await expect.element(page.getByRole('status')).toHaveTextContent('Membership changed. Review the handoff again.');
  expect(onTransfer).not.toHaveBeenCalled();
  await page.getByRole('button', { name:'Review handoff', exact:true }).click();
  await page.getByRole('button', { name:'Give Fern the keys', exact:true }).click();
  expect(onTransfer).toHaveBeenCalledExactlyOnceWith('fern','new-membership');
});

it('a lost reply is not replayed and a new owner removes the old controls', async () => {
  let reject!: (reason: Error) => void;
  const onTransfer = vi.fn((_member: string, _roster: string) => new Promise<void>((_, fail) => { reject = fail; }));
  const { snapshot } = render(onTransfer);
  await review();
  await page.getByRole('button', { name:'Give Fern the keys', exact:true }).click();
  await expect.element(page.getByRole('button', { name:'Give Fern the keys', exact:true })).toBeDisabled();
  reject(new Error('Reply lost'));
  await expect.element(page.getByRole('alert')).toHaveTextContent('Reply lost');
  expect(onTransfer).toHaveBeenCalledTimes(1);
  await expect.element(page.getByRole('button', { name:'Give Fern the keys', exact:true })).not.toBeInTheDocument();
  snapshot.update(current => ({ ...current, roster_hash:'handed-off', roster:{payload:{...current.roster.payload,owner:'fern'}} }));
  await expect.element(page.getByText('Hand over the keys', { exact:true })).not.toBeInTheDocument();
  expect(onTransfer).toHaveBeenCalledTimes(1);
});
