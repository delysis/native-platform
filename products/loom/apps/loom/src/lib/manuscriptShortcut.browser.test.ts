import { mount, unmount } from 'svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import CabalEditorBrowserHarness from './CabalEditorBrowserHarness.svelte';
import { installManuscriptRunShortcut } from './manuscriptShortcut';

let mounted: ReturnType<typeof mount> | undefined;
let stop: (() => void) | undefined;
afterEach(async () => {
  stop?.(); stop = undefined;
  if (mounted) await unmount(mounted);
  mounted = undefined; document.body.replaceChildren();
});

describe('manuscript run shortcut ownership', () => {
  it('runs once from the real visual editor without changing the manuscript', async () => {
    const target = document.createElement('section'); target.className = 'editor-stage';
    document.body.append(target);
    const harness = mount(CabalEditorBrowserHarness, { target, props: { initial: 'Together.' } });
    mounted = harness;
    const run = vi.fn(); stop = installManuscriptRunShortcut(run);
    const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
    await editor.click();
    editor.element().dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', code: 'Enter', keyCode: 13, metaKey: true, bubbles: true, cancelable: true }));
    expect(run).toHaveBeenCalledOnce();
    harness.flush(); expect(harness.value()).toBe('Together.');
  });

  it('leaves composition, modified Returns, and chat shortcuts with their own editor', () => {
    const stage = document.createElement('section'); stage.className = 'editor-stage';
    const source = document.createElement('textarea'); stage.append(source);
    const chat = document.createElement('textarea'); document.body.append(stage, chat);
    const run = vi.fn(); stop = installManuscriptRunShortcut(run);
    for (const extra of [{ isComposing: true }, { keyCode: 229 }, { shiftKey: true }, { altKey: true }]) {
      source.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', ctrlKey: true, bubbles: true, cancelable: true, ...extra }));
    }
    const send = vi.fn(); chat.addEventListener('keydown', send);
    chat.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', metaKey: true, bubbles: true, cancelable: true }));
    expect(send).toHaveBeenCalledOnce(); expect(run).not.toHaveBeenCalled();
    source.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', ctrlKey: true, bubbles: true, cancelable: true }));
    expect(run).toHaveBeenCalledOnce();
    stop(); source.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', ctrlKey: true, bubbles: true }));
    expect(run).toHaveBeenCalledOnce();
  });
});
