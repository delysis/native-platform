import { mount, unmount, tick } from 'svelte';
import { afterEach, describe, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import CabalEditorBrowserHarness from './CabalEditorBrowserHarness.svelte';
import '../app.css';

let mounted: ReturnType<typeof mount> | undefined;
afterEach(async () => { if (mounted) await unmount(mounted); mounted = undefined; document.body.replaceChildren(); });
async function open(initial: string) {
  const target = document.createElement('div'); document.body.append(target);
  const harness = mount(CabalEditorBrowserHarness, { target, props: { initial } }); mounted = harness;
  await page.getByRole('textbox', { name: 'Manuscript editor' }).click();
  return harness;
}

describe('shared text in the real WebKit writing surface', () => {
  it('keeps local undo while an incoming edit changes another paragraph', async () => {
    const harness = await open('Alpha\n\nBeta');
    await userEvent.keyboard('{Meta>}{End}{/Meta}!');
    harness.flush();
    expect(harness.value()).toBe('Alpha\n\nBeta!');
    expect(harness.receive('Shared Alpha\n\nBeta!')).toBe(true); await tick();
    expect(document.querySelector('.ProseMirror')?.textContent).toBe('Shared AlphaBeta!');
    await userEvent.keyboard('{Meta>}z{/Meta}'); harness.flush();
    expect(harness.value()).toBe('Shared Alpha\n\nBeta');
  });

  it('maps the live caret through a remote prefix without stealing focus', async () => {
    const harness = await open('A little shared mind.');
    await userEvent.keyboard('{End}');
    expect(harness.receive('🌻 A little shared mind.')).toBe(true); await tick();
    await userEvent.keyboard('!'); harness.flush();
    expect(harness.value()).toBe('🌻 A little shared mind.!');
    expect(document.activeElement?.classList.contains('ProseMirror')).toBe(true);
  });

  it('refuses an incoming projection while an IME composition is active', async () => {
    const harness = await open('Together');
    const surface = page.getByRole('textbox', { name: 'Manuscript editor' }).element();
    surface.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true, data: '' }));
    expect(harness.receive('A remote replacement')).toBe(false);
    expect(harness.value()).toBe('Together');
    surface.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true, data: '' }));
    await tick(); harness.flush();
    expect(harness.receive('Together, remotely')).toBe(true);
  });
});
