import { mount, unmount, tick } from 'svelte';
import { get } from 'svelte/store';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import WorkspacePane, { type WorkspacePaneConfig } from './WorkspacePane.svelte';
import type { OpenDocument, TerminalRun } from './types';
import { WorkspacePaneDrafts, type WorkspacePaneDraftStore } from './workspacePaneDrafts';
import '../app.css';

vi.mock('@tauri-apps/api/core', async (original) => ({ ...await original<typeof import('@tauri-apps/api/core')>(), convertFileSrc: (path: string, protocol: string) => `${protocol}://localhost/${path}` }));
const ipc = vi.hoisted(() => ({ list: vi.fn(), run: vi.fn(), cancel: vi.fn(), open: vi.fn(), resolve: vi.fn(), import: vi.fn(), bind: vi.fn() }));
vi.mock('./ipc', async (original) => ({
  ...await original<typeof import('./ipc')>(),
  listWorkspacePaneRuns: ipc.list, runWorkspacePane: ipc.run, cancelWorkspacePaneRun: ipc.cancel, readWorkspacePaneOutput: ipc.open, resolveWorkspaceDocument: ipc.resolve,
  importAttachmentPaths: ipc.import, bindAttachmentMaterial: ipc.bind,
  normalizeFailure: (error: unknown) => ({ message: error instanceof Error ? error.message : String(error) })
}));
let mounted: ReturnType<typeof mount> | undefined;
afterEach(async () => {
  if (mounted) await unmount(mounted);
  mounted = undefined; document.body.replaceChildren(); vi.resetAllMocks();
});
const fullAnswer = 'Earlier answer ' + 'x'.repeat(2200) + ' full ending';
const source: OpenDocument = {
  summary: { document_id: 'draft', relative_path: 'Draft.md', title: 'Draft', kind: 'prose', revision_id: 'revision', active_blob_id: 'blob', word_count: 1, externally_modified: false },
  visible_blob_id: 'blob', text: 'Writing', transient_draft: null
};
function run(id: string, paneId = 'conversation'): TerminalRun {
  return { run_id: id, status: 'completed', expression: 'prompt', presentation: { pane_id: paneId, input: 'Earlier message' },
    output_document_id: 'result', output_relative_path: 'Runs/1/Answer.md', preview: 'Earlier answer', error: null, created_at_ms: 1 };
}
async function render(config: Partial<WorkspacePaneConfig> = {}, value = source.text, retained: { draft?: WorkspacePaneDraftStore; projectId?: string; sessionId?: string; workspaceScope?: { projectId: string; sessionId: string }; ownerActive?: boolean; source?: OpenDocument | null } = {}) {
  const initialListCalls = ipc.list.mock.calls.length;
  const target = document.createElement('div'); target.style.height = '400px'; target.style.width = '320px'; document.body.append(target);
  ipc.list.mockResolvedValue(config.kind === 'browser' ? [] : [run('old'), run('unrelated', 'other')]);
  if (!ipc.open.getMockImplementation()) ipc.open.mockResolvedValue({ ...source, text: fullAnswer });
  if (!ipc.resolve.getMockImplementation()) ipc.resolve.mockImplementation(async (_project, _session, reference) => reference === 'Answer' ? { ...source, summary: { ...source.summary, document_id: 'result', title: 'Answer' } } : source);
  const beforeRun = vi.fn(async () => ({ document: retained.source === undefined ? source : retained.source })), onOpenDocument = vi.fn(), onChange = vi.fn(), onCompositionChange = vi.fn(), onPinOutput = vi.fn();
  mounted = mount(WorkspacePane, { target, props: {
    paneId: 'conversation', config: { kind: 'chat', position: 'right', visible: true, title: null, document: null, context: ['Voice notes'], ...config },
    workspaceScope: { projectId: 'owner', sessionId: 'workspace-session' }, configurationRevisionId: 'template-revision',
    projectId: 'project', sessionId: 'session', source, value, beforeRun, onOpenDocument, onChange, onCompositionChange, onPinOutput, ...retained
  } });
  await tick(); await expect.poll(() => ipc.list.mock.calls.length).toBe(initialListCalls + 1);
  return { beforeRun, onOpenDocument, onChange, onCompositionChange, onPinOutput };
}

