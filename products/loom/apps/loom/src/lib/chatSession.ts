import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { get, writable } from 'svelte/store';

type Result<T> = { result?: T; blocker?: { code: string; message: string } };
export type Source = { conversation_id: string; message_id: string; representation: { attachment_id: string; root_sha256: string; manifest_sha256: string; artifact_ids: string[] } };
export type Message = { id: string; role: string; content: string; attribution?: { invocation_id: string; source_id: string; label: string } };
export type Conversation = { id: string; title: string; kind: string; messages: Message[] };
export type Candidate = { id: string; handle: string; label: string; kind: string };
type Draft = { message: string; attachment_ids: string[] };
type TargetResult = { target_id: string; cache_id?: string; cache_reused: boolean; real_engine_invoked: boolean; fake_fixture: boolean };
type Dispatch = { kind: 'mention'; invocation: { id: string; results: TargetResult[] } } | { kind: 'direct' };
type Stream = { kind: 'mention' | 'chat'; event: { invocation_id?: string; request_id?: string; target_id?: string; label?: string; delta?: string; event: string; fake_fixture: boolean } };
export type Pending = { request: string; conversation: string; invocation?: string; stop: boolean; text: Record<string, string> };
export const chatSession = writable({ ready: false, loading: false, error: '', conversations: [] as Conversation[], selected: null as Conversation | null, candidates: [] as Candidate[], draft: '', attachments: [] as string[], pending: null as Pending | null, receipts: {} as Record<string, TargetResult[]> });
let initialization: Promise<void> | null = null;
let selection = 0;
let draftLane: Promise<unknown> = Promise.resolve();

