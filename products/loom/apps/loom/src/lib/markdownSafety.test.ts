import { defaultMarkdownParser } from 'prosemirror-markdown';
import { EditorState, Selection } from 'prosemirror-state';
import { describe, expect, it } from 'vitest';
import {
  canRoundTripMarkdownExactly,
  canUseVisualMarkdown,
  normalizeVisualMarkdownSource,
  serializeVisualMarkdown
} from './markdownSafety';

describe('visual Markdown safety gate', () => {
  it('admits the canonical subset used by the visual editor', () => {
    expect(canRoundTripMarkdownExactly('A quiet paragraph.')).toBe(true);
    expect(canRoundTripMarkdownExactly('# Heading\n\nA paragraph.')).toBe(true);
    expect(canRoundTripMarkdownExactly('A quiet paragraph.\t')).toBe(true);
    expect(canRoundTripMarkdownExactly('A\tquiet paragraph.')).toBe(true);
  });

  it('defines a leading raw tab as visual manuscript indentation, not CommonMark code', () => {
    expect(defaultMarkdownParser.parse('\tA quiet paragraph.').firstChild?.type.name)
      .toBe('code_block');
    expect(canRoundTripMarkdownExactly('\tA quiet paragraph.')).toBe(true);
  });

  it('holds unsupported GFM syntax in the source editor', () => {
    expect(canRoundTripMarkdownExactly('| left | right |\n| --- | --- |\n| one | two |')).toBe(false);
    expect(canRoundTripMarkdownExactly('~~not part of the basic schema~~')).toBe(false);
    expect(canRoundTripMarkdownExactly('```text\nA\tcode line\n```')).toBe(false);
  });

  it('does not eject a live visual editor for an ordinary trailing space', () => {
    expect(canRoundTripMarkdownExactly('It ')).toBe(false);
    expect(canUseVisualMarkdown('It ', true)).toBe(true);
    expect(canUseVisualMarkdown('It ', false)).toBe(true);
  });

  it('admits only a serializer-proven single terminal space in prose', () => {
    expect(canUseVisualMarkdown('# Heading ', false)).toBe(true);

    expect(canUseVisualMarkdown('It  ', false)).toBe(false);
    expect(canUseVisualMarkdown('- A quiet item ', false)).toBe(false);
    expect(canUseVisualMarkdown('```text\ncode\n``` ', false)).toBe(false);
    expect(canUseVisualMarkdown('~~unsupported~~ ', false)).toBe(false);
  });

  it('normalizes only the invisible terminal prose byte that would stale a completion boundary', () => {
    expect(normalizeVisualMarkdownSource('Something ')).toBe('Something');
    expect(normalizeVisualMarkdownSource('# Heading ')).toBe('# Heading');

    expect(normalizeVisualMarkdownSource('Something  ')).toBe('Something  ');
    expect(normalizeVisualMarkdownSource('- A quiet item ')).toBe('- A quiet item ');
    expect(normalizeVisualMarkdownSource('Something\n')).toBe('Something\n');
  });

  it('canonicalizes a terminal space produced by a live ProseMirror edit before persistence', () => {
    const document = defaultMarkdownParser.parse('hello');
    const state = EditorState.create({
      doc: document,
      selection: Selection.atEnd(document)
    });
    const edited = state.tr.insertText(' ').doc;

    expect(serializeVisualMarkdown(edited)).toBe('hello');
  });
});
