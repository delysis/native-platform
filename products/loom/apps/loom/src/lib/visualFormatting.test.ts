import { defaultMarkdownParser, defaultMarkdownSerializer } from 'prosemirror-markdown';
import { EditorState, TextSelection } from 'prosemirror-state';
import { describe, expect, it } from 'vitest';
import { applyVisualFormat, visualFormatState, type VisualFormatAction } from './visualFormatting';

function formatted(markdown: string, action: VisualFormatAction, href = ''): EditorState {
  const doc = defaultMarkdownParser.parse(markdown);
  let state = EditorState.create({
    doc,
    selection: TextSelection.create(doc, 1, Math.max(1, doc.content.size - 1))
  });
  const applied = applyVisualFormat(state, action, href, (transaction) => {
    state = state.apply(transaction);
  });
  expect(applied).toBe(true);
  return state;
}

describe('Markdown-safe visual formatting', () => {
  it('maps Notes-like paragraph styles to exact Markdown headings', () => {
    expect(defaultMarkdownSerializer.serialize(formatted('Words\n', 'title').doc)).toBe('# Words');
    expect(defaultMarkdownSerializer.serialize(formatted('Words\n', 'heading').doc)).toBe('## Words');
    expect(defaultMarkdownSerializer.serialize(formatted('Words\n', 'subheading').doc)).toBe('### Words');
  });

  it('applies inline marks, quotes, lists, and links losslessly', () => {
    expect(defaultMarkdownSerializer.serialize(formatted('Words\n', 'bold').doc)).toBe('**Words**');
    expect(defaultMarkdownSerializer.serialize(formatted('Words\n', 'italic').doc)).toBe('*Words*');
    expect(defaultMarkdownSerializer.serialize(formatted('Words\n', 'blockquote').doc)).toBe('> Words');
    expect(defaultMarkdownSerializer.serialize(formatted('Words\n', 'bullet_list').doc)).toBe('* Words');
    expect(defaultMarkdownSerializer.serialize(formatted('Words\n', 'ordered_list').doc)).toBe('1. Words');
    const linked = formatted('Words\n', 'link', 'https://example.com');
    expect(defaultMarkdownSerializer.serialize(linked.doc)).toBe('[Words](https://example.com)');
    expect(visualFormatState(linked).linkHref).toBe('https://example.com');
  });

  it('rejects unsafe or selection-free links', () => {
    const doc = defaultMarkdownParser.parse('Words\n');
    let state = EditorState.create({ doc, selection: TextSelection.create(doc, 1) });
    expect(applyVisualFormat(state, 'link', 'https://example.com', (transaction) => {
      state = state.apply(transaction);
    })).toBe(false);
    expect(applyVisualFormat(state, 'link', 'bad url', () => {})).toBe(false);
  });
});
