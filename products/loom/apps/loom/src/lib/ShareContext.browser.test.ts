import { mount, unmount } from 'svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import ShareContext from './ShareContext.svelte';
import type { ContextPublicationReview } from './contextPublication';
import '../app.css';

const ipc = vi.hoisted(() => ({ review: vi.fn(), publish: vi.fn() }));
vi.mock('./contextPublication', () => ({ reviewContext: ipc.review, publishContext: ipc.publish }));
const scope = { projectId: 'project', sessionId: 'session', documentId: 'source' };
const review: ContextPublicationReview = {
  request: { source: 'source', publication: 'chosen-id', fingerprint: 'reviewed-by-native' },
  path: 'Context/Garden.md', material: { markdown: 'Exact reviewed words.', files: [{ id: 'audio', name: 'tone.wav', byte_count: 1024, markdown: '[Audio](loom-attachment:audio)' }] },
  members: ['Alice', 'Bob'], started: false, published: false
};
const result = { document_id: 'shared-document', path: review.path, reference: '@"Context/Garden.md"' };
let mounted: ReturnType<typeof mount> | null = null;
beforeEach(() => { vi.clearAllMocks(); ipc.review.mockResolvedValue(review); ipc.publish.mockResolvedValue(result); });
afterEach(async () => { if (mounted) await unmount(mounted); mounted = null; document.body.replaceChildren(); });
function render(readonly = false) {
  const target = document.createElement('div'); document.body.append(target);
  const beforeReview = vi.fn().mockResolvedValue(true), onOpen = vi.fn().mockResolvedValue(undefined);
  mounted = mount(ShareContext, { target, props: { scope, readonly, beforeReview, onOpen } });
  return { beforeReview, onOpen };
}

describe('reviewed context publication', () => {
  it('shows exact words, files and people before publication and exposes the reusable reference', async () => {
    const { beforeReview, onOpen } = render();
    await page.getByRole('button', { name: 'Share context as a document…' }).click();
    expect(beforeReview).toHaveBeenCalledOnce();
    expect(ipc.review).toHaveBeenCalledWith(scope);
    await expect.element(page.getByText('Exact reviewed words.')).toBeVisible();
    await expect.element(page.getByRole('list', { name: 'Files to share' })).toHaveTextContent('tone.wav');
    expect(document.body.textContent).toContain('Alice, Bob');
    expect(ipc.publish).not.toHaveBeenCalled();
    await page.getByRole('button', { name: 'Share with cabal' }).click();
    expect(ipc.publish).toHaveBeenCalledWith(scope, review.request);
    await expect.element(page.getByText(result.reference)).toBeVisible();
    expect(onOpen).not.toHaveBeenCalled();
    await page.getByRole('button', { name: 'Open shared context' }).click();
    expect(onOpen).toHaveBeenCalledWith(result);
  });

  it('retries a lost reply using exactly the original approved publication', async () => {
    ipc.publish.mockRejectedValueOnce(new Error('reply lost'));
    render();
    await page.getByRole('button', { name: 'Share context as a document…' }).click();
    await page.getByRole('button', { name: 'Share with cabal' }).click();
    await expect.element(page.getByRole('alert')).toHaveTextContent('reply lost');
    await page.getByRole('button', { name: 'Finish sharing' }).click();
    expect(ipc.publish).toHaveBeenCalledTimes(2);
    expect(ipc.publish.mock.calls[0]).toEqual(ipc.publish.mock.calls[1]);
    expect(ipc.review).toHaveBeenCalledOnce();
  });

  it('reopens a retained publication with its original identity after the pane was closed', async () => {
    ipc.review.mockResolvedValue({ ...review, started: true, published: true });
    render();
    await page.getByRole('button', { name: 'Share context as a document…' }).click();
    await expect.element(page.getByText('This context already has a shared document. Its later edits are kept there.')).toBeVisible();
    expect(document.querySelector('pre')).toBeNull();
    await page.getByRole('button', { name: 'Find shared context' }).click();
    expect(ipc.publish).toHaveBeenCalledWith(scope, review.request);
  });

  it('does not transfer a late review into another document or publish on unmount', async () => {
    let resolve: ((value: ContextPublicationReview) => void) | undefined;
    ipc.review.mockImplementation(() => new Promise(done => { resolve = done; }));
    render();
    await page.getByRole('button', { name: 'Share context as a document…' }).click();
    await expect.poll(() => resolve).toBeDefined();
    await unmount(mounted!); mounted = null;
    resolve!(review);
    render(true);
    await expect.element(page.getByRole('button', { name: 'Share context as a document…' })).toBeDisabled();
    expect(ipc.publish).not.toHaveBeenCalled();
    expect(document.querySelector('pre')).toBeNull();
  });

  it('requires another visible review when membership changes during an uncertain publication', async () => {
    ipc.publish.mockRejectedValueOnce(new Error('Membership changed'));
    render();
    await page.getByRole('button', { name: 'Share context as a document…' }).click();
    await page.getByRole('button', { name: 'Share with cabal' }).click();
    const renewed = { ...review, started: true, members: ['Alice', 'Bob', 'Carol'], request: { ...review.request, fingerprint: 'new-membership' } };
    ipc.review.mockResolvedValue(renewed);
    await page.getByRole('button', { name: 'Review saved publication' }).click();
    expect(document.body.textContent).toContain('Alice, Bob, Carol');
    expect(ipc.publish).toHaveBeenCalledOnce();
    await page.getByRole('button', { name: 'Finish sharing' }).click();
    expect(ipc.publish).toHaveBeenLastCalledWith(scope, renewed.request);
  });
});
