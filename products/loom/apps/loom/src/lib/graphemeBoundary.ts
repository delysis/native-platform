let graphemeSegmenter: Intl.Segmenter | null | undefined;

function segmenter(): Intl.Segmenter | null {
  if (graphemeSegmenter === undefined) {
    graphemeSegmenter = typeof Intl.Segmenter === 'function'
      ? new Intl.Segmenter(undefined, { granularity: 'grapheme' })
      : null;
  }
  return graphemeSegmenter;
}

/**
 * Prove that a UTF-16 editor offset is an extended-grapheme boundary.
 *
 * Browser selection APIs use UTF-16 offsets, while a visible character may
 * span several Unicode scalars. Loom has no safe approximation when the
 * platform segmenter is unavailable, including at either string edge.
 */
export function isExtendedGraphemeBoundary(text: string, offset: number): boolean {
  if (!Number.isSafeInteger(offset) || offset < 0 || offset > text.length) return false;
  const graphemes = segmenter();
  if (!graphemes) return false;
  if (offset === 0 || offset === text.length) return true;
  for (const segment of graphemes.segment(text)) {
    if (segment.index === offset) return true;
    if (segment.index > offset) return false;
  }
  return false;
}

/** Prove that literal insertion leaves a grapheme edge on both sides. */
export function insertionPreservesExtendedGraphemeEdges(
  text: string,
  offset: number,
  insertion: string
): boolean {
  if (!isExtendedGraphemeBoundary(text, offset)) return false;
  const inserted = text.slice(0, offset) + insertion + text.slice(offset);
  return isExtendedGraphemeBoundary(inserted, offset) &&
    isExtendedGraphemeBoundary(inserted, offset + insertion.length);
}
