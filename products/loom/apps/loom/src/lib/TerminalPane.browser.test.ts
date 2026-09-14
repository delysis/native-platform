import { mount, unmount, tick } from 'svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import TerminalPane from './TerminalPane.svelte';
import '../app.css';

let mounted: ReturnType<typeof mount> | null = null;
afterEach(async () => {
  if (mounted) await unmount(mounted);
  mounted = null;
  document.body.replaceChildren();
});

function render(open = false) {
  const target = document.createElement('div');
  document.body.append(target);
  const onRun = vi.fn();
  const onOpen = vi.fn();
  const onCancel = vi.fn();
  mounted = mount(TerminalPane, { target, props: {
    open, onRun, onOpen, onCancel, onClose: vi.fn(), onCheck: vi.fn(),
    runs: [{ run_id: 'retained-1', status: 'completed', expression: '',
      output_document_id: 'document-1', output_relative_path: 'Results/Try 1.md',
      preview: 'Retained words, outside the source manuscript.', error: null, created_at_ms: 1 }]
  } });
  return { onRun, onOpen, onCancel };
}

describe('retained output pane', () => {
  it('keeps command entry absent from basic results and opens the normal retained document', async () => {
    const { onOpen } = render(true);
    expect(document.querySelector('textarea')).toBeNull();
    expect(document.querySelector('[placeholder]')).toBeNull();
    await page.getByRole('button', { name: 'Retained words, outside the source manuscript.' }).click();
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
});
