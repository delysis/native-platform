import { defaultMarkdownParser, defaultMarkdownSerializer } from 'prosemirror-markdown';
import { AllSelection, EditorState, Selection, TextSelection } from 'prosemirror-state';
import { describe, expect, it } from 'vitest';
import { applyVisualFormat, visualFormatState, type VisualFormatAction } from './visualFormatting';
import { parseVisualMarkdown } from './markdownSafety';

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

  it('changes the current paragraph style from a caret-only palette invocation', () => {
    const doc = defaultMarkdownParser.parse('Words');
    let state = EditorState.create({
      doc,
      selection: TextSelection.create(doc, doc.content.size - 1)
    });
    expect(applyVisualFormat(state, 'title', '', (transaction) => {
      state = state.apply(transaction);
    })).toBe(true);
    expect(defaultMarkdownSerializer.serialize(state.doc)).toBe('# Words');
    expect(visualFormatState(state).block).toBe('title');
  });

  it('arms an inline mark at an empty selection and applies it to subsequent typing', () => {
    const doc = defaultMarkdownParser.parse('Words');
    let state = EditorState.create({
      doc,
      selection: TextSelection.create(doc, doc.content.size - 1)
    });
    expect(applyVisualFormat(state, 'bold', '', (transaction) => {
      state = state.apply(transaction);
    })).toBe(true);
    expect(visualFormatState(state).bold).toBe(true);
    state = state.apply(state.tr.insertText(' more'));
    expect(defaultMarkdownSerializer.serialize(state.doc)).toBe('Words **more**');
  });

  it.each([
    ['bold', '**Words** '],
    ['italic', '*Words* ']
  ] as const)('keeps terminal prose whitespace outside %s delimiters under Select All', (
    action,
    expected
  ) => {
    const doc = parseVisualMarkdown('Words ');
    let state = EditorState.create({ doc, selection: new AllSelection(doc) });
    expect(applyVisualFormat(state, action, '', (transaction) => {
      state = state.apply(transaction);
    })).toBe(true);
    expect(state.selection).toBeInstanceOf(AllSelection);
    expect(defaultMarkdownSerializer.serialize(state.doc)).toBe(expected);
    expect(state.doc.eq(parseVisualMarkdown(expected))).toBe(true);
  });

  it('keeps terminal prose whitespace outside an updated link destination', () => {
    const doc = parseVisualMarkdown('[Words](https://old.example) ');
    let state = EditorState.create({ doc, selection: new AllSelection(doc) });
    expect(applyVisualFormat(state, 'link', 'https://new.example', (transaction) => {
      state = state.apply(transaction);
    })).toBe(true);
    expect(state.selection).toBeInstanceOf(AllSelection);
    expect(defaultMarkdownSerializer.serialize(state.doc))
      .toBe('[Words](https://new.example) ');
    expect(state.doc.eq(parseVisualMarkdown('[Words](https://new.example) '))).toBe(true);
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

  it.each([
    ['blockquote', '> Words'],
    ['bullet_list', '* Words'],
    ['ordered_list', '1. Words']
  ] as const)('maps browser Select All inside the visible text when applying %s', (
    action,
    expected
  ) => {
    const doc = defaultMarkdownParser.parse('Words');
    let state = EditorState.create({ doc, selection: new AllSelection(doc) });
    expect(applyVisualFormat(state, action, '', (transaction) => {
      state = state.apply(transaction);
    })).toBe(true);

    expect(defaultMarkdownSerializer.serialize(state.doc)).toBe(expected);
    expect(state.selection).toBeInstanceOf(TextSelection);
    expect({ from: state.selection.from, to: state.selection.to }).toEqual({
      from: Selection.atStart(state.doc).from,
      to: Selection.atEnd(state.doc).to
    });
  });

  it.each([
    ['---'],
    ['---\n\nWords'],
    ['Words\n\n---']
  ] as const)('handles leaf block boundaries without inventing an invalid text endpoint: %s', (
    markdown
  ) => {
    const doc = defaultMarkdownParser.parse(markdown);
    let state = EditorState.create({ doc, selection: new AllSelection(doc) });
    expect(applyVisualFormat(state, 'blockquote', '', (transaction) => {
      state = state.apply(transaction);
    })).toBe(true);
    expect(state.selection).toBeInstanceOf(AllSelection);
  });

  it.each([
    ['---\n\nWords'],
    ['Words\n\n---']
  ] as const)('retains leaf blocks in the next Select All quote toggle: %s', (markdown) => {
    const doc = defaultMarkdownParser.parse(markdown);
    const original = defaultMarkdownSerializer.serialize(doc);
    let state = EditorState.create({ doc, selection: new AllSelection(doc) });
    expect(applyVisualFormat(state, 'blockquote', '', (transaction) => {
      state = state.apply(transaction);
    })).toBe(true);
    expect(applyVisualFormat(state, 'blockquote', '', (transaction) => {
      state = state.apply(transaction);
    })).toBe(true);
    expect(defaultMarkdownSerializer.serialize(state.doc)).toBe(original);
  });

  it.each([
    ['blockquote', '> Words'],
    ['bullet_list', '* Words'],
    ['ordered_list', '1. Words']
  ] as const)('toggles %s back out across the command-mapped selection', (action, expected) => {
    let state = formatted('Words', action);
    expect(defaultMarkdownSerializer.serialize(state.doc)).toBe(expected);
    expect(applyVisualFormat(state, action, '', (transaction) => {
      state = state.apply(transaction);
    })).toBe(true);
    expect(defaultMarkdownSerializer.serialize(state.doc)).toBe('Words');
  });

  it.each([
    ['> Words', 'blockquote'],
    ['* Words', 'bullet_list'],
    ['1. Words', 'ordered_list']
  ] as const)('lifts %s when browser Select All produces an AllSelection', (markdown, action) => {
    const doc = defaultMarkdownParser.parse(markdown);
    let state = EditorState.create({ doc, selection: new AllSelection(doc) });
    expect(applyVisualFormat(state, action, '', (transaction) => {
      state = state.apply(transaction);
    })).toBe(true);
    expect(defaultMarkdownSerializer.serialize(state.doc)).toBe('Words');
    expect(state.selection).toBeInstanceOf(TextSelection);
    expect({ from: state.selection.from, to: state.selection.to }).toEqual({
      from: Selection.atStart(state.doc).from,
      to: Selection.atEnd(state.doc).to
    });
  });

  it.each([
    ['Plain\n\n> Quoted', 'blockquote', 'Plain\n\nQuoted'],
    ['Plain\n\n* Listed', 'bulletList', 'Plain\n\nListed'],
    ['Plain\n\n1. Listed', 'orderedList', 'Plain\n\nListed']
  ] as const)('reports and removes %s across a mixed AllSelection', (markdown, field, expected) => {
    const doc = defaultMarkdownParser.parse(markdown);
    let state = EditorState.create({ doc, selection: new AllSelection(doc) });
    expect(visualFormatState(state)[field]).toBe(true);
    const action = field === 'bulletList'
      ? 'bullet_list'
      : field === 'orderedList'
        ? 'ordered_list'
        : 'blockquote';
    expect(applyVisualFormat(state, action, '', (transaction) => {
      state = state.apply(transaction);
    })).toBe(true);
    expect(defaultMarkdownSerializer.serialize(state.doc)).toBe(expected);
    expect(state.selection).toBeInstanceOf(TextSelection);
    expect({ from: state.selection.from, to: state.selection.to }).toEqual({
      from: Selection.atStart(state.doc).from,
      to: Selection.atEnd(state.doc).to
    });
  });

  it.each([
    ['> One\n\nPlain\n\n> Two', 'blockquote', 'One\n\nPlain\n\nTwo'],
    ['* One\n\nPlain\n\n* Two', 'bullet_list', 'One\n\nPlain\n\nTwo'],
    ['1. One\n\nPlain\n\n1. Two', 'ordered_list', 'One\n\nPlain\n\nTwo']
  ] as const)('removes every disjoint %s container in one AllSelection transaction', (
    markdown,
    action,
    expected
  ) => {
    const doc = defaultMarkdownParser.parse(markdown);
    let state = EditorState.create({ doc, selection: new AllSelection(doc) });
    expect(applyVisualFormat(state, action, '', (transaction) => {
      state = state.apply(transaction);
    })).toBe(true);
    expect(defaultMarkdownSerializer.serialize(state.doc)).toBe(expected);
    expect(state.selection).toBeInstanceOf(TextSelection);
    expect({ from: state.selection.from, to: state.selection.to }).toEqual({
      from: Selection.atStart(state.doc).from,
      to: Selection.atEnd(state.doc).to
    });
  });

  it('rejects unsafe or selection-free links', () => {
    const doc = defaultMarkdownParser.parse('Words\n');
    let state = EditorState.create({ doc, selection: TextSelection.create(doc, 1) });
    expect(applyVisualFormat(state, 'link', 'https://example.com', (transaction) => {
      state = state.apply(transaction);
    })).toBe(false);
    expect(applyVisualFormat(state, 'link', 'bad url', () => {})).toBe(false);
  });

  it('removes an existing link without changing its text', () => {
    const linked = formatted('[Words](https://example.com)', 'link', 'https://other.example');
    expect(defaultMarkdownSerializer.serialize(linked.doc))
      .toBe('[Words](https://other.example)');
    let state = linked;
    expect(applyVisualFormat(state, 'unlink', '', (transaction) => {
      state = state.apply(transaction);
    })).toBe(true);
    expect(defaultMarkdownSerializer.serialize(state.doc)).toBe('Words');
  });
});
