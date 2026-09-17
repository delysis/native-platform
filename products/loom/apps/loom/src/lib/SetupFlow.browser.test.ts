import { mount, unmount, tick } from 'svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import SetupFlow from './SetupFlow.svelte';
import '../app.css';

let mounted: ReturnType<typeof mount> | undefined;
afterEach(async () => {
  if (mounted) await unmount(mounted);
  mounted = undefined;
  document.body.replaceChildren();
});

function render(error = '') {
  const shell = document.createElement('div'); shell.className = 'app-shell';
  const writing = document.createElement('textarea'); writing.ariaLabel = 'Writing';
  writing.style.flex = '1'; writing.style.minHeight = '0';
  const target = document.createElement('div');
  shell.append(writing, target); document.body.append(shell);
  const onApply = vi.fn(), onSkip = vi.fn(), onTransfers = vi.fn(), onSettings = vi.fn();
  mounted = mount(SetupFlow, { target, props: { error, onApply, onSkip, onTransfers, onSettings } });
  return { writing, onApply, onSkip, onTransfers, onSettings };
}

describe('configuration alongside a download', () => {
  it('keeps writing usable, preserves answers across steps, and sends only explicit choices', async () => {
    const { writing, onApply, onTransfers } = render();
    await page.getByRole('textbox', { name: 'Writing' }).fill('My exact writing.');
    await page.getByRole('button', { name: 'Chat beside it', exact: false }).click();
    await page.getByRole('button', { name: 'Next', exact: true }).click();
    await tick();
    expect(document.activeElement?.textContent).toBe('Suggestions as you write?');
    await page.getByRole('button', { name: 'Let me ask', exact: false }).click();
    await page.getByRole('button', { name: 'Back', exact: true }).click();
    expect(page.getByRole('button', { name: 'Chat beside it', exact: false }).element().getAttribute('aria-pressed')).toBe('true');
    await page.getByRole('button', { name: 'Next', exact: true }).click();
    await page.getByRole('button', { name: 'Download details' }).click();
    expect(onTransfers).toHaveBeenCalledOnce();
    expect(onApply).not.toHaveBeenCalled();
    await page.getByRole('button', { name: 'Use these settings' }).click();
    expect(onApply).toHaveBeenCalledExactlyOnceWith({ chat: true, suggestions: false });
    expect(writing.value).toBe('My exact writing.');
    expect(writing.getBoundingClientRect().height).toBeGreaterThan(300);
    expect(document.querySelector('[role="dialog"]')).toBeNull();
  });

  it('skip neither saves choices nor invokes download controls', async () => {
    const { onApply, onSkip, onTransfers } = render();
    await page.getByRole('button', { name: 'Skip setup' }).click();
    expect(onSkip).toHaveBeenCalledOnce();
    expect(onApply).not.toHaveBeenCalled();
    expect(onTransfers).not.toHaveBeenCalled();
  });

  it('keeps failed setup repairable without replacing the writing page', async () => {
    const { onSettings, writing } = render('A settings file already exists. Open it to adjust these choices.');
    await page.getByRole('textbox', { name: 'Writing' }).fill('Still writing');
    await expect.element(page.getByRole('alert')).toHaveTextContent('A settings file already exists.');
    await page.getByRole('button', { name: 'Open settings file' }).click();
    expect(onSettings).toHaveBeenCalledOnce();
    expect(writing.value).toBe('Still writing');
  });
});
