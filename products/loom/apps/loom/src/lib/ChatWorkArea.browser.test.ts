import { mount, unmount } from 'svelte';
import { afterEach, expect, test, vi } from 'vitest';
import { page } from 'vitest/browser';
const fixture = vi.hoisted(() => ({ open: vi.fn(), list: vi.fn(), documents: vi.fn() }));
vi.mock('./chatSession', async () => {
  const { writable } = await import('svelte/store');
  return {
    chatSession: writable({ ready: true, loading: false, error: '', conversations: [{ id: 'host', title: 'Consultation' }], selected: { id: 'host', title: 'Consultation', messages: [{ id: 'reply', role: 'assistant', content: '<script>untrusted()</script> Exact answer.', attribution: { invocation_id: 'invocation', source_id: 'persona', label: 'Mom' } }] }, draft: '', attachments: [], pending: null, candidates: [], receipts: {} }),
    initializeChat: vi.fn(), selectChat: vi.fn(), newChat: vi.fn(), updateChatDraft: vi.fn(), sendChat: vi.fn(), stopChat: vi.fn(), attachChatFile: vi.fn(), pasteChatSource: vi.fn(), listConsultSources: fixture.list, openConsultSource: fixture.open,
  };
});
import ChatWorkArea from './ChatWorkArea.svelte';
import '../app.css';
let mounted: ReturnType<typeof mount> | undefined;
afterEach(async () => { if (mounted) await unmount(mounted); mounted = undefined; document.body.replaceChildren(); vi.clearAllMocks(); });
function render(width: number) {
  const target = document.createElement('div'); target.style.cssText = `position:relative;width:${width}px;height:700px`;
  document.body.append(target); mounted = mount(ChatWorkArea, { target, props: { onDocuments: fixture.documents } }); return target;
}
test('opens the retained attributed source, keeps untrusted text inert, and displays a stale blocker', async () => {
  const source = { conversation_id: 'source-chat', message_id: 'source-message', representation: { attachment_id: 'attachment', root_sha256: 'a'.repeat(64), manifest_sha256: 'b'.repeat(64), artifact_ids: ['text'] } };
  fixture.list.mockResolvedValue([source]); fixture.open.mockResolvedValue({ source, canonical_text: '<img src="https://example.com/tracker"> Retained source.' });
  render(1200);
  await expect.element(page.getByText('<script>untrusted()</script> Exact answer.', { exact: true })).toBeVisible();
  expect(document.querySelector('.message-content script')).toBeNull();
  await page.getByRole('button', { name: 'Sources', exact: true }).click();
  await page.getByRole('button', { name: 'Open occurrence attachme' }).click();
  expect(fixture.open).toHaveBeenCalledWith('invocation', 'persona', source);
  await expect.element(page.getByText('<img src="https://example.com/tracker"> Retained source.', { exact: true })).toBeVisible();
  expect(document.querySelector('.sources img')).toBeNull();
  fixture.open.mockRejectedValue(new Error('the selected source occurrence was removed'));
  await page.getByRole('button', { name: 'Open occurrence attachme' }).click();
  await expect.element(page.getByRole('alert')).toHaveTextContent('the selected source occurrence was removed');
  await page.getByRole('button', { name: 'Documents', exact: true }).click(); expect(fixture.documents).toHaveBeenCalledOnce();
});
test('keeps composer and navigation within a compact work area', async () => {
  await page.viewport(540, 800);
  const target = render(540);
  await expect.element(page.getByRole('textbox', { name: 'Message', exact: true })).toBeVisible();
  expect(target.scrollWidth).toBeLessThanOrEqual(target.clientWidth);
  expect(document.querySelector('.chat-work-area')!.getBoundingClientRect().width).toBe(540);
  await page.screenshot();
});
