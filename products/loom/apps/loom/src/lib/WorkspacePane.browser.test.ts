import { mount, unmount, tick } from 'svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import WorkspacePane, { type WorkspacePaneConfig } from './WorkspacePane.svelte';
import type { OpenDocument, TerminalRun } from './types';
import { canUseVisualMarkdown } from './markdownSafety';
import '../app.css';

vi.mock('@tauri-apps/api/core', async (original) => ({ ...await original<typeof import('@tauri-apps/api/core')>(), convertFileSrc: (path: string, protocol: string) => `${protocol}://localhost/${path}` }));
const ipc = vi.hoisted(() => ({ list: vi.fn(), run: vi.fn(), cancel: vi.fn(), open: vi.fn() }));
vi.mock('./ipc', async (original) => ({
  ...await original<typeof import('./ipc')>(),
  listTerminalRuns: ipc.list, runTerminal: ipc.run, cancelTerminalRun: ipc.cancel, openDocument: ipc.open,
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
async function render(config: Partial<WorkspacePaneConfig> = {}, value = source.text, currentSource = source) {
  const target = document.createElement('div'); target.style.height = '400px'; target.style.width = '320px'; document.body.append(target);
  ipc.list.mockResolvedValue(config.kind === 'browser' ? [] : [run('old'), run('unrelated', 'other')]);
  if (!ipc.open.getMockImplementation()) ipc.open.mockResolvedValue({ ...source, text: fullAnswer });
  const beforeRun = vi.fn(async () => currentSource), onOpenDocument = vi.fn(), onChange = vi.fn(), onCompositionChange = vi.fn();
  mounted = mount(WorkspacePane, { target, props: {
    paneId: 'conversation', config: { kind: 'chat', position: 'right', visible: true, title: null, document: null, context: ['Voice notes'], ...config },
    projectId: 'project', sessionId: 'session', source: currentSource, value, documents: [currentSource.summary, { ...source.summary, document_id: 'result', relative_path: 'Runs/1/Answer.md', title: 'Answer' }], beforeRun, onOpenDocument, onChange, onCompositionChange
  } });
  await tick(); await expect.poll(() => ipc.list.mock.calls.length).toBe(1);
  return { beforeRun, onOpenDocument, onChange, onCompositionChange };
}

describe('workspace panes', () => {
  it.each(['\n', '\r\n'])('edits Mine settings as exact source with %j line endings', async (newline) => {
    const text = '# Mine\n\nversion = 1\n'.replaceAll('\n', newline);
    if (newline === '\n') expect(canUseVisualMarkdown(text, false)).toBe(true);
    const settings = { ...source, summary: { ...source.summary, relative_path: '.mine.toml', title: 'Mine settings' }, text };
    const { onChange } = await render({ kind: 'editor' }, text, settings);
    const editor = page.getByRole('textbox', { name: 'Pane editor' });
    expect(editor.element().tagName).toBe('TEXTAREA');
    expect(document.querySelector('[contenteditable=true]')).toBeNull();
    await editor.fill('# Mine\n\nversion = 1\n[assistance]\nsuggestions = false\n');
    expect(onChange).toHaveBeenLastCalledWith('# Mine\n\nversion = 1\n[assistance]\nsuggestions = false\n'.replaceAll('\n', newline));
  });

  it('restores only this pane history and submits one retained raw prompt with explicit context and full prior output', async () => {
    const { beforeRun, onOpenDocument } = await render({ context: ['@document', '@"Voice notes"'], document: '@Draft' });
    await expect.poll(() => document.querySelector('.output')?.textContent).toBe(fullAnswer);
    await page.getByText(fullAnswer, { exact: true }).click();
    const range = document.createRange(); range.selectNodeContents(document.querySelector('.output')!);
    const selection = window.getSelection(); selection?.removeAllRanges(); selection?.addRange(range);
    expect(selection?.toString()).toBe(fullAnswer);
    expect(onOpenDocument).not.toHaveBeenCalled();
    expect(document.querySelectorAll('article')).toHaveLength(1);
    await page.getByRole('button', { name: 'Open output document' }).click();
    expect(onOpenDocument).toHaveBeenCalledWith('result');
    ipc.run.mockImplementation(async (request) => ({ ...run(request.commandId), presentation: request.presentation }));
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
    expect(request.turnBoundary).toBe('chat');
    expect(request.presentation).toEqual({ pane_id: 'conversation', input: 'Continue that idea' });
    expect(request.contextReferences).toEqual(['Draft.md', 'Voice notes', 'Draft']);
    expect(request.expression).toContain('Assistant: ' + fullAnswer);
    expect(request.expression).not.toContain('@document');
    expect(request.expression).toContain('User: Continue that idea\nAssistant:');
    expect(request.sourceRevisionId).toBe('revision');
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

  it('opens the configured editor document rather than editing unrelated current writing', async () => {
    const { onOpenDocument, onChange } = await render({ kind: 'editor', document: '@Answer' });
    await page.getByRole('button', { name: 'Open Answer' }).click();
    expect(document.querySelector('[contenteditable=true]')).toBeNull();
    expect(onOpenDocument).toHaveBeenCalledWith('result');
    expect(onChange).not.toHaveBeenCalled();
  });

  it('binds the sandboxed preview to exact native document identity and retains page generation', async () => {
    await render({ kind: 'browser', document: 'Draft.md' });
    await expect.poll(() => document.querySelector('iframe')?.src).toContain('loom-preview://localhost/v1-project-session-draft-revision-blob');
    const frame = document.querySelector('iframe')!;
    expect(frame.getAttribute('sandbox')).toBe('');
    expect(frame.hasAttribute('srcdoc')).toBe(false);
    expect(frame.referrerPolicy).toBe('no-referrer');
    expect(ipc.open).not.toHaveBeenCalled();
    ipc.run.mockImplementation(async (request) => ({ ...run(request.commandId), presentation: request.presentation }));
    await page.getByRole('textbox', { name: 'Page description' }).fill('A quiet reading page');
    await page.getByRole('button', { name: 'Run', exact: true }).click();
    await expect.poll(() => ipc.run.mock.calls.length).toBe(1);
    expect(ipc.run.mock.calls[0][0].turnBoundary).toBeUndefined();
    expect(ipc.run.mock.calls[0][0].contextReferences).toEqual(['Voice notes', 'Draft.md']);
    expect(ipc.run.mock.calls[0][0].expression).toContain('Write a complete static HTML page for: A quiet reading page');
    expect(ipc.run.mock.calls[0][0].presentation).toEqual({ pane_id: 'conversation', input: 'A quiet reading page' });
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
      active = { ...run(request.commandId), status: 'running', presentation: request.presentation };
      return active;
    });
    ipc.list.mockImplementation(async () => active ? [active] : []);
    await userEvent.keyboard('{Enter}');
    await expect.element(page.getByRole('button', { name: 'Stop', exact: true })).toBeVisible();
    expect(ipc.run.mock.calls[0][0].presentation.input).toBe('Line one\nLine two');
    ipc.cancel.mockImplementation(async () => { active = { ...active!, status: 'cancelled' }; });
    await page.getByRole('button', { name: 'Stop', exact: true }).click();
    await expect.poll(() => document.querySelector('form button')).toBeNull();
    expect(ipc.cancel).toHaveBeenCalledWith('project', 'session', active!.run_id);
  });

  it('retains explicit recovery for an uncertain chat admission', async () => {
    await render();
    ipc.run.mockRejectedValue(new Error('Reply interrupted'));
    await page.getByRole('textbox', { name: 'Message' }).fill('Continue');
    await userEvent.keyboard('{Enter}');
    await expect.element(page.getByRole('button', { name: 'Check result' })).toBeVisible();
    await page.getByRole('button', { name: 'Check result' }).click();
    expect(ipc.run).toHaveBeenCalledOnce();
  });

});
