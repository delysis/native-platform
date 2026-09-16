import { mount, unmount } from 'svelte';
import { SvelteMap } from 'svelte/reactivity';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import SignalIdentity from './SignalIdentity.svelte';
import type { SignalCommand, SignalEvent, SignalIdentityReview } from './signal';
import '../app.css';

const ipc = vi.hoisted(() => ({ request: vi.fn() }));
vi.mock('./signal', async original => ({ ...await original<typeof import('./signal')>(), signalRequest: ipc.request }));
let pane: ReturnType<typeof mount> | null = null;
const review = (state: SignalIdentityReview['state'] = 'unverified', person = 'alice'): SignalIdentityReview => ({
  recipient_id: person, review_id: `${person}-${state}`, safety_number: '1234567890'.repeat(6),
  qr_code: 'data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciLz4=', state, refreshed_at: null,
});
const reply = (value: SignalIdentityReview, conversation = value.recipient_id): SignalEvent => ({ kind: 'identity', conversation_id: conversation, members: [{ id: value.recipient_id, title: value.recipient_id }], review: value });
beforeEach(() => { ipc.request.mockReset(); });
afterEach(async () => { if (pane) await unmount(pane); pane = null; document.body.replaceChildren(); });

function render(conversation = 'alice') {
  const target = document.createElement('div'); document.body.append(target);
  const state = new SvelteMap([['refresh', 0]]);
  pane = mount(SignalIdentity, { target, props: { conversation, get refreshToken() { return state.get('refresh')!; } } });
  return state;
}

it('accepts only the explicitly compared review and waits for the restarted worker before showing verified', async () => {
  let current = review();
  ipc.request.mockImplementation(async (command: SignalCommand) => {
    if (command.kind === 'verify_identity') {
      expect(command).toEqual({ kind: 'verify_identity', conversation_id: 'alice', recipient_id: 'alice', review_id: 'alice-changed' });
      current = { ...current, state: 'pending' };
    } else if (command.kind === 'identity' && command.refresh) current = review('changed');
    return reply(current);
  });
  const state = render();
  expect(ipc.request).not.toHaveBeenCalled();
  await page.getByRole('button', { name: 'Safety numbers', exact: true }).click();
  await expect.element(page.getByRole('button', { name: 'Mark verified' })).toBeDisabled();
  expect(ipc.request).toHaveBeenCalledWith({ kind: 'identity', conversation_id: 'alice', recipient_id: null, refresh: false });
  await page.getByRole('button', { name: 'Refresh safety number' }).click();
  await expect.element(page.getByRole('button', { name: 'Verify and accept new number' })).toBeDisabled();
  await page.getByRole('checkbox').click();
  state.set('refresh', 1);
  await expect.element(page.getByRole('button', { name: 'Verify and accept new number' })).toBeEnabled();
  await expect.element(page.getByRole('checkbox')).toBeChecked();
  await page.getByRole('button', { name: 'Verify and accept new number' }).click();
  await expect.element(page.getByText('Reconnecting to finish verification…')).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Refresh safety number' })).toBeDisabled();
  expect(document.body.textContent).not.toContain('Verified on this Loom device.');
  current = { ...current, state: 'verified' }; state.set('refresh', 2);
  await expect.element(page.getByText('Verified on this Loom device.')).toBeVisible();
  expect(ipc.request.mock.calls.filter(([command]) => command.kind === 'verify_identity')).toHaveLength(1);
  expect(ipc.request.mock.calls.some(([command]) => command.kind === 'send')).toBe(false);
});

it('discards a closed conversation’s delayed safety number', async () => {
  let finish!: (event: SignalEvent) => void;
  ipc.request.mockImplementation((command: SignalCommand) => 'conversation_id' in command && command.conversation_id === 'alice'
    ? new Promise(resolve => finish = resolve) : Promise.resolve(reply(review('unverified', 'bob'))));
  render(); await page.getByRole('button', { name: 'Safety numbers', exact: true }).click();
  await expect.poll(() => typeof finish).toBe('function');
  await unmount(pane!); pane = null; document.body.replaceChildren();
  render('bob'); await page.getByRole('button', { name: 'Safety numbers', exact: true }).click();
  await expect.element(page.getByRole('checkbox', { name: 'I compared this number with bob.' })).toBeVisible();
  finish(reply(review()));
  await expect.element(page.getByRole('checkbox', { name: 'I compared this number with bob.' })).toBeVisible();
  expect(document.body.textContent).not.toContain('alice');
  expect(ipc.request.mock.calls.some(([command]) => command.kind === 'verify_identity')).toBe(false);
});

it('chooses a current group member before requesting or verifying a safety number', async () => {
  const members = [{ id: 'alice', title: 'Alice' }, { id: 'bob', title: 'Bob' }];
  ipc.request.mockImplementation(async (command: SignalCommand) => ({
    kind: 'identity', conversation_id: 'group', members,
    review: command.kind === 'identity' && command.recipient_id ? review('unverified', command.recipient_id) : null,
  }));
  render('group'); await page.getByRole('button', { name: 'Safety numbers', exact: true }).click();
  await page.getByRole('combobox', { name: 'Whose safety number' }).selectOptions('bob');
  await expect.element(page.getByRole('checkbox', { name: 'I compared this number with Bob.' })).toBeVisible();
  expect(ipc.request).toHaveBeenLastCalledWith({ kind: 'identity', conversation_id: 'group', recipient_id: 'bob', refresh: false });
  await page.getByRole('checkbox').click();
  await page.getByRole('combobox', { name: 'Whose safety number' }).selectOptions('alice');
  await expect.element(page.getByRole('checkbox', { name: 'I compared this number with Alice.' })).not.toBeChecked();
  await expect.element(page.getByRole('button', { name: 'Mark verified' })).toBeDisabled();
});

it('clears a comparison after refresh fails or returns another person’s number', async () => {
  ipc.request.mockResolvedValue(reply(review()));
  render(); await page.getByRole('button', { name: 'Safety numbers', exact: true }).click();
  await page.getByRole('checkbox').click();
  ipc.request.mockResolvedValue(reply(review('unverified', 'bob'), 'alice'));
  await page.getByRole('button', { name: 'Refresh safety number' }).click();
  await expect.element(page.getByRole('alert')).toHaveTextContent('Signal returned an unrelated safety number.');
  expect(document.querySelector('input[type="checkbox"]')).toBeNull();
  expect(ipc.request.mock.calls.some(([command]) => command.kind === 'verify_identity')).toBe(false);
});
