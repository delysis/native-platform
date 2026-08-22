import { defaultMarkdownParser, defaultMarkdownSerializer } from 'prosemirror-markdown';
import { EditorState, Selection } from 'prosemirror-state';
import type { EditorView } from 'prosemirror-view';
import { describe, expect, it } from 'vitest';
import { visualMarkdownInputRules } from './visualInputRules';

function typeMarkdown(input: string): string {
  const plugin = visualMarkdownInputRules();
  const doc = defaultMarkdownParser.parse('');
  let state = EditorState.create({
    doc,
    selection: Selection.atEnd(doc),
    plugins: [plugin]
  });
  const view = {
    get state() { return state; },
    dispatch(transaction: Parameters<EditorState['apply']>[0]) { state = state.apply(transaction); },
    composing: false
  } as unknown as EditorView;
  for (const character of input) {
    const { from, to } = state.selection;
    const handled = plugin.props.handleTextInput?.call(
      plugin,
      view,
      from,
      to,
      character,
      () => state.tr.insertText(character, from, to)
    ) ?? false;
    if (!handled) view.dispatch(state.tr.insertText(character, from, to));
  }
  return defaultMarkdownSerializer.serialize(state.doc);
}

describe('Write-mode Markdown input rules', () => {
  it('turns block markers into formatting and removes the markers', () => {
    expect(typeMarkdown('# Heading')).toBe('# Heading');
    expect(typeMarkdown('## Subhead')).toBe('## Subhead');
    expect(typeMarkdown('* Item')).toBe('* Item');
    expect(typeMarkdown('1. Item')).toBe('1. Item');
    expect(typeMarkdown('> Quote')).toBe('> Quote');
  });

  it('turns inline Markdown into marks and links without visible delimiters', () => {
    expect(typeMarkdown('This is **bold**.')).toBe('This is **bold**.');
    expect(typeMarkdown('This is *italic*.')).toBe('This is *italic*.');
    expect(typeMarkdown('[Loom](https://example.com)')).toBe('[Loom](https://example.com)');
  });

  it('leaves escaped markers literal', () => {
    expect(typeMarkdown(String.raw`This is \*literal\*.`)).toBe(
      String.raw`This is \\\*literal\\\*.`
    );
  });
});
