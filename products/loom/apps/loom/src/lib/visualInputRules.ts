import {
  InputRule,
  inputRules,
  textblockTypeInputRule,
  wrappingInputRule
} from 'prosemirror-inputrules';
import { schema } from 'prosemirror-markdown';
import type { Attrs, MarkType } from 'prosemirror-model';

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
    const mark = markType.create(attrs(match));
    const inherited = state.storedMarks ?? state.selection.$from.marks();
    return state.tr.replaceWith(
      start + wholeOffset,
      end,
      schema.text(content, mark.addToSet(inherited))
    ).setStoredMarks(inherited);
  });
}

export function visualMarkdownInputRules() {
  return inputRules({
    rules: [
      textblockTypeInputRule(/^(#{1,3})\s$/, schema.nodes.heading, (match) => ({
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
