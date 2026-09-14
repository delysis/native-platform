import {
  InputRule,
  inputRules,
  textblockTypeInputRule,
  wrappingInputRule
} from 'prosemirror-inputrules';
import { schema as markdownSchema } from 'prosemirror-markdown';
import type { Attrs, MarkType, Schema } from 'prosemirror-model';
import { TextSelection, type Command } from 'prosemirror-state';

function markedTextRule(
  pattern: RegExp,
  markType: MarkType,
  attrs: (match: RegExpMatchArray) => Attrs | null = () => null
): InputRule {
  return new InputRule(pattern, (state, match, start, end) => {
    const whole = match[1];
    const content = match[2];
    if (!whole || !content) return null;
    const wholeOffset = match[0].lastIndexOf(whole);
    if (wholeOffset < 0) return null;
    const before = state.doc.textBetween(state.selection.$from.start(), start + wholeOffset);
    let codeDelimiter = 0;
    for (const token of before.matchAll(/\\.|`+/g)) {
      if (token[0][0] !== '`') continue;
      if (!codeDelimiter) codeDelimiter = token[0].length;
      else if (token[0].length === codeDelimiter) codeDelimiter = 0;
    }
    if (codeDelimiter) return null;
    const mark = markType.create(attrs(match));
    const inherited = state.storedMarks ?? state.selection.$from.marks();
    return state.tr.replaceWith(
      start + wholeOffset,
      end,
      state.schema.text(content, mark.addToSet(inherited))
    ).setStoredMarks(inherited);
  }, { inCodeMark: false });
}

export function visualMarkdownInputRules(schema: Schema = markdownSchema) {
  return inputRules({
    rules: [
      textblockTypeInputRule(/^(#{1,6})\s$/, schema.nodes.heading, (match) => ({
        level: match[1].length
      })),
      wrappingInputRule(/^\s*>\s$/, schema.nodes.blockquote),
      wrappingInputRule(/^\s*([-+*])\s$/, schema.nodes.bullet_list, { tight: true }),
      wrappingInputRule(
        /^(\d+)\.\s$/,
        schema.nodes.ordered_list,
        (match) => ({ order: Number(match[1]), tight: true }),
        (match, node) => node.childCount + Number(node.attrs.order) === Number(match[1])
      ),
      markedTextRule(/(?:^|[^`\\])(`([^`\n]+)`)$/, schema.marks.code),
      markedTextRule(/(?:^|[^*\\])(\*\*([^*\n]+)\*\*)$/, schema.marks.strong),
      markedTextRule(/(?:^|[^_\\])(__([^_\n]+)__)$/, schema.marks.strong),
      markedTextRule(/(?:^|[^*\\])(\*([^*\n]+)\*)$/, schema.marks.em),
      markedTextRule(/(?:^|[^_\\])(_([^_\n]+)_)$/, schema.marks.em),
      markedTextRule(
        /(?:^|[^!\\])(\[([^\]\n]+)\]\(([^)\s\u0000-\u001f\u007f]+)\))$/,
        schema.marks.link,
        (match) => ({ href: match[3], title: null })
      )
    ]
  });
}

/** Enter completes an authored opening/closing fence; other code stays literal. */
export const visualMarkdownFenceEnter: Command = (state, dispatch) => {
  const { $from, empty } = state.selection;
  if (!empty || $from.parentOffset !== $from.parent.content.size) return false;
  const { paragraph, code_block: codeBlock } = state.schema.nodes;
  const text = $from.parent.textContent;
  if ($from.parent.type === paragraph) {
    const opening = /^```([\w+-]*)$/.exec(text);
    if (!opening) return false;
    dispatch?.(state.tr.delete($from.start(), $from.end())
      .setBlockType($from.start(), $from.start(), codeBlock, { params: opening[1] }));
    return true;
  }
  if ($from.parent.type !== codeBlock || !/(?:^|\n)```$/.test(text)) return false;
  const parent = $from.node(-1);
  const afterIndex = $from.indexAfter(-1);
  if (!parent.canReplaceWith(afterIndex, afterIndex, paragraph)) return false;
  if (dispatch) {
    const removed = text.endsWith('\n```') ? 4 : 3;
    const transaction = state.tr.delete($from.end() - removed, $from.end());
    const after = transaction.mapping.map($from.after());
    transaction.insert(after, paragraph.create());
    transaction.setSelection(TextSelection.create(transaction.doc, after + 1));
    dispatch(transaction.scrollIntoView());
  }
  return true;
};
