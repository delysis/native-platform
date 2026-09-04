import { describe, expect, it } from 'vitest';
import {
  CLOSED_COMPLETION_LENS,
  completionLensVisible,
  reduceCompletionLens
} from './completionLens';

describe('completion lens state', () => {
  it('opens momentarily for a real family and releases after a swallowed native key-up', () => {
    const open = reduceCompletionLens(CLOSED_COMPLETION_LENS, {
      kind: 'option_down',
      alternativeCount: 4
    });
    expect(open).toEqual({ momentary: true, pinned: false });
    expect(completionLensVisible(open)).toBe(true);

    const resumed = reduceCompletionLens(open, { kind: 'release_option' });
    expect(resumed).toEqual(CLOSED_COMPLETION_LENS);
  });

  it('preserves an explicit pin across transient release', () => {
    const pinned = reduceCompletionLens(CLOSED_COMPLETION_LENS, {
      kind: 'toggle_pin',
      alternativeCount: 4
    });
    const held = reduceCompletionLens(pinned, { kind: 'option_down', alternativeCount: 4 });
    expect(reduceCompletionLens(held, { kind: 'release_option' })).toEqual({
      momentary: false,
      pinned: true
    });
  });

  it('closes both latches when the family collapses or the writer dismisses the lens', () => {
    const open = { momentary: true, pinned: true };
    expect(reduceCompletionLens(open, {
      kind: 'alternatives_changed',
      alternativeCount: 1
    })).toEqual(CLOSED_COMPLETION_LENS);
    expect(reduceCompletionLens(open, { kind: 'close' })).toEqual(CLOSED_COMPLETION_LENS);
  });
});
