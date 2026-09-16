import { mount, unmount, tick, type ComponentProps } from 'svelte';
import { SvelteMap } from 'svelte/reactivity';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import SignalPane from './SignalPane.svelte';
import { SignalDraftEditor } from './signalDraft';
import type { SignalCommand, SignalDraft, SignalEvent, SignalWorkspaceLinks } from './signal';
import '../app.css';

const ipc = vi.hoisted(() => ({ request: vi.fn(), listen: vi.fn() }));
vi.mock('./signal', async original => ({ ...await original<typeof import('./signal')>(), listenSignal: ipc.listen }));
vi.mock('@tauri-apps/api/core', async original => ({ ...await original<typeof import('@tauri-apps/api/core')>(), invoke: (_name: string, args: { request: { command: SignalCommand; id: string } }) => ipc.request(args.request.command, args.request.id) }));

let pane: ReturnType<typeof mount> | null = null;
const drafts = new Map<string, SignalDraft>();
const links = new Map<string, SignalWorkspaceLinks>();
const workspaceId = '5c5b144e-1619-476a-bf58-e624d1f402cc';
let description = '';
beforeEach(() => {
  drafts.clear(); links.clear(); description = ''; ipc.request.mockReset(); ipc.listen.mockResolvedValue(() => {});
  ipc.request.mockImplementation(async (command: SignalCommand): Promise<SignalEvent> => {
    switch (command.kind) {
      case 'status': return { kind: 'status', status: { version: 4, phase: 'connected', account_id: 'me', device_name: 'Loom' } };
      case 'conversations': return { kind: 'conversations', conversations: ['alice', 'bob'].map(id => ({ id, title: id, description, disappearing: false, is_group: false })) };
      case 'messages': return { kind: 'messages', conversation_id: command.conversation_id, messages: [] };
      case 'workspaces': return { kind: 'workspaces', conversation_id: command.conversation_id, links: links.get(command.conversation_id) ?? { version: 0, workspaces: [] } };
      case 'update_workspace': {
        const next = { version: command.expected_version + 1, workspaces: (links.get(command.conversation_id)?.workspaces ?? []).filter(item => item.id !== command.workspace_id) };
        if (command.title !== null) next.workspaces.push({ id: command.workspace_id, title: command.title });
        links.set(command.conversation_id, next);
        return { kind: 'workspaces', conversation_id: command.conversation_id, links: next };
      }
      case 'draft': return { kind: 'draft', conversation_id: command.conversation_id, draft: drafts.get(command.conversation_id) ?? { version: 0, text: '', pending: null } };
      case 'save_draft': {
        const draft = { version: command.expected_version + 1, text: command.text, pending: command.pending };
        drafts.set(command.conversation_id, draft);
        return { kind: 'draft', conversation_id: command.conversation_id, draft };
      }
      default: throw new Error(`Unexpected request: ${command.kind}`);
    }
  });
});
afterEach(async () => { if (pane) await unmount(pane); pane = null; document.body.replaceChildren(); });

function render(editor: SignalDraftEditor, options: Partial<ComponentProps<typeof SignalPane>> = {}) {
  const target = document.createElement('div'); document.body.append(target);
  const scope = new SvelteMap([['workspace', 'first']]);
  const props = { editor, onClose: vi.fn(), onDraft: vi.fn(), onCabal: vi.fn(), onJoin: vi.fn(), onWorkspace: vi.fn(), ...options, get workspaceScope() { return scope.get('workspace')!; } };
  pane = mount(SignalPane, { target, props });
  return { ...props, scope };
}

