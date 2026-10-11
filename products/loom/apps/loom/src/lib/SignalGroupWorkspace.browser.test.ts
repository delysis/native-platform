import { mount, unmount } from 'svelte';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import SignalGroupWorkspace from './SignalGroupWorkspace.svelte';
import type { SignalCommand, SignalEvent, SignalGroupWorkspaceReview } from './signal';
import '../app.css';

const ipc = vi.hoisted(() => ({ request: vi.fn() }));
vi.mock('./signal', async original => ({ ...await original<typeof import('./signal')>(), signalRequest: ipc.request }));
let pane: ReturnType<typeof mount> | null = null;
const workspace = { id: '02020202-0202-0202-0202-020202020202', title: 'Our garden' };
const prepared = (): SignalGroupWorkspaceReview => ({ id: 'review-one', workspace, before: 'Keep these words.', after: `Keep these words.\n\nOur garden\nloom://workspace/${workspace.id}`, revision: 8, state: 'prepared', notification: 'none' });
const reply = (review: SignalGroupWorkspaceReview | null, conversation = 'friends'): SignalEvent => ({ kind: 'group_workspace', conversation_id: conversation, review });
beforeEach(() => { ipc.request.mockReset(); });
afterEach(async () => { if (pane) await unmount(pane); pane = null; document.body.replaceChildren(); });
function render(conversation = 'friends') {
  const target = document.createElement('div'); document.body.append(target);
  pane = mount(SignalGroupWorkspace, { target, props: { conversation, workspaces: [workspace], connected: true, onChanged: () => {} } });
}
async function open() { await page.getByRole('button', { name: 'Workspace in group description', exact: true }).click(); }

it('reviews the full description before publishing and checking never sends a member notification', async () => {
  let review: SignalGroupWorkspaceReview | null = null;
  ipc.request.mockImplementation(async (command: SignalCommand) => {
    if (command.kind === 'prepare_group_workspace') review = prepared();
    if (command.kind === 'publish_group_workspace') {
      expect(command.review_id).toBe('review-one'); review = { ...prepared(), state: 'unconfirmed' };
    }
    if (command.kind === 'check_group_workspace') review = { ...prepared(), state: 'published' };
    if (command.kind === 'notify_group_workspace') review = { ...prepared(), state: 'published', notification: 'unconfirmed' };
    return reply(review);
  });
  render(); expect(ipc.request).not.toHaveBeenCalled(); await open();
  expect(document.body.textContent).not.toContain('Publish description');
  await page.getByRole('button', { name: 'Preview description', exact: true }).click();
  await expect.poll(() => document.querySelector('[aria-label="Reviewed group description"]')?.textContent).toBe(prepared().after);
  await page.getByRole('button', { name: 'Publish description', exact: true }).click();
  await expect.element(page.getByRole('button', { name: 'Preview description' })).toBeDisabled();
  await page.getByRole('button', { name: 'Check description' }).click();
  await expect.element(page.getByText('Workspace link published.')).toBeVisible();
  expect(ipc.request.mock.calls.some(([command]) => command.kind === 'notify_group_workspace')).toBe(false);
  await page.getByRole('button', { name: 'Notify group members' }).click();
  await expect.element(page.getByText('Notification delivery is unconfirmed.', { exact: false })).toBeVisible();
  expect(document.body.textContent).not.toContain('Notify group members');
  await page.getByRole('button', { name: 'Check description' }).click();
  expect(ipc.request.mock.calls.filter(([command]) => command.kind === 'notify_group_workspace')).toHaveLength(1);
  expect(ipc.request.mock.calls.some(([command]) => command.kind === 'send')).toBe(false);
});

it('recovers an IPC failure by reading the saved review before another mutation', async () => {
  ipc.request.mockImplementation(async (command: SignalCommand) => {
    if (command.kind === 'publish_group_workspace') throw new Error('Reply lost');
    return reply(prepared());
  });
  render(); await open(); await page.getByRole('button', { name: 'Publish description', exact: true }).click();
  await expect.element(page.getByRole('alert')).toHaveTextContent('Reply lost');
  expect(document.body.textContent).not.toContain('Publish description');
  ipc.request.mockResolvedValue(reply({ ...prepared(), state: 'published', notification: 'unconfirmed' }));
  await page.getByRole('button', { name: 'Reopen saved review' }).click();
  await expect.element(page.getByText('Workspace link published.')).toBeVisible();
  expect(ipc.request).toHaveBeenLastCalledWith({ kind: 'group_workspace', conversation_id: 'friends' });
  expect(ipc.request.mock.calls.filter(([command]) => command.kind === 'publish_group_workspace')).toHaveLength(1);
});

it('rejects an unrelated review and discards a closed conversation’s late reply', async () => {
  let finish!: (event: SignalEvent) => void;
  ipc.request.mockImplementation(() => new Promise(resolve => finish = resolve));
  render(); await open(); await expect.poll(() => typeof finish).toBe('function');
  await unmount(pane!); pane = null; document.body.replaceChildren();
  ipc.request.mockResolvedValue(reply(null, 'other-group')); render('other-group'); await open();
  finish(reply(prepared()));
  await expect.element(page.getByRole('button', { name: 'Preview description' })).toBeEnabled();
  expect(document.body.textContent).not.toContain('Keep these words.');
  ipc.request.mockResolvedValue(reply(prepared()));
  await page.getByRole('button', { name: 'Preview description' }).click();
  await expect.element(page.getByRole('alert')).toHaveTextContent('Signal returned an unrelated workspace review.');
  expect(document.body.textContent).not.toContain('Publish description');
});
