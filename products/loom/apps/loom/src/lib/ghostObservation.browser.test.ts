import { mount, unmount } from 'svelte';
import { afterEach, describe, expect, it } from 'vitest';
import LoomEditor from './LoomEditor.svelte';
import SourceEditorBrowserHarness from './SourceEditorBrowserHarness.svelte';
import { unavailableVisualCompletionWitness, type VisualCompletionAccessibilityWitness } from './completionAccessibility';
import '../app.css';

let mounted: ReturnType<typeof mount> | null = null;
afterEach(async () => {
  if (mounted) await unmount(mounted);
  mounted = null;
  document.body.replaceChildren();
});

// Real component + real ProseMirror + real WebKit layout. Input is deliberately
// a component fixture: this does not certify model execution or the App route.
async function render() {
  const outer = document.createElement('div');
  const pane = document.createElement('section');
  pane.className = 'editor-pane';
  pane.style.cssText = 'width:800px;height:400px;overflow:auto';
  outer.append(pane);
  document.body.append(outer);
  let witness: VisualCompletionAccessibilityWitness = unavailableVisualCompletionWitness();
  let changes = 0;
  let insertions = 0;
  const editor = mount(LoomEditor, { target: pane, props: {
    value: 'hello', autofocus: true,
    ghostText: ' world again', ghostCandidateId: 'run:r1', ghostPresentationKey: 'stream:r1:7',
    ghostAnchorByteOffset: 5, ghostInsertsOnAccept: true, surfaceKey: 'test:document:revision',
    onChange: () => { changes += 1; },
    onGhostInsert: () => { insertions += 1; return false; },
    onCompletionAccessibilityChange: (next: VisualCompletionAccessibilityWitness) => { witness = next; }
  } });
  mounted = editor;
  await expect.poll(() => editor.focusAtDocumentEnd()).toBe(true);
  editor.refreshGhostPresentation();
  await expect.poll(() => witness.inline?.text).toBe(' world');
  const glyph = pane.querySelector<HTMLElement>('.loom-visual-ghost')!;
  return { editor, outer, pane, glyph, witness: () => witness,
    changes: () => changes, insertions: () => insertions };
}

describe('rendered inline ghost witness', () => {
  it('observes only painted preview text while keeping the multiword buffer out of manuscript state', async () => {
    const state = await render();
    expect(state.glyph.textContent).toBe(' world');
    expect(state.glyph.getAttribute('aria-hidden')).toBe('true');
    expect(state.witness().inline).toEqual({ presentationKey: 'stream:r1:7', text: ' world', utf8Bytes: 6 });
    expect(state.glyph.getBoundingClientRect().width).toBeGreaterThan(0);
    expect(state.changes()).toBe(0);
    expect(state.insertions()).toBe(0);
  });

  it('refuses blank or substituted glyphs with a still-current presentation key', async () => {
    const state = await render();
    for (const text of ['', ' forged']) {
      state.glyph.textContent = text;
      // Observe synchronously, before ProseMirror has a chance to repair DOM.
      window.dispatchEvent(new Event('resize'));
      expect(state.witness().inline).toBeNull();
      expect(state.editor.acceptGhostWord()).toBe(false);
      expect(state.insertions()).toBe(0);
    }
  });

  it('invalidates an unchanged key when its containing app is hidden and restores it without generation', async () => {
    const state = await render();
    state.outer.style.visibility = 'hidden';
    window.dispatchEvent(new Event('resize'));
    expect(state.witness().inline).toBeNull();
    expect(state.editor.acceptGhostWord()).toBe(false);
    state.outer.style.visibility = '';
    window.dispatchEvent(new Event('resize'));
    expect(state.witness().inline?.presentationKey).toBe('stream:r1:7');
    expect(state.changes()).toBe(0);
    expect(state.insertions()).toBe(0);
  });
});

it('Source uses the same exact-glyph authority while retaining its own insertion bytes', async () => {
  const outer = document.createElement('div');
  document.body.append(outer);
  mounted = mount(SourceEditorBrowserHarness, { target: outer, props: {
    initialValue: 'hello', completionCandidates: [{ runId: 'r1', candidateId: 'run:r1',
      presentationKey: 'stream:r1:7', targetByte: 5, text: ' world again', insertsOnAccept: true }]
  } });
  await expect.poll(() => outer.querySelector('.loom-source-ghost-text')?.textContent).toBe(' world');
  const glyph = outer.querySelector<HTMLElement>('.loom-source-ghost-text')!;
  const editor = outer.querySelector('textarea')!;
  const markdown = () => outer.querySelector('[aria-label="Source Markdown"]')?.textContent;
  const accept = () => editor.dispatchEvent(new KeyboardEvent('keydown', {
    key: 'ArrowRight', altKey: true, bubbles: true, cancelable: true
  }));
  for (const text of ['', ' forged']) {
    glyph.textContent = text;
    accept();
    expect(markdown()).toBe('hello');
    glyph.textContent = ' world';
  }
  outer.style.visibility = 'hidden';
  accept();
  expect(markdown()).toBe('hello');
  outer.style.visibility = '';
  accept();
  await expect.poll(markdown).toBe('hello world '); // Source retains its authorized separator.
  expect(outer.querySelector('[aria-label="Source Generation Requests"]')?.textContent).toBe('0');
});
