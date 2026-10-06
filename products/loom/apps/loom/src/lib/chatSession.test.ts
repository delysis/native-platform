import { beforeEach, expect, test, vi } from 'vitest';
import { get } from 'svelte/store';
const transport = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn(), listener: null as null | ((event: { payload: unknown }) => void), unlisten: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: transport.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: transport.listen }));

beforeEach(() => {
  vi.resetModules(); transport.invoke.mockReset(); transport.listen.mockReset(); transport.unlisten.mockReset(); transport.listener = null;
  transport.listen.mockImplementation(async (_name, callback) => { transport.listener = callback; return transport.unlisten; });
});
const chat = (id: string) => ({ id, title: id, kind: 'chat', messages: [] });
async function session() {
  const module = await import('./chatSession');
  module.chatSession.update(state => ({ ...state, ready: true, selected: chat('original'), draft: '@mom hello' }));
  return module;
}
test('late stream and final reply never replace a different selected chat', async () => {
  const module = await session();
  let finish!: (value: unknown) => void;
  transport.invoke.mockImplementation((command) => command.endsWith('chat_dispatch') ? new Promise(resolve => finish = resolve) : Promise.resolve({ result: [] }));
  const send = module.sendChat();
  await vi.waitFor(() => expect(finish).toBeTypeOf('function'));
  const pending = get(module.chatSession).pending!;
  module.chatSession.update(state => ({ ...state, selected: chat('other'), draft: 'other draft' }));
  transport.listener!({ payload: { client_request: 'wrong', conversation_id: 'original', event: { kind: 'mention', event: { invocation_id: 'wrong', label: 'Mom', delta: 'wrong', fake_fixture: false } } } });
  expect(get(module.chatSession).pending?.text).toEqual({});
  transport.listener!({ payload: { client_request: pending.request, conversation_id: 'original', event: { kind: 'mention', event: { invocation_id: 'real', label: 'Mom', delta: 'real text', fake_fixture: false } } } });
  expect(get(module.chatSession).pending?.text).toEqual({ Mom: 'real text' });
  finish({ result: { kind: 'mention', invocation: { id: 'real', results: [] } } });
  await send;
  expect(get(module.chatSession).selected?.id).toBe('other');
  expect(get(module.chatSession).draft).toBe('other draft');
  expect(transport.invoke.mock.calls.some(([command]) => command.endsWith('conversation_select'))).toBe(false);
  expect(transport.unlisten).toHaveBeenCalledOnce();
});
test('Stop before the native invocation arrives cancels that exact invocation when it arrives', async () => {
  const module = await session();
  let finish!: (value: unknown) => void;
  transport.invoke.mockImplementation((command) => command.endsWith('chat_dispatch') ? new Promise(resolve => finish = resolve) : Promise.resolve({ result: {} }));
  const send = module.sendChat();
  await vi.waitFor(() => expect(finish).toBeTypeOf('function'));
  await module.stopChat();
  const pending = get(module.chatSession).pending!;
  transport.listener!({ payload: { client_request: pending.request, conversation_id: pending.conversation, event: { kind: 'mention', event: { invocation_id: 'owned-invocation', label: 'Mom', event: 'started', fake_fixture: false } } } });
  await vi.waitFor(() => expect(transport.invoke).toHaveBeenCalledWith('mom_llama_mention_cancel', { invocation: 'owned-invocation', target: null }));
  module.chatSession.update(state => ({ ...state, selected: chat('other') }));
  finish({ result: { kind: 'direct' } }); await send;
});
test('a failed encrypted draft write blocks dispatch and preserves the draft', async () => {
  const module = await session();
  transport.invoke.mockRejectedValue(new Error('encrypted write failed'));
  module.updateChatDraft('@mom retained');
  await module.sendChat();
  expect(transport.invoke.mock.calls.some(([command]) => command.endsWith('chat_dispatch'))).toBe(false);
  expect(get(module.chatSession).draft).toBe('@mom retained');
  expect(get(module.chatSession).pending).toBeNull();
});
