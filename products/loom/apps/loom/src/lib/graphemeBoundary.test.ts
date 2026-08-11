import { describe, expect, it, vi } from 'vitest';

import {
  insertionPreservesExtendedGraphemeEdges,
  isExtendedGraphemeBoundary
} from './graphemeBoundary';

describe('extended grapheme boundaries', () => {
  it('rejects scalar boundaries inside combining, ZWJ, and flag clusters', () => {
    expect(isExtendedGraphemeBoundary('e\u0301', 1)).toBe(false);
    expect(isExtendedGraphemeBoundary('e\u0301', 2)).toBe(true);
    expect(isExtendedGraphemeBoundary('👩‍👩', 2)).toBe(false);
    expect(isExtendedGraphemeBoundary('🇺🇳', 2)).toBe(false);
  });

  it('rejects insertions that join either neighboring grapheme', () => {
    expect(insertionPreservesExtendedGraphemeEdges('e', 1, '\u0301 morning')).toBe(false);
    expect(insertionPreservesExtendedGraphemeEdges('👩 waits', 0, '👩‍')).toBe(false);
    expect(insertionPreservesExtendedGraphemeEdges('🇳 waits', 0, '🇺')).toBe(false);
    expect(insertionPreservesExtendedGraphemeEdges('rain waits', 4, ' softly')).toBe(true);
  });

  it('fails closed when the runtime has no segmenter', async () => {
    vi.resetModules();
    const original = Intl.Segmenter;
    Object.defineProperty(Intl, 'Segmenter', { configurable: true, value: undefined });
    try {
      const withoutSegmenter = await import('./graphemeBoundary');
      expect(withoutSegmenter.isExtendedGraphemeBoundary('', 0)).toBe(false);
      expect(withoutSegmenter.insertionPreservesExtendedGraphemeEdges('', 0, 'word')).toBe(false);
    } finally {
      Object.defineProperty(Intl, 'Segmenter', { configurable: true, value: original });
      vi.resetModules();
    }
  });
});