describe('workspace panes', () => {
  it('drops existing workspace writing as a reference without creating an attachment or starting inference', async () => {
    const { onChange } = await render();
    const input = page.getByRole('textbox', { name: 'Message' });
    await input.fill('Consider this');
    ipc.import.mockResolvedValue({ imported: [], references: ['@"Inside Notes.md"'], failures: [], next_page_token: null });
    await (mounted as unknown as WorkspacePane).importDroppedPaths(['/workspace/Inside Notes.md'], { x: 0, y: 0 });
    await expect.element(input).toHaveValue('Consider this\n\n@"Inside Notes.md"');
    expect(ipc.import).toHaveBeenCalledWith('project', 'session', ['/workspace/Inside Notes.md']);
    expect(ipc.bind).not.toHaveBeenCalled();
    expect(ipc.run).not.toHaveBeenCalled();
    expect(onChange).not.toHaveBeenCalled();
  });

  it('keeps an edited composer intact when a document reference arrives after the edit', async () => {
    await render();
    let complete!: (report: unknown) => void;
    ipc.import.mockImplementation(() => new Promise(resolve => { complete = resolve; }));
    const pending = (mounted as unknown as WorkspacePane).importDroppedPaths(['/workspace/Inside Notes.md'], { x: 0, y: 0 });
    await expect.poll(() => ipc.import.mock.calls.length).toBe(1);
    const input = page.getByRole('textbox', { name: 'Message' });
    await input.fill('New thought');
    complete({ imported: [], references: ['@"Inside Notes.md"'], failures: [], next_page_token: null });
    await pending;
    await expect.element(input).toHaveValue('New thought');
    expect(ipc.bind).not.toHaveBeenCalled();
    expect(ipc.run).not.toHaveBeenCalled();
  });

  it('restores only this pane history and submits one retained raw prompt with explicit context and full prior output', async () => {
    const { beforeRun, onOpenDocument } = await render({ context: ['@document', '@"Voice notes"'], document: '@Draft' });
    await expect.poll(() => document.querySelector('.output')?.textContent).toBe(fullAnswer);
    await page.getByText(fullAnswer, { exact: true }).click();
    const range = document.createRange(); range.selectNodeContents(document.querySelector('.output p')!);
    const selection = window.getSelection(); selection?.removeAllRanges(); selection?.addRange(range);
    expect(selection?.toString()).toBe(fullAnswer);
    expect(onOpenDocument).not.toHaveBeenCalled();
    expect(document.querySelectorAll('article')).toHaveLength(1);
    await page.getByRole('button', { name: 'Open output document' }).click();
    expect(onOpenDocument).toHaveBeenCalledWith('result');
    ipc.run.mockImplementation(async (request) => ({ status: 'accepted', run: { ...run(request.command_id), presentation: { pane_id: request.pane_id, input: request.input } } }));
    const input = page.getByRole('textbox', { name: 'Message' });
    const inputRect = input.element().getBoundingClientRect();
    expect(document.querySelector('.composer-actions')).toBeNull();
    expect(document.querySelector('form button')).toBeNull();
    const formRect = document.querySelector('form')!.getBoundingClientRect();
    expect(inputRect.width).toBeGreaterThan(formRect.width - 12);
    expect(formRect.height).toBeLessThanOrEqual(48);
    await input.fill('Continue that idea');
    await userEvent.keyboard('{Enter}');
    await expect.poll(() => ipc.run.mock.calls.length).toBe(1);
    expect(beforeRun).toHaveBeenCalledOnce();
    const request = ipc.run.mock.calls[0][0];
    expect(request.workspace_id).toBe('owner');
    expect(request.workspace_session_id).toBe('workspace-session');
    expect(request.configuration_revision_id).toBe('template-revision');
    expect(request.pane_id).toBe('conversation');
    expect(request.input).toBe('Continue that idea');
    expect(request).not.toHaveProperty('contextReferences');
    expect(request).not.toHaveProperty('turnBoundary');
    expect(request.expression).toContain('Assistant: ' + fullAnswer);
    expect(request.expression).not.toContain('@document');
    expect(request.expression).toContain('User: Continue that idea\nAssistant:');
    expect(request.captured_document).toEqual({ project_id: 'project', session_id: 'session', document_id: 'draft', revision_id: 'revision', visible_blob_id: 'blob' });
  });

  it('keeps an explicit @document reference literal and sends its independently captured source identity', async () => {
    await render();
    ipc.run.mockImplementation(async request => ({ status: 'accepted', run: { ...run(request.command_id), presentation: { pane_id: request.pane_id, input: request.input } } }));
    await page.getByRole('textbox', { name: 'Message' }).fill('Consider @document');
    await userEvent.keyboard('{Enter}');
    await expect.poll(() => ipc.run.mock.calls.length).toBe(1);
    const request = ipc.run.mock.calls[0][0];
    expect(request.expression).toContain('Consider @document');
    expect(request.captured_document.project_id).toBe('project');
    expect(request.workspace_id).toBe('owner');
  });

  it('can admit an owner-only pane run without inventing a current document', async () => {
    await render({}, '', { source: null });
    ipc.run.mockImplementation(async request => ({ status: 'accepted', run: { ...run(request.command_id), presentation: { pane_id: request.pane_id, input: request.input } } }));
    await page.getByRole('textbox', { name: 'Message' }).fill('Start here');
    await userEvent.keyboard('{Enter}');
    await expect.poll(() => ipc.run.mock.calls.length).toBe(1);
    expect(ipc.run.mock.calls[0][0]).not.toHaveProperty('captured_document');
    expect(ipc.run.mock.calls[0][0].workspace_id).toBe('owner');
  });

  it('keeps hidden reasoning and Markdown bytes intact in the next raw prompt', async () => {
    const raw = '<think>A private model trace.</think>**The moon.**';
    ipc.open.mockResolvedValue({ ...source, text: raw });
    await render();
    await expect.poll(() => document.querySelector('.output strong')?.textContent).toBe('The moon.');
    expect(document.querySelector('details')?.open).toBe(false);
    ipc.run.mockImplementation(async request => ({ status: 'accepted', run: { ...run(request.command_id), presentation: { pane_id: request.pane_id, input: request.input } } }));
    await page.getByRole('textbox', { name: 'Message' }).fill('Continue');
    await userEvent.keyboard('{Enter}');
    await expect.poll(() => ipc.run.mock.calls.length).toBe(1);
    expect(ipc.run.mock.calls[0][0].expression).toContain('Assistant: ' + raw);
  });

  it('pins a retained output explicitly without invoking a model or editing its originating document', async () => {
    const { onPinOutput, onChange, onOpenDocument } = await render();
    await page.getByRole('button', { name: 'Pin output in workspace' }).click();
    expect(onPinOutput).toHaveBeenCalledWith('result');
    expect(onChange).not.toHaveBeenCalled();
    expect(onOpenDocument).not.toHaveBeenCalled();
    expect(ipc.run).not.toHaveBeenCalled();
  });

  it('inserts a source into the captured chat selection and refuses a changed input', async () => {
    await render();
    const input = page.getByRole('textbox', { name: 'Message' });
    await input.fill('Before after');
    (input.element() as HTMLTextAreaElement).setSelectionRange(7, 7);
    const pane = mounted as unknown as WorkspacePane;
    const insert = pane.captureReferenceInsertion();
    expect(insert).not.toBeNull();
    expect(insert!('[@Source](loom-material:material-' + 'a'.repeat(64) + ') ')).toBe(true);
    await expect.element(input).toHaveValue('Before [@Source](loom-material:material-' + 'a'.repeat(64) + ') after');
    const stale = pane.captureReferenceInsertion();
    await input.fill('A changed message');
    expect(stale!('unexpected')).toBe(false);
    await expect.element(input).toHaveValue('A changed message');
    expect(ipc.run).not.toHaveBeenCalled();
  });

  it('keeps a lost admission uncertain and does not issue another generation', async () => {
    await render({ kind: 'terminal' });
    ipc.run.mockRejectedValue(new Error('Reply interrupted'));
    await page.getByRole('textbox', { name: 'Command' }).fill('An experiment');
    await userEvent.keyboard('{Enter}');
    await expect.element(page.getByRole('button', { name: 'Check result' })).toBeVisible();
    await page.getByRole('button', { name: 'Check result' }).click();
    expect(ipc.run).toHaveBeenCalledOnce();
    expect(document.querySelector('button[type=submit]')).toBeNull();
  });

  it('retains a workspace draft and its unresolved request across a root-transition remount', async () => {
    const drafts = new WorkspacePaneDrafts();
    const retained = drafts.forPane('workspace-session', 'conversation');
    await render({}, source.text, { draft: retained });
    await page.getByRole('textbox', { name: 'Message' }).fill('Keep this idea');
    await unmount(mounted!); mounted = undefined; document.body.replaceChildren();
    await render({}, source.text, { draft: drafts.forPane('workspace-session', 'conversation'), projectId: 'child', sessionId: 'child-session' });
    await expect.element(page.getByRole('textbox', { name: 'Message' })).toHaveValue('Keep this idea');
    ipc.run.mockRejectedValue(new Error('Reply interrupted'));
    await page.getByRole('textbox', { name: 'Message' }).click();
    await userEvent.keyboard('{Enter}');
    await expect.element(page.getByRole('button', { name: 'Check result' })).toBeVisible();
    const request = get(retained).pending;
    expect(request?.captured_document?.project_id).toBe('child');
    expect(request?.workspace_session_id).toBe('workspace-session');
    await unmount(mounted!); mounted = undefined; document.body.replaceChildren();
    await render({}, source.text, { draft: drafts.forPane('workspace-session', 'conversation'), projectId: 'next-root', sessionId: 'next-session' });
    await expect.element(page.getByRole('textbox', { name: 'Message' })).toHaveValue('Keep this idea');
    expect(get(retained).pending).toBe(request);
    await page.getByRole('button', { name: 'Check result' }).click();
    expect(ipc.list).toHaveBeenLastCalledWith('owner', 'workspace-session');
    await page.getByRole('textbox', { name: 'Message' }).click();
    await userEvent.keyboard('{Enter}');
    expect(ipc.run).toHaveBeenCalledOnce();
    await page.getByRole('button', { name: 'Stop', exact: true }).click();
    expect(ipc.cancel).toHaveBeenCalledWith('owner', 'workspace-session', request!.command_id);
    expect(get(retained).pending).toBe(request);
  });

  it('recovers an admitted owner run after remount without resubmission or child output reads', async () => {
    const drafts = new WorkspacePaneDrafts(), draft = drafts.forPane('workspace-session', 'conversation');
    await render({}, source.text, { draft });
    let complete!: (run: TerminalRun) => void;
    ipc.run.mockImplementation(() => new Promise<TerminalRun>(resolve => { complete = resolve; }).then(run => ({ status: 'accepted', run })));
    await page.getByRole('textbox', { name: 'Message' }).fill('Retain this conversation');
    await userEvent.keyboard('{Enter}');
    await expect.poll(() => ipc.run.mock.calls.length).toBe(1);
    const request = get(draft).pending!;
    await unmount(mounted!); mounted = undefined; document.body.replaceChildren();
    await render({}, source.text, { draft, projectId: 'child-root', sessionId: 'child-session' });
    ipc.list.mockResolvedValue([{ ...run(request.command_id), presentation: { pane_id: request.pane_id, input: request.input } }]);
    await page.getByRole('button', { name: 'Check result' }).click();
    await expect.element(page.getByRole('textbox', { name: 'Message' })).toHaveValue('');
    expect(get(draft).pending).toBeNull();
    expect(ipc.run).toHaveBeenCalledOnce();
    expect(ipc.list).toHaveBeenLastCalledWith('owner', 'workspace-session');
    expect(ipc.open.mock.calls.every(([project, session]) => project === 'owner' && session === 'workspace-session')).toBe(true);
    await page.getByRole('textbox', { name: 'Message' }).fill('A new thought in the child folder');
    complete(run(request.command_id));
    await tick();
    await expect.element(page.getByRole('textbox', { name: 'Message' })).toHaveValue('A new thought in the child folder');
  });

  it('does not confuse an owner editor document with a child copy that has the same document ID', async () => {
    ipc.resolve.mockResolvedValue({ ...source, summary: { ...source.summary, title: 'Owner draft' } });
    const { onOpenDocument, onChange } = await render({ kind: 'editor', document: '@Draft' });
    await page.getByRole('button', { name: 'Open Owner draft' }).click();
    expect(ipc.resolve).toHaveBeenCalledWith('owner', 'workspace-session', 'Draft');
    expect(onOpenDocument).toHaveBeenCalledWith('draft');
    expect(document.querySelector('[contenteditable=true]')).toBeNull();
    expect(onChange).not.toHaveBeenCalled();
  });

  it('does not let a late submission completion clear another workspace draft', async () => {
    const drafts = new WorkspacePaneDrafts();
    const original = drafts.forPane('workspace-one', 'conversation');
    await render({}, source.text, { draft: original });
    let complete!: (run: TerminalRun) => void;
    ipc.run.mockImplementation(() => new Promise<TerminalRun>(resolve => { complete = resolve; }).then(run => ({ status: 'accepted', run })));
    await page.getByRole('textbox', { name: 'Message' }).fill('First workspace');
    await userEvent.keyboard('{Enter}');
    await expect.poll(() => ipc.run.mock.calls.length).toBe(1);
    const request = get(original).pending!;
    await unmount(mounted!); mounted = undefined; document.body.replaceChildren();
    const replacement = drafts.forPane('workspace-two', 'conversation');
    await render({}, source.text, { draft: replacement, workspaceScope: { projectId: 'other-owner', sessionId: 'workspace-two' } });
    await expect.element(page.getByRole('textbox', { name: 'Message' })).toHaveValue('');
    expect(get(replacement).pending).toBeNull();
    await page.getByRole('textbox', { name: 'Message' }).fill('Second workspace');
    complete(run(request.command_id));
    await tick();
    await expect.element(page.getByRole('textbox', { name: 'Message' })).toHaveValue('Second workspace');
    expect(get(replacement).pending).toBeNull();
    expect(get(original).entry).toBe('');
    expect(get(original).pending).toBeNull();
  });

  it('preserves newly edited input when the previous submission succeeds', async () => {
    await render();
    let complete!: (run: TerminalRun) => void;
    ipc.run.mockImplementation(() => new Promise<TerminalRun>(resolve => { complete = resolve; }).then(run => ({ status: 'accepted', run })));
    await page.getByRole('textbox', { name: 'Message' }).fill('Submitted idea');
    await userEvent.keyboard('{Enter}');
    await expect.poll(() => ipc.run.mock.calls.length).toBe(1);
    await page.getByRole('textbox', { name: 'Message' }).fill('Next idea');
    complete(run(ipc.run.mock.calls[0][0].command_id));
    await expect.element(page.getByRole('textbox', { name: 'Message' })).toHaveValue('Next idea');
  });

  it('opens the configured editor document rather than editing unrelated current writing', async () => {
    const { onOpenDocument, onChange } = await render({ kind: 'editor', document: '@Answer' });
    await page.getByRole('button', { name: 'Open Answer' }).click();
    expect(document.querySelector('[contenteditable=true]')).toBeNull();
    expect(onOpenDocument).toHaveBeenCalledWith('result');
    expect(onChange).not.toHaveBeenCalled();
  });

  it('binds the sandboxed preview to exact native document identity and retains page generation', async () => {
    await render({ kind: 'browser', document: 'Draft.md' });
    await expect.poll(() => document.querySelector('iframe')?.src).toContain('loom-preview://localhost/v1-owner-workspace-session-draft-revision-blob');
    const frame = document.querySelector('iframe')!;
    expect(frame.getAttribute('sandbox')).toBe('');
    expect(frame.hasAttribute('srcdoc')).toBe(false);
    expect(frame.referrerPolicy).toBe('no-referrer');
    expect(ipc.resolve).toHaveBeenCalledWith('owner', 'workspace-session', 'Draft.md');
    ipc.run.mockImplementation(async (request) => ({ status: 'accepted', run: { ...run(request.command_id), presentation: { pane_id: request.pane_id, input: request.input } } }));
    await page.getByRole('textbox', { name: 'Page description' }).fill('A quiet reading page');
    await page.getByRole('button', { name: 'Run', exact: true }).click();
    await expect.poll(() => ipc.run.mock.calls.length).toBe(1);
    expect(ipc.run.mock.calls[0][0].turnBoundary).toBeUndefined();
    expect(ipc.run.mock.calls[0][0]).not.toHaveProperty('contextReferences');
    expect(ipc.run.mock.calls[0][0].expression).toContain('Write a complete static HTML page for: A quiet reading page');
    expect(ipc.run.mock.calls[0][0].input).toBe('A quiet reading page');
  });

  it('rejects oversized prompts before admitting a native run', async () => {
    await render({ kind: 'terminal' });
    await page.getByRole('textbox', { name: 'Command' }).fill('x'.repeat(64 * 1024 + 1));
    await userEvent.keyboard('{Enter}');
    await expect.element(page.getByRole('alert')).toHaveTextContent('64 KiB prompt limit');
    expect(ipc.run).not.toHaveBeenCalled();
  });


  it('uses terminal history and key handling without a second global shortcut or chrome', async () => {
    await render({ kind: 'terminal' });
    const input = page.getByRole('textbox', { name: 'Command' });
    await expect.element(input).toHaveFocus();
    expect(document.querySelector('.terminal-header')).toBeNull();
    expect(document.querySelector('.workspace-pane form')).toBeNull();
    const event = new KeyboardEvent('keydown', { key: '`', code: 'Backquote', metaKey: true, bubbles: true, cancelable: true });
    window.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(false);
    await userEvent.keyboard('{ArrowUp}');
    await expect.element(input).toHaveValue('Earlier message');
    await userEvent.keyboard('{ArrowDown}');
    await expect.element(input).toHaveValue('');
  });

  it.each(['Writing', '| a | b |\n| - | - |\n| c | d |'])('refuses to flush composing visual or exact-source writing: %s', async (value) => {
    const { onCompositionChange } = await render({ kind: 'editor' }, value);
    const input = page.getByRole('textbox', { name: 'Pane editor' });
    await input.click();
    input.element().dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true }));
    expect((mounted as unknown as WorkspacePane).flush()).toBe(false);
    expect(onCompositionChange).toHaveBeenLastCalledWith(true);
    input.element().dispatchEvent(new CompositionEvent('compositionend', { bubbles: true }));
    await expect.poll(() => onCompositionChange.mock.calls.at(-1)?.[0]).toBe(false);
  });


  it('keeps idle chat input-only while Shift+Enter adds a line and an active run remains cancellable', async () => {
    await render();
    const input = page.getByRole('textbox', { name: 'Message' });
    await input.fill('Line one');
    await userEvent.keyboard('{Shift>}{Enter}{/Shift}Line two');
    await expect.element(input).toHaveValue('Line one\nLine two');
    expect(ipc.run).not.toHaveBeenCalled();
    expect(document.querySelector('form button')).toBeNull();
    let active: TerminalRun | undefined;
    ipc.run.mockImplementation(async (request) => {
      active = { ...run(request.command_id), status: 'running', presentation: { pane_id: request.pane_id, input: request.input } };
      return { status: 'accepted', run: active };
    });
    ipc.list.mockImplementation(async () => active ? [active] : []);
    await userEvent.keyboard('{Enter}');
    await expect.element(page.getByRole('button', { name: 'Stop', exact: true })).toBeVisible();
    expect(ipc.run.mock.calls[0][0].input).toBe('Line one\nLine two');
    ipc.cancel.mockImplementation(async () => { active = { ...active!, status: 'cancelled' }; });
    await page.getByRole('button', { name: 'Stop', exact: true }).click();
    await expect.poll(() => document.querySelector('form button')).toBeNull();
    expect(ipc.cancel).toHaveBeenCalledWith('owner', 'workspace-session', active!.run_id);
  });

  it('settles a late authoritative rejection in the retained draft after a root remount', async () => {
    const draft = new WorkspacePaneDrafts().forPane('workspace-session', 'conversation');
    await render({}, source.text, { draft });
    let rejectBeforeAdmission!: () => void;
    ipc.run.mockImplementation(() => new Promise(resolve => {
      rejectBeforeAdmission = () => resolve({ status: 'rejected', error: { code: 'configuration_changed', message: 'Configuration changed.', retryable: true } });
    }));
    await page.getByRole('textbox', { name: 'Message' }).fill('Keep this unsubmitted idea');
    await userEvent.keyboard('{Enter}');
    await expect.poll(() => ipc.run.mock.calls.length).toBe(1);
    await unmount(mounted!); mounted = undefined; document.body.replaceChildren();
    await render({}, source.text, { draft, projectId: 'child-root', sessionId: 'child-session' });
    rejectBeforeAdmission();
    await expect.poll(() => get(draft).pending).toBeNull();
    await expect.element(page.getByRole('textbox', { name: 'Message' })).toHaveValue('Keep this unsubmitted idea');
    await expect.poll(() => document.querySelector('form button')).toBeNull();
    await expect.element(page.getByRole('alert')).toHaveTextContent('Configuration changed.');
    expect(ipc.run).toHaveBeenCalledOnce();
  });

  it('keeps rejected input editable and permits a deliberate resubmission only after authoritative rejection', async () => {
    const draft = new WorkspacePaneDrafts().forPane('workspace-session', 'conversation');
    await render({}, source.text, { draft });
    ipc.run.mockResolvedValue({ status: 'rejected', error: { code: 'configuration_changed', message: 'The pane configuration changed.', retryable: true } });
    await page.getByRole('textbox', { name: 'Message' }).fill('Keep my words');
    await userEvent.keyboard('{Enter}');
    await expect.element(page.getByRole('alert')).toHaveTextContent('The pane configuration changed.');
    await expect.element(page.getByRole('textbox', { name: 'Message' })).toHaveValue('Keep my words');
    expect(get(draft).pending).toBeNull();
    expect(ipc.run).toHaveBeenCalledOnce();
    expect(document.querySelector('form button')).toBeNull();
    await userEvent.keyboard('{Enter}');
    await expect.poll(() => ipc.run.mock.calls.length).toBe(2);
    expect(ipc.run.mock.calls[1][0].command_id).not.toBe(ipc.run.mock.calls[0][0].command_id);
  });

  it.each([new Error('Reply interrupted'), { code: 'filesystem_error', message: 'Receipt write interrupted' }])('retains explicit recovery for an uncertain chat admission: %s', async failure => {
    await render();
    ipc.run.mockRejectedValue(failure);
    await page.getByRole('textbox', { name: 'Message' }).fill('Continue');
    await userEvent.keyboard('{Enter}');
    await expect.element(page.getByRole('button', { name: 'Check result' })).toBeVisible();
    await page.getByRole('button', { name: 'Check result' }).click();
    expect(ipc.run).toHaveBeenCalledOnce();
  });

});

it('chat input preserves IME confirmation, Alt-Return and native editing context-menu ownership', async () => {
  await render();
  const field = page.getByRole('textbox', { name: 'Message' }); await field.fill('Draft');
  const input = field.element();
  for (const init of [{ key: 'Enter', keyCode: 229 }, { key: 'Enter', altKey: true }]) {
    const event = new KeyboardEvent('keydown', { ...init, bubbles: true, cancelable: true }); input.dispatchEvent(event); expect(event.defaultPrevented).toBe(false);
  }
  expect(ipc.run).not.toHaveBeenCalled();
  const menu = new MouseEvent('contextmenu', { bubbles: true, cancelable: true }); input.dispatchEvent(menu); expect(menu.defaultPrevented).toBe(false);
});
