import { mount, tick, unmount } from 'svelte';
import { afterEach, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import LoompadBrowserHarness from './LoompadBrowserHarness.svelte';
import type { CompletionCandidate } from './completionSession';
import '../app.css';

let harness: ReturnType<typeof LoompadBrowserHarness> | undefined;
afterEach(async () => {
  if (harness) await unmount(harness);
  harness = undefined;
  document.body.replaceChildren();
});

it('streams four full previews without changing next-word actions, slots, or manuscript geometry', async () => {
  const choices: CompletionCandidate[] = ['silver', 'amber', 'blue', 'green'].map((word, index) => ({
    runId: `run-${index}`, candidateId: `candidate-${index}`, presentationKey: `stream:run-${index}:1`,
    targetByte: 0, text: ` ${word} light keeps growing`, insertsOnAccept: true
  }));
  const target = document.createElement('div');
  document.body.append(target);
  const onChoose = vi.fn(), onAccept = vi.fn();
  harness = mount(LoompadBrowserHarness, { target, props: { choices, onChoose, onAccept } });
  const writing = page.getByRole('textbox', { name: 'Writing' });
  await writing.click();
  const editor = writing.element() as HTMLTextAreaElement;
  const bounds = () => { const r = editor.getBoundingClientRect(); return [r.x, r.y, r.width, r.height]; };
  const before = bounds();
  const previews = () => Array.from(document.querySelectorAll('.loompad-continuation'), node => node.textContent);
  await userEvent.keyboard('{Alt>}');
  await expect.poll(previews).toEqual(choices.map(choice => choice.text));
  const buttons = Array.from(document.querySelectorAll<HTMLButtonElement>('.loompad-choice'));
  const labels = buttons.map(button => button.getAttribute('aria-label'));
  expect(labels).toEqual(['W:  silver', 'A:  amber', 'S:  blue', 'D:  green']);
  const grown = choices.map((choice, index) => ({
    ...choice, presentationKey: `stream:${choice.runId}:2`, text: `${choice.text} café ${index}  `
  }));
  harness.update({ choices: [grown[3], grown[1], grown[0], grown[2]] });
  await tick();
  await expect.poll(previews).toEqual(grown.map(choice => choice.text));
  expect(Array.from(document.querySelectorAll('.loompad-choice'))).toEqual(buttons);
  expect(buttons.map(button => button.getAttribute('aria-label'))).toEqual(labels);
  expect(buttons.every(button => !button.disabled)).toBe(true);
  for (const preview of document.querySelectorAll('.loompad-continuation')) {
    expect(getComputedStyle(preview).whiteSpace).toBe('pre-wrap');
    expect(getComputedStyle(preview).textOverflow).not.toBe('ellipsis');
    expect(preview.getBoundingClientRect().height).toBeGreaterThan(0);
  }
  expect(bounds()).toEqual(before);
  expect(editor.value).toBe('');
  expect(onAccept).not.toHaveBeenCalled();
  await userEvent.keyboard('[KeyW]{/Alt}');
  // This boundary dispatches one word. Exact document insertion is exercised
  // separately by completionController.browser.test.ts with the real editors.
  expect(onAccept).toHaveBeenCalledExactlyOnceWith(grown[0], 'word');
  expect(editor.value).toBe('');
  expect(document.querySelector('.loompad')).toBeNull();
});

it('Loompad leaves legacy IME keys to composition without requesting manuscript acceptance', async () => {
  const target = document.createElement('div'); document.body.append(target);
  const choices: CompletionCandidate[] = ['silver', 'amber', 'blue', 'green'].map((word, index) => ({ runId: `run-${index}`, candidateId: `candidate-${index}`, presentationKey: `stream:${index}`, targetByte: 0, text: ` ${word} light`, insertsOnAccept: true }));
  const onAccept = vi.fn();
  const mounted = mount(LoompadBrowserHarness, { target, props: { choices, onAccept, onChoose: vi.fn() } });
  try {
    await tick(); const editor = target.querySelector('textarea')!; editor.focus();
    const ime = new KeyboardEvent('keydown', { key: 'Process', code: 'KeyW', altKey: true, keyCode: 229, isComposing: false, bubbles: true, cancelable: true });
    editor.dispatchEvent(ime);
    expect(ime.defaultPrevented).toBe(false);
    expect(onAccept).not.toHaveBeenCalled();
  } finally { await unmount(mounted); target.remove(); }
});
