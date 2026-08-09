import { describe, expect, it } from 'vitest';
import { canRoundTripMarkdownExactly, canUseVisualMarkdown } from './markdownSafety';

describe('visual Markdown safety gate', () => {
  it('admits the canonical subset used by the visual editor', () => {
    expect(canRoundTripMarkdownExactly('A quiet paragraph.')).toBe(true);
    expect(canRoundTripMarkdownExactly('# Heading\n\nA paragraph.')).toBe(true);
  });

  it('holds unsupported GFM syntax in the source editor', () => {
    expect(canRoundTripMarkdownExactly('| left | right |\n| --- | --- |\n| one | two |')).toBe(false);
    expect(canRoundTripMarkdownExactly('~~not part of the basic schema~~')).toBe(false);
  });

  it('does not eject a live visual editor for an ordinary trailing space', () => {
    expect(canRoundTripMarkdownExactly('It ')).toBe(false);
    expect(canUseVisualMarkdown('It ', true)).toBe(true);
    expect(canUseVisualMarkdown('It ', false)).toBe(false);
  });
});
