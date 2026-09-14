import { mount, unmount, tick } from 'svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import TerminalPane from './TerminalPane.svelte';
import '../app.css';
import type { DocumentSummary } from './types';
const ipc = vi.hoisted(() => ({ open: vi.fn() }));
vi.mock('./ipc', async (original) => ({ ...await original<typeof import('./ipc')>(), openDocument: ipc.open }));

let mounted: ReturnType<typeof mount> | null = null;
afterEach(async () => {
  if (mounted) await unmount(mounted);
  mounted = null;
  document.body.replaceChildren();
});

function render(open = false, documents: DocumentSummary[] = [], preview = 'Retained words, outside the source manuscript.') {
  const target = document.createElement('div');
  document.body.append(target);
  const onRun = vi.fn();
  const onOpen = vi.fn();
  const onCancel = vi.fn();
  mounted = mount(TerminalPane, { target, props: {
    open, onRun, onOpen, onCancel, projectId: 'project', sessionId: 'session', documents, onClose: vi.fn(), onCheck: vi.fn(),
    runs: [{ run_id: 'retained-1', status: 'completed', expression: '',
      output_document_id: 'document-1', output_relative_path: 'Results/Try 1.md',
      preview, error: null, created_at_ms: 1 }]
  } });
  return { onRun, onOpen, onCancel };
}

describe('retained output pane', () => {
  it('keeps retained output selectable and opens its ordinary document', async () => {
    const { onOpen } = render(true);
    expect(document.querySelector('textarea')).not.toBeNull();
    expect(document.querySelector('[placeholder]')).toBeNull();
    await page.getByRole('button', { name: 'Results/Try 1.md' }).click();
    expect(onOpen).toHaveBeenCalledWith(expect.objectContaining({ output_document_id: 'document-1' }));
    expect(document.body.textContent).not.toMatch(/@Function|@Document|advanced|⌘|Cmd/);
  });

  it('exposes the focused command field only after the secret key and runs its keyboard action once', async () => {
    const { onRun } = render();
    await tick();
    expect(document.querySelector('#retained-terminal')).toBeNull();
    window.dispatchEvent(new KeyboardEvent('keydown', { key: '`', code: 'Backquote', metaKey: true, bubbles: true, cancelable: true }));
    const entry = page.getByRole('textbox', { name: 'Command' });
    await expect.element(entry).toBeVisible();
    await expect.element(entry).toHaveFocus();
    await entry.fill('A plain continuation');
    entry.element().dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', metaKey: true, bubbles: true, cancelable: true }));
    expect(onRun).toHaveBeenCalledOnce();
  });

  it('replaces the truncated preview with the full revision-bound retained document', async () => {
    const full = ('Output ' + 'x'.repeat(40) + '\n').repeat(100) + ' retained ending';
    ipc.open.mockResolvedValue({ text: full });
    render(true, [{ document_id: 'document-1', relative_path: 'Results/Try 1.md', title: 'Take', kind: 'prose', revision_id: 'revision', active_blob_id: 'blob', word_count: 3, externally_modified: false }]);
    await expect.poll(() => document.querySelector('pre')?.textContent).toBe(full);
    expect(ipc.open).toHaveBeenCalledWith('project', 'session', 'document-1', 'revision', 'blob');
    const viewport = document.querySelector('.terminal-scroll')!;
    await expect.poll(() => viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight).toBeLessThan(2);
    expect(viewport.scrollHeight).toBeGreaterThan(viewport.clientHeight);
    expect(document.activeElement).toBe(document.querySelector('textarea'));
    const selection = window.getSelection();
    const range = document.createRange();
    range.selectNodeContents(document.querySelector('pre')!);
    selection?.removeAllRanges(); selection?.addRange(range);
    expect(selection?.toString()).toBe(full);
  });


  it('does not pull the reader away from scrollback when full output arrives', async () => {
    let resolve: ((document: { text: string }) => void) | undefined;
    ipc.open.mockImplementation(() => new Promise((done) => { resolve = done; }));
    render(true, [{ document_id: 'document-1', relative_path: 'Results/Try 1.md', title: 'Take', kind: 'prose', revision_id: 'revision', active_blob_id: 'blob', word_count: 3, externally_modified: false }], 'Earlier line\n'.repeat(100));
    const viewport = document.querySelector('.terminal-scroll')!;
    await expect.poll(() => viewport.scrollTop).toBeGreaterThan(0);
    await expect.element(page.getByRole('textbox', { name: 'Command' })).toHaveFocus();
    await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    viewport.scrollTop = 0;
    viewport.dispatchEvent(new Event('scroll'));
    await tick();
    resolve!({ text: 'Earlier line\n'.repeat(200) });
    await expect.poll(() => document.querySelector('pre')?.textContent).toBe('Earlier line\n'.repeat(200));
    expect(viewport.scrollTop).toBe(0);
  });

});

it('keeps an unconfirmed peer result inert until Check, Resume, or Cancel is selected', async () => {
  const target = document.createElement('div'); document.body.append(target);
  const onRecover = vi.fn(), onCancelRun = vi.fn(), onRun = vi.fn();
  const run = { run_id: 'peer-run', status: 'unconfirmed' as const, expression: '=@Polish(@Draft)',
    remote: { host: 'bob', model: { name: 'Shared Gemma', fingerprint: 'model' } },
    output_document_id: null, output_relative_path: null, preview: '', error: 'The peer outcome is unconfirmed.', created_at_ms: 1 };
  mounted = mount(TerminalPane, { target, props: { open: true, runs: [run], onRecover, onCancelRun, onRun,
    onCancel: vi.fn(), onOpen: vi.fn(), onClose: vi.fn(), onCheck: vi.fn() } });
  await expect.element(page.getByRole('button', { name: 'Check', exact: true })).toBeVisible();
  expect(onRecover).not.toHaveBeenCalled(); expect(onRun).not.toHaveBeenCalled();
  await page.getByRole('button', { name: 'Check', exact: true }).click();
  expect(onRecover.mock.calls).toEqual([[run, 'check']]);
  await page.getByRole('button', { name: 'Resume', exact: true }).click();
  expect(onRecover.mock.calls[1]).toEqual([run, 'resume']);
  await page.getByRole('button', { name: 'Cancel remaining steps' }).click();
  expect(onCancelRun).toHaveBeenCalledWith(run); expect(onRun).not.toHaveBeenCalled();
});
