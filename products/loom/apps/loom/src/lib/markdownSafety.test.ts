import { defaultMarkdownParser } from 'prosemirror-markdown';
import { EditorState, Selection, TextSelection } from 'prosemirror-state';
import { describe, expect, it } from 'vitest';
import { history, undo } from 'prosemirror-history';
import {
  canRoundTripMarkdownExactly,
  canUseVisualMarkdown,
  normalizeVisualMarkdownSource,
  parseVisualMarkdown,
  serializeVisualMarkdown,
  visualMarkdownSchema
} from './markdownSafety';

describe('visual Markdown safety gate', () => {
  it('describes the attachment save action without changing authored labels or titles', () => {
    const source = `[Exact label](loom-attachment:${'ab'.repeat(32)} "Authored title")`;
    const parsed = parseVisualMarkdown(source);
    const mark = parsed.firstChild!.firstChild!.marks[0];
    expect(visualMarkdownSchema.marks.link.spec.toDOM!(mark, true)).toEqual([
      'a', { href: `loom-attachment:${'ab'.repeat(32)}`, title: 'Authored title', 'aria-description': 'Save original…' }, 0
    ]);
    expect(serializeVisualMarkdown(parsed)).toBe(source);
  });

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
    expect(canRoundTripMarkdownExactly('It ')).toBe(true);
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

  it('keeps the admitted terminal prose byte in the canonical manuscript identity', () => {
    expect(normalizeVisualMarkdownSource('Something ')).toBe('Something ');
    expect(normalizeVisualMarkdownSource('# Heading ')).toBe('# Heading ');

    expect(normalizeVisualMarkdownSource('Something  ')).toBe('Something  ');
    expect(normalizeVisualMarkdownSource('- A quiet item ')).toBe('- A quiet item ');
    expect(normalizeVisualMarkdownSource('Something\n')).toBe('Something\n');
  });

  it('round-trips a terminal space produced by a live ProseMirror edit', () => {
    const document = parseVisualMarkdown('hello');
    const state = EditorState.create({
      doc: document,
      selection: Selection.atEnd(document)
    });
    const edited = state.tr.insertText(' ').doc;

    expect(serializeVisualMarkdown(edited)).toBe('hello ');
    expect(parseVisualMarkdown(serializeVisualMarkdown(edited)).eq(edited)).toBe(true);
  });

  it.each(['\n', '\n\n', '\r\n', '\r\n\r\n'])(
    'retains terminal %j through fenced-code editing, undo, and reopen', (suffix) => {
      const source = '```wgsl\nfn shade() {}\n```' + suffix;
      expect(canRoundTripMarkdownExactly(source)).toBe(true);
      const doc = parseVisualMarkdown(source);
      let state = EditorState.create({
        doc, selection: TextSelection.create(doc, 4), plugins: [history()]
      });
      expect(serializeVisualMarkdown(state.doc)).toBe(source);
      state = state.apply(state.tr.insertText('new_'));
      const edited = '```wgsl\nfn new_shade() {}\n```' + suffix;
      expect(serializeVisualMarkdown(state.doc)).toBe(edited);
      expect(parseVisualMarkdown(edited).eq(state.doc)).toBe(true);
      expect(undo(state, (transaction) => { state = state.apply(transaction); })).toBe(true);
      expect(serializeVisualMarkdown(state.doc)).toBe(source);
      expect(parseVisualMarkdown(source).eq(state.doc)).toBe(true);
    }
  );

  it('keeps unsupported syntax and internal line-ending normalization fail-closed', () => {
    expect(canRoundTripMarkdownExactly('~~unsupported~~\r\n')).toBe(false);
    expect(canRoundTripMarkdownExactly('```wgsl\r\ncode\r\n```\r\n')).toBe(false);
    expect(canRoundTripMarkdownExactly('It  \n')).toBe(false);
    expect(serializeVisualMarkdown(parseVisualMarkdown('It \r\n'))).toBe('It \r\n');
  });

});