export async function nativeChat<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  const response = await invoke<Result<T>>(`mom_llama_${command}`, args);
  if (response.blocker) throw new Error(response.blocker.message);
  if (response.result === undefined) throw new Error('The local command returned no result.');
  return response.result;
}
function failure(error: unknown): void { chatSession.update(state => ({ ...state, error: String(error) })); }
export function initializeChat(): Promise<void> {
  if (initialization) return initialization;
  initialization = (async () => {
    chatSession.update(state => ({ ...state, loading: true, error: '' }));
    try {
      await invoke('mom_llama_runtime_initialize');
      const conversations = await nativeChat<Conversation[]>('conversation_list');
      chatSession.update(state => ({ ...state, ready: true, conversations: conversations.filter(item => item.kind === 'chat') }));
      const first = conversations.find(item => item.kind === 'chat');
      if (first && !get(chatSession).selected) await selectChat(first.id);
    } catch (error) { failure(error); initialization = null; }
    finally { chatSession.update(state => ({ ...state, loading: false })); }
  })();
  return initialization;
}
export async function selectChat(id: string): Promise<void> {
  const serial = ++selection;
  try {
    await draftLane;
    const conversation = await nativeChat<Conversation>('conversation_select', { conversation: id });
    const draft = await nativeChat<Draft>('draft_get', { conversation: id });
    const candidates = await nativeChat<Candidate[]>('mention_candidates', { query: '', conversation: id });
    if (serial !== selection) return;
    chatSession.update(state => ({ ...state, selected: conversation, draft: draft.message, attachments: draft.attachment_ids, candidates, error: '' }));
  } catch (error) { if (serial === selection) failure(error); }
}
export async function newChat(): Promise<void> {
  try {
    const conversation = await nativeChat<Conversation>('conversation_new', { title: null });
    chatSession.update(state => ({ ...state, conversations: [conversation, ...state.conversations] }));
    await selectChat(conversation.id);
  } catch (error) { failure(error); }
}
export function updateChatDraft(message: string, attachmentIds = get(chatSession).attachments): void {
  const state = get(chatSession);
  if (!state.selected) return;
  const conversation = state.selected.id;
  const attachments = [...attachmentIds];
  chatSession.update(value => ({ ...value, draft: message, attachments }));
  // Capture the exact owner now; ordered encrypted writes cannot migrate to the
  // conversation selected later. No renderer disk storage contains chat prose.
  draftLane = draftLane.catch(() => undefined).then(() => nativeChat<Draft>('draft_update', { conversation, message, attachmentIds: attachments }));
  void draftLane.catch(failure);
}
export async function flushChatDraft(): Promise<boolean> {
  try { await draftLane; return true; }
  catch (error) { failure(error); return false; }
}
export async function attachChatFile(): Promise<void> {
  const state = get(chatSession);
  if (!state.selected) return;
  const conversation = state.selected.id;
  try {
    const picked = await nativeChat<{ path?: string }>('pick_file', { kind: 'attachment' });
    if (!picked.path) return;
    const record = await nativeChat<{ attachment: { id: string } }>('attachment_import', { conversation, path: picked.path });
    if (get(chatSession).selected?.id === conversation) updateChatDraft(get(chatSession).draft, [...get(chatSession).attachments, record.attachment.id]);
  } catch (error) { failure(error); }
}
export async function pasteChatSource(text: string): Promise<void> {
  const state = get(chatSession);
  if (!state.selected || !text.trim()) return;
  const conversation = state.selected.id;
  try {
    const record = await nativeChat<{ attachment: { id: string } }>('attachment_import_paste', { conversation, text });
    if (get(chatSession).selected?.id === conversation) updateChatDraft(get(chatSession).draft, [...get(chatSession).attachments, record.attachment.id]);
  } catch (error) { failure(error); }
}
async function cancelPending(pending: Pending): Promise<void> {
  if (pending.invocation) await nativeChat('mention_cancel', { invocation: pending.invocation, target: null });
  else await nativeChat('chat_cancel', { conversation: pending.conversation });
}
export async function stopChat(): Promise<void> {
  const pending = get(chatSession).pending;
  if (!pending) return;
  chatSession.update(state => ({ ...state, pending: state.pending ? { ...state.pending, stop: true } : null }));
  try { await cancelPending(pending); } catch (error) { failure(error); }
}
export async function sendChat(): Promise<void> {
  const state = get(chatSession);
  if (!state.selected || state.pending || !state.draft.trim()) return;
  const conversation = state.selected.id;
  const message = state.draft;
  const request = crypto.randomUUID();
  let unlisten: UnlistenFn | undefined;
  chatSession.update(value => ({ ...value, pending: { request, conversation, stop: false, text: {} }, error: '' }));
  try {
    await draftLane;
    unlisten = await listen<{ client_request: string; conversation_id: string; event: Stream }>('loom_mom_chat_dispatch_stream', ({ payload }) => {
      const pending = get(chatSession).pending;
      if (!pending || pending.request !== payload.client_request || pending.conversation !== payload.conversation_id || payload.event.event.fake_fixture) return;
      const event = payload.event.event;
      if (event.invocation_id && pending.invocation && event.invocation_id !== pending.invocation) return;
      const firstInvocation = event.invocation_id && !pending.invocation;
      const key = event.label ?? event.target_id ?? 'Reply';
      const updated = { ...pending, invocation: event.invocation_id ?? pending.invocation, text: { ...pending.text, [key]: (pending.text[key] ?? '') + (event.delta ?? '') } };
      chatSession.update(value => ({ ...value, pending: updated }));
      if (firstInvocation && updated.stop) void cancelPending(updated).catch(failure);
    });
    if (get(chatSession).pending?.stop) return;
    const result = await nativeChat<Dispatch>('chat_dispatch', { conversation, message, clientRequest: request });
    if (result.kind === 'mention') chatSession.update(value => ({ ...value, receipts: { ...value.receipts, [result.invocation.id]: result.invocation.results } }));
    // Selection may have changed during inference. The native result belongs to
    // its captured conversation and must never replace another visible chat.
    if (get(chatSession).selected?.id === conversation) await selectChat(conversation);
  } catch (error) { failure(error); }
  finally {
    unlisten?.();
    chatSession.update(value => ({ ...value, pending: value.pending?.request === request ? null : value.pending }));
  }
}
export async function listConsultSources(invocation: string, target: string): Promise<Source[]> {
  return nativeChat('consult_sources', { invocation, target });
}
export async function openConsultSource(invocation: string, target: string, source: Source): Promise<{ source: Source; canonical_text: string }> {
  return nativeChat('consult_source_open', { invocation, target, conversation: source.conversation_id, message: source.message_id, attachment: source.representation.attachment_id });
}
