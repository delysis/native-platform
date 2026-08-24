import { describe, expect, it } from 'vitest';
import {
  AUTOCOMPLETE_MODEL_MENU_LONG_PRESS_MS,
  autocompleteModelMenuLongPressMoved,
  canStartAutocompleteModelMenuLongPress,
  isAutocompleteModelMenuKey
} from './autocompleteModelMenu';

describe('autocomplete model-menu entrypoint', () => {
  it('opens from Menu or unmodified Shift-F10 only', () => {
    expect(isAutocompleteModelMenuKey({
      key: 'ContextMenu', shiftKey: false, altKey: false, ctrlKey: false, metaKey: false
    })).toBe(true);
    expect(isAutocompleteModelMenuKey({
      key: 'F10', shiftKey: true, altKey: false, ctrlKey: false, metaKey: false
    })).toBe(true);
    expect(isAutocompleteModelMenuKey({
      key: 'F10', shiftKey: false, altKey: false, ctrlKey: false, metaKey: false
    })).toBe(false);
    expect(isAutocompleteModelMenuKey({
      key: 'ContextMenu', shiftKey: false, altKey: false, ctrlKey: true, metaKey: false
    })).toBe(false);
  });

  it('reserves long press for primary touch or pen input and cancels on movement', () => {
    expect(AUTOCOMPLETE_MODEL_MENU_LONG_PRESS_MS).toBe(550);
    expect(canStartAutocompleteModelMenuLongPress({
      pointerType: 'touch', button: 0, isPrimary: true
    })).toBe(true);
    expect(canStartAutocompleteModelMenuLongPress({
      pointerType: 'pen', button: 0, isPrimary: true
    })).toBe(true);
    expect(canStartAutocompleteModelMenuLongPress({
      pointerType: 'mouse', button: 0, isPrimary: true
    })).toBe(false);
    expect(canStartAutocompleteModelMenuLongPress({
      pointerType: 'touch', button: 0, isPrimary: false
    })).toBe(false);
    expect(autocompleteModelMenuLongPressMoved({ x: 10, y: 10 }, { x: 20, y: 20 }))
      .toBe(false);
    expect(autocompleteModelMenuLongPressMoved({ x: 10, y: 10 }, { x: 21, y: 10 }))
      .toBe(true);
  });
});
