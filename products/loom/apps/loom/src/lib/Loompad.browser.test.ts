import { mount, tick, unmount } from 'svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import LoompadBrowserHarness from './LoompadBrowserHarness.svelte';
import type { CompletionCandidate } from './completionSession';
import '../app.css';
let harness: ReturnType<typeof LoompadBrowserHarness> | undefined;
afterEach(async () => { if (harness) await unmount(harness); harness = undefined; document.body.replaceChildren(); });
function candidate(index: number, text = `Choice${index} more writing.`): CompletionCandidate {
  return { runId: `run-${index}`, candidateId: `candidate-${index}`, presentationKey: `presentation-${index}`, text, targetByte: 0, insertsOnAccept: true };
}
async function open(count = 9) {
  const target = document.createElement('div'); document.body.append(target);
  const onChoose = vi.fn(), onAccept = vi.fn();
  harness = mount(LoompadBrowserHarness, { target, props: { choices: Array.from({ length: count }, (_, i) => candidate(i + 1)), onChoose, onAccept } });
  await tick(); await page.getByRole('textbox', { name: 'Writing' }).click();
  return { onChoose, onAccept };
}
const label = (direction: string) => document.querySelector(`[data-direction="${direction}"]`)?.getAttribute('aria-label');
describe('Option word choices', () => {
  it('leaves letters untouched and overlays without displacing writing', async () => {
    const { onAccept } = await open();
    const writing = page.getByRole('textbox', { name: 'Writing' }).element() as HTMLTextAreaElement;
    const bounds = () => { const r = writing.getBoundingClientRect(); return [r.x, r.y, r.width, r.height]; };
    const before = bounds();
    expect(document.querySelector('.loompad')).toBeNull();
    await userEvent.keyboard('wasd ijkl ');
    expect(writing.value).toBe('wasd ijkl ');
    expect(onAccept).not.toHaveBeenCalled();
    await userEvent.keyboard('{Alt>}');
    expect(label('w')).toBe('W: Choice1');
    expect(getComputedStyle(document.querySelector('.loompad')!).position).toBe('absolute');
    expect(bounds()).toEqual(before);
    await userEvent.keyboard('[KeyW>3][/KeyW]{/Alt}');
    expect(onAccept).toHaveBeenCalledExactlyOnceWith(candidate(1), 'word');
    expect(writing.value).toBe('wasd ijkl ');
    expect(document.querySelector('.loompad')).toBeNull();
    expect(bounds()).toEqual(before);
  });
  it('pages unique words and preserves surviving slots across cached branch changes', async () => {
    const { onAccept } = await open();
    await userEvent.keyboard('{Alt>} ');
    expect(label('w')).toBe('W: Choice5');
    await userEvent.keyboard('{Shift>} {/Shift}');
    expect(label('w')).toBe('W: Choice1');
    await userEvent.keyboard('{Shift>} {/Shift}');
    expect(label('w')).toBe('W: Choice9');
    await userEvent.keyboard('[KeyW]');
    expect(onAccept).toHaveBeenLastCalledWith(candidate(9), 'word');
    const remainder = candidate(9, 'Remaining words.');
    harness!.update({ choices: [remainder, candidate(10, 'Remaining different branch.'), candidate(11, 'Another branch.')] });
    await tick();
    expect(label('w')).toBe('W: Remaining');
    expect(label('a')).toBe('A: Another');
    expect(label('s')).toBeUndefined();
    await userEvent.keyboard('[KeyA]{/Alt}');
    expect(onAccept).toHaveBeenLastCalledWith(candidate(11, 'Another branch.'), 'word');
  });
  it('hides on focus loss and rejects an old repeated key after scope replacement', async () => {
    const { onAccept } = await open();
    await userEvent.keyboard('{Alt>}[KeyW>]');
    expect(onAccept).toHaveBeenCalledTimes(1);
    harness!.update({ scope: 'second', choices: [candidate(20)] }); await tick();
    expect(label('w')).toBe('W: Choice20');
    const writing = page.getByRole('textbox', { name: 'Writing' }).element();
    writing.dispatchEvent(new KeyboardEvent('keydown', { code: 'KeyW', key: 'w', altKey: true, repeat: true, bubbles: true }));
    expect(onAccept).toHaveBeenCalledTimes(1);
    harness!.update({ focused: false }); await tick();
    expect(document.querySelector('.loompad')).toBeNull();
    await userEvent.keyboard('[/KeyW]{/Alt}');
  });
});
