import { describe, expect, it } from 'vitest';
import {
  allocateCompletionPopupDomIds,
  completionPopupPlacement,
  type PopupRect
} from './completionPopup';

function rect(left: number, top: number, width = 2, height = 18): PopupRect {
  return { left, top, right: left + width, bottom: top + height, width, height };
}

describe('completion popup placement', () => {
  it('allocates stable option IDs in collision-safe editor namespaces', () => {
    const first = allocateCompletionPopupDomIds('visual');
    const second = allocateCompletionPopupDomIds('visual');
    const source = allocateCompletionPopupDomIds('source');

    expect(new Set([first.listboxId, second.listboxId, source.listboxId]).size).toBe(3);
    expect(first.optionId(0)).toBe(first.optionId(0));
    expect(first.optionId(0)).not.toBe(first.optionId(1));
    expect(first.optionId(0)).not.toBe(second.optionId(0));
    expect(first.optionId(0)).toContain(first.listboxId.replace('-listbox', ''));
  });

  it('stays below the caret when there is room', () => {
    expect(completionPopupPlacement(rect(300, 120), { width: 360, height: 180 }, 1000, 700)).toEqual({
      left: 628,
      top: 146,
      maxHeight: 180,
      side: 'below'
    });
  });

  it('flips above a caret near the bottom edge', () => {
    expect(completionPopupPlacement(rect(300, 650), { width: 360, height: 180 }, 1000, 700)).toEqual({
      left: 628,
      top: 462,
      maxHeight: 180,
      side: 'above'
    });
  });

  it('clamps horizontally and vertically in a tight viewport', () => {
    expect(completionPopupPlacement(rect(610, 210), { width: 360, height: 300 }, 640, 320)).toEqual({
      left: 268,
      top: 12,
      maxHeight: 190,
      side: 'above'
    });
  });
});