describe('Signal pane ownership', () => {
  it('does not put a hidden pane’s delayed invitation in another friend’s draft', async () => {
    const editor = new SignalDraftEditor(ipc.request); await editor.open('alice');
    let finish!: (text: string) => void;
    const invitation = new Promise<string>(resolve => finish = resolve);
    const onCabal = vi.fn(() => invitation);
    render(editor, { onCabal });
    await page.getByRole('button', { name: 'Invite to cabal' }).click();
    expect(onCabal).toHaveBeenCalledOnce();
    await unmount(pane!); pane = null; document.body.replaceChildren();
    await editor.open('bob'); editor.text = 'Only for Bob'; await editor.flush(); render(editor);
    finish('Private Alice invitation');
    await invitation; await editor.flush();
    await expect.element(page.getByRole('textbox', { name: 'Signal message draft' })).toHaveValue('Only for Bob');
    expect(editor.text).toBe('Only for Bob');
    expect(drafts.get('bob')?.text).toBe('Only for Bob');
    expect(ipc.request.mock.calls.some(([command]) => command.kind === 'send')).toBe(false);
  });

  it('discards an AI proposal when its workspace changes while the model is running', async () => {
    const editor = new SignalDraftEditor(ipc.request); await editor.open('alice');
    let finish!: (text: string) => void;
    const reply = new Promise<string>(resolve => finish = resolve);
    const { scope } = render(editor, { onDraft: () => reply });
    await page.getByRole('button', { name: 'Draft locally' }).click();
    scope.set('workspace', 'second'); await tick();
    finish('A proposal from the previous workspace'); await reply; await tick();
    expect(document.querySelector('.proposal')).toBeNull();
    expect(editor.text).toBe('');
    expect(ipc.request.mock.calls.some(([command]) => command.kind === 'send')).toBe(false);
  });

  it('clears an already visible proposal when moving to another workspace', async () => {
    const editor = new SignalDraftEditor(ipc.request); await editor.open('alice');
    const { scope } = render(editor, { onDraft: async () => 'A proposal from the first workspace' });
    await page.getByRole('button', { name: 'Draft locally' }).click();
    await expect.element(page.getByRole('button', { name: 'Use draft' })).toBeVisible();
    expect(editor.text).toBe('');
    scope.set('workspace', 'second'); await tick();
    expect(document.querySelector('.proposal')).toBeNull();
    expect(editor.text).toBe('');
  });

  it('opens saved and description workspaces only on a click and forgets only the selected bookmark', async () => {
    const editor = new SignalDraftEditor(ipc.request); await editor.open('alice');
    links.set('alice', { version: 4, workspaces: [{ id: workspaceId, title: 'The garden' }] });
    links.set('bob', { version: 1, workspaces: [{ id: workspaceId, title: 'Bob’s garden' }] });
    const other = '01165b79-0d61-4fa3-b0c2-cf4c431fc184';
    description = `Our shared writing: loom://workspace/${other}`;
    const { onWorkspace, onJoin } = render(editor);
    await expect.element(page.getByRole('button', { name: 'The garden', exact: true })).toBeVisible();
    expect(onWorkspace).not.toHaveBeenCalled(); expect(onJoin).not.toHaveBeenCalled();
    await page.getByRole('button', { name: 'The garden', exact: true }).click();
    expect(onWorkspace).toHaveBeenCalledWith(expect.objectContaining({ id: 'alice' }), workspaceId);
    await page.getByRole('button', { name: 'Open workspace', exact: true }).click();
    expect(onWorkspace).toHaveBeenLastCalledWith(expect.objectContaining({ id: 'alice' }), other);
    await page.getByRole('button', { name: 'Forget workspace The garden' }).click();
    await expect.poll(() => links.get('alice')?.workspaces).toEqual([]);
    expect(ipc.request.mock.calls.find(([command]) => command.kind === 'update_workspace')?.[0]).toMatchObject({ conversation_id: 'alice', expected_version: 4, workspace_id: workspaceId, title: null });
    expect(links.get('bob')?.workspaces).toHaveLength(1);
    expect(onJoin).not.toHaveBeenCalled();
    expect(ipc.request.mock.calls.some(([command]) => command.kind === 'send')).toBe(false);
  });

  it('reattaches after a hidden pane’s conversation load without showing the previous friend’s text', async () => {
    const editor = new SignalDraftEditor(ipc.request); await editor.open('alice');
    editor.text = 'Alice only'; await editor.flush();
    drafts.set('bob', { version: 2, text: 'Bob only', pending: null });
    render(editor);
    await expect.element(page.getByRole('textbox', { name: 'Signal message draft' })).toHaveValue('Alice only');
    let finish!: (event: SignalEvent) => void;
    const original = ipc.request.getMockImplementation()!;
    ipc.request.mockImplementation((command: SignalCommand, id: string) => command.kind === 'draft' && command.conversation_id === 'bob'
      ? new Promise(resolve => finish = resolve) : original(command, id));
    await page.getByRole('combobox', { name: 'Signal conversation' }).selectOptions('bob');
    await expect.poll(() => typeof finish).toBe('function');
    await unmount(pane!); pane = null; document.body.replaceChildren(); render(editor);
    await expect.element(page.getByRole('textbox', { name: 'Signal message draft' })).toBeDisabled();
    finish({ kind: 'draft', conversation_id: 'bob', draft: drafts.get('bob')! });
    await expect.element(page.getByRole('textbox', { name: 'Signal message draft' })).toHaveValue('Bob only');
    await expect.element(page.getByRole('combobox', { name: 'Signal conversation' })).toHaveValue('bob');
  });
});
