import { mount, tick, unmount } from 'svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import LoompadBrowserHarness from './LoompadBrowserHarness.svelte';
import type { CompletionCandidate } from './completionSession';
import '../app.css';

let harness: ReturnType<typeof LoompadBrowserHarness> | undefined;
afterEach(async () => { if (harness) await unmount(harness); harness = undefined; document.body.replaceChildren(); });
function candidate(index: number, text = `Choice ${index}. More writing.`): CompletionCandidate {
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

describe('Loompad pages', () => {
  it('pages every supplied choice forward/backward without inserting a Space or accepting a candidate', async () => {
    const { onChoose, onAccept } = await open();
    expect(label('w')).toBe('W: Choice 1.');
    await userEvent.keyboard(' ');
    expect(label('w')).toBe('W: Choice 5.');
    await userEvent.keyboard('{Shift>} {/Shift}');
    expect(label('w')).toBe('W: Choice 1.');
    await userEvent.keyboard('{Shift>} {/Shift}');
    expect(label('w')).toBe('W: Choice 9.');
    await userEvent.keyboard('[KeyW]');
    expect(onChoose).toHaveBeenLastCalledWith(candidate(9));
    expect(onAccept).not.toHaveBeenCalled();
    expect((page.getByRole('textbox', { name: 'Writing' }).element() as HTMLTextAreaElement).value).toBe('');
    await userEvent.keyboard(' ');
    expect(label('w')).toBe('W: Choice 1.');
  });

  it('keeps a consumed remainder on its key while pruning empty pages and surfacing new choices', async () => {
    const { onAccept } = await open(8);
    await userEvent.keyboard(' ');
    expect(label('d')).toBe('D: Choice 8.');
    const remainder = candidate(8, 'Remaining words.');
    harness!.update({ choices: [remainder] }); await tick();
    expect(label('d')).toBe('D: Remaining words.');
    expect(label('w')).toBe('W: Waiting');
    await userEvent.keyboard('[KeyD>][KeyI][/KeyD]');
    expect(onAccept).toHaveBeenLastCalledWith(remainder, 'word');
    harness!.update({ choices: [remainder, candidate(9), candidate(10), candidate(11), candidate(12)] }); await tick();
    expect(label('d')).toBe('D: Remaining');
    await userEvent.keyboard(' ');
    expect(label('w')).toBe('W: Choice');
    await userEvent.keyboard('[KeyW>][KeyI][/KeyW]');
    expect(onAccept).toHaveBeenLastCalledWith(candidate(12), 'word');
  });

  it('clears held chords on page, scope and focus changes instead of accepting a stale key', async () => {
    const { onAccept } = await open(8);
    await userEvent.keyboard('[KeyW>] ');
    expect(document.querySelector('.held')).toBeNull();
    await userEvent.keyboard('[KeyI][/KeyW]');
    expect(onAccept).not.toHaveBeenCalled();
    await userEvent.keyboard('[KeyW>]');
    harness!.update({ scope: 'second' }); await tick();
    expect(label('w')).toBe('W: Choice 1.');
    expect(document.querySelector('.held')).toBeNull();
    await userEvent.keyboard('[KeyI][/KeyW]');
    expect(onAccept).not.toHaveBeenCalled();
    await userEvent.keyboard('[KeyW>]');
    harness!.update({ focused: false }); await tick();
    expect(document.querySelector('.held')).toBeNull();
    harness!.update({ focused: true }); await tick();
    await userEvent.keyboard('[KeyI][/KeyW]');
    expect(onAccept).not.toHaveBeenCalled();
    await userEvent.keyboard('[KeyW>][KeyI][/KeyW]');
    expect(onAccept).toHaveBeenLastCalledWith(candidate(1), 'word');
  });
});
