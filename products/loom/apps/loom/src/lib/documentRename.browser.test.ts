import { mount, unmount } from 'svelte';
import { afterEach, describe, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import DocumentRenameBrowserHarness from './DocumentRenameBrowserHarness.svelte';

let mounted: ReturnType<typeof mount> | null = null;

afterEach(async () => {
  if (mounted) await unmount(mounted);
  mounted = null;
  document.body.replaceChildren();
});

function render(): void {
  const target = document.createElement('div');
  document.body.append(target);
  mounted = mount(DocumentRenameBrowserHarness, { target });
}

function dispatchComposition(
  input: HTMLInputElement,
  type: 'compositionstart' | 'compositionend',
  data: string
): CompositionEvent {
  const event = new CompositionEvent(type, { bubbles: true, cancelable: true, data });
  input.dispatchEvent(event);
  return event;
}

function dispatchRenameKey(
  input: HTMLInputElement,
  key: 'Escape' | 'Enter',
  keyCode: number,
  isComposing: boolean
): KeyboardEvent {
  const event = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true });
  Object.defineProperties(event, {
    isComposing: { configurable: true, value: isComposing },
    keyCode: { configurable: true, value: keyCode }
  });
  input.dispatchEvent(event);
  return event;
}

function updateInput(input: HTMLInputElement, value: string): void {
  input.value = value;
  input.dispatchEvent(new InputEvent('input', {
    bubbles: true,
    cancelable: false,
    data: value,
    inputType: 'insertCompositionText'
  }));
}

describe('document rename browser behavior', () => {
  it('releases the disabled state before selecting the failed rename input', async () => {
    render();

    const input = page.getByRole('textbox', { name: 'Document title' });
    await input.click();
    await page.getByRole('button', { name: 'Simulate failed rename' }).click();

    await expect.element(page.getByRole('status', { name: 'Rename focus witness' }))
      .toHaveTextContent(
        'disabled-before-release:true;restored:true;active:true;selection:0-9'
      );
    expect(document.activeElement).toBe(input.element());
  });

  it('leaves composing Escape with the IME and requires a later ordinary Escape to cancel', async () => {
    render();
    const input = page.getByRole('textbox', { name: 'IME title field' });
    await input.click();
    const element = input.element() as HTMLInputElement;
    dispatchComposition(element, 'compositionstart', '');
    updateInput(element, '物語');

    const composingEscape = dispatchRenameKey(element, 'Escape', 27, true);

    expect(composingEscape.defaultPrevented).toBe(false);
    await expect.element(page.getByRole('status', { name: 'Rename composition outcome' }))
      .toHaveTextContent('editing');
    await expect.element(input).toBeInTheDocument();

    dispatchComposition(element, 'compositionend', '物語');
    const ordinaryEscape = dispatchRenameKey(element, 'Escape', 27, false);

    expect(ordinaryEscape.defaultPrevented).toBe(true);
    await expect.element(page.getByRole('status', { name: 'Rename composition outcome' }))
      .toHaveTextContent('cancelled');
    await expect.element(input).not.toBeInTheDocument();
  });

  it('honors the legacy 229 composition witness and commits only on a later ordinary Enter', async () => {
    render();
    const input = page.getByRole('textbox', { name: 'IME title field' });
    await input.click();
    const element = input.element() as HTMLInputElement;
    updateInput(element, '章題');

    const legacyCompositionEnter = dispatchRenameKey(element, 'Enter', 229, false);

    expect(legacyCompositionEnter.defaultPrevented).toBe(false);
    await expect.element(page.getByRole('status', { name: 'Rename composition outcome' }))
      .toHaveTextContent('editing');
    dispatchComposition(element, 'compositionend', '章題');

    const ordinaryEnter = dispatchRenameKey(element, 'Enter', 13, false);

    expect(ordinaryEnter.defaultPrevented).toBe(true);
    await expect.element(page.getByRole('status', { name: 'Rename composition outcome' }))
      .toHaveTextContent('committed:enter:章題');
    await expect.element(input).not.toBeInTheDocument();
  });

  it('defers blur until compositionend and commits the final CJK title', async () => {
    render();
    const input = page.getByRole('textbox', { name: 'IME title field' });
    await input.click();
    const element = input.element() as HTMLInputElement;
    dispatchComposition(element, 'compositionstart', '');
    updateInput(element, '物');

    element.blur();

    await expect.element(page.getByRole('status', { name: 'Rename composition outcome' }))
      .toHaveTextContent('editing');
    await expect.element(input).toBeInTheDocument();

    updateInput(element, '物語');
    dispatchComposition(element, 'compositionend', '物語');

    await expect.element(page.getByRole('status', { name: 'Rename composition outcome' }))
      .toHaveTextContent('committed:deferred-blur:物語');
    await expect.element(input).not.toBeInTheDocument();
  });
});
