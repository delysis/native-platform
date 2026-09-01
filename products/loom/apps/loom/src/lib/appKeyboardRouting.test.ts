import { describe, expect, it } from 'vitest';
import { shouldCaptureFormatMenuEscape } from './appKeyboardRouting';

const idleState = {
  formatMenuOpen: true,
  compositionActive: false,
  documentRenameOwnsEscape: false,
  documentMenuOwnsEscape: false,
  modelManagerOwnsEscape: false
};

describe('App Escape capture routing', () => {
  it('captures an open formatting popover before the focused editor', () => {
    expect(shouldCaptureFormatMenuEscape(
      { key: 'Escape', keyCode: 27, defaultPrevented: false, isComposing: false },
      idleState
    )).toBe(true);
  });

  it('preserves rename, document-menu, and model-manager Escape ownership', () => {
    for (const owner of [
      'documentRenameOwnsEscape',
      'documentMenuOwnsEscape',
      'modelManagerOwnsEscape'
    ] as const) {
      expect(shouldCaptureFormatMenuEscape(
        { key: 'Escape', keyCode: 27, defaultPrevented: false, isComposing: false },
        { ...idleState, [owner]: true }
      )).toBe(false);
    }
  });

  it('leaves Escape with an active or browser-reported IME composition', () => {
    expect(shouldCaptureFormatMenuEscape(
      { key: 'Escape', keyCode: 27, defaultPrevented: false, isComposing: false },
      { ...idleState, compositionActive: true }
    )).toBe(false);
    expect(shouldCaptureFormatMenuEscape(
      { key: 'Escape', keyCode: 27, defaultPrevented: false, isComposing: true },
      idleState
    )).toBe(false);
    expect(shouldCaptureFormatMenuEscape(
      { key: 'Escape', keyCode: 229, defaultPrevented: false, isComposing: false },
      idleState
    )).toBe(false);
  });

  it('ignores closed menus, other keys, and already handled events', () => {
    expect(shouldCaptureFormatMenuEscape(
      { key: 'Escape', keyCode: 27, defaultPrevented: false, isComposing: false },
      { ...idleState, formatMenuOpen: false }
    )).toBe(false);
    expect(shouldCaptureFormatMenuEscape(
      { key: 'Enter', keyCode: 13, defaultPrevented: false, isComposing: false },
      idleState
    )).toBe(false);
    expect(shouldCaptureFormatMenuEscape(
      { key: 'Escape', keyCode: 27, defaultPrevented: true, isComposing: false },
      idleState
    )).toBe(false);
  });
});
