import {
  MarkdownParser, defaultMarkdownParser, defaultMarkdownSerializer, schema as markdownSchema
} from 'prosemirror-markdown';
import { Schema, type Node as ProseMirrorNode } from 'prosemirror-model';

// Root attributes survive ProseMirror transactions and keep source-only EOF
// whitespace out of the editable text while retaining its exact bytes.
export const visualMarkdownSchema = new Schema({
  nodes: {
    ...markdownSchema.spec.nodes.toObject(),
    doc: {
      ...markdownSchema.spec.nodes.get('doc')!,
      attrs: { terminalSuffix: { default: '' } }
    }
  },
  marks: {
    ...markdownSchema.spec.marks.toObject(),
    link: {
      ...markdownSchema.spec.marks.get('link')!,
      toDOM(mark) {
        const attrs: Record<string, string | null> = { href: mark.attrs.href, title: mark.attrs.title };
        if (/^loom-attachment:[a-f0-9]{64}$/u.test(mark.attrs.href)) attrs['aria-description'] = 'Save original…';
        return ['a', attrs, 0];
      }
    }
  }
});
const parsers = new WeakMap<Schema, MarkdownParser>();
function parserFor(schema: Schema): MarkdownParser {
  let parser = parsers.get(schema);
  if (!parser) {
    parser = new MarkdownParser(schema, defaultMarkdownParser.tokenizer, defaultMarkdownParser.tokens);
    parsers.set(schema, parser);
  }
  return parser;
}

/**
 * Parse Loom's exact visual Markdown dialect. That dialect deliberately
 * reserves every raw U+0009 as manuscript indentation, including at a line
 * edge; it does not assign CommonMark's tab-indented-code meaning to the same
 * byte. Character references are decoded in prose but remain literal in fenced
 * or inline code, so unsupported tabbed code stays outside the visual subset.
 */
export function parseVisualMarkdown(
  markdown: string,
  schema: Schema = visualMarkdownSchema
): ProseMirrorNode {
  const terminalSuffix = markdown.match(/(?:\r\n|\n)+$/)?.[0] ?? '';
  if (terminalSuffix && !schema.topNodeType.spec.attrs?.terminalSuffix) {
    throw new Error('This document schema cannot retain terminal newlines.');
  }
  const body = markdown.slice(0, markdown.length - terminalSuffix.length);
  let parserSource = body.replaceAll('\t', '&#9;');
  if (differsOnlyByHarmlessTerminalProseSpace(body)) {
    parserSource = `${parserSource.slice(0, -1)}&#32;`;
  }
  const parsed = parserFor(schema).parse(parserSource);
  return parsed.type.create({ ...parsed.attrs, terminalSuffix }, parsed.content, parsed.marks);
}

export function canRoundTripMarkdownExactly(markdown: string): boolean {
  try {
    return serializeVisualMarkdown(parseVisualMarkdown(markdown)) === markdown;
  } catch {
    return false;
  }
}

function differsOnlyByHarmlessTerminalProseSpace(markdown: string): boolean {
  if (!markdown.endsWith(' ') || markdown.endsWith('  ')) return false;

  try {
    const parsed = defaultMarkdownParser.parse(markdown.replaceAll('\t', '&#9;'));
    let tail: ProseMirrorNode | null = parsed.lastChild;
    while (tail && !tail.isTextblock && tail.lastChild) tail = tail.lastChild;

    if (!tail || (tail.type.name !== 'paragraph' && tail.type.name !== 'heading')) {
      return false;
    }

    return defaultMarkdownSerializer.serialize(parsed) === markdown.slice(0, -1);
  } catch {
    return false;
  }
}

/**
 * Return the canonical byte surface shared by the visual document and store.
 *
 * Loom's parser encodes the one serializer-proven terminal prose space as a
 * character reference before parsing, so ProseMirror preserves the writer's
 * separator and serializes it back to the same byte. The function remains the
 * single normalization boundary for callers, but no longer creates a second,
 * shorter manuscript identity behind the mounted editor.
 */
export function normalizeVisualMarkdownSource(markdown: string): string {
  return markdown;
}

/**
 * Serialize the live ProseMirror document to the one canonical byte sequence
 * that both persistence and completion anchoring consume. Local edits and
 * externally restored Markdown must cross the same normalization boundary.
 */
export function serializeVisualMarkdown(document: ProseMirrorNode): string {
  return defaultMarkdownSerializer.serialize(document) + (document.attrs.terminalSuffix ?? '');
}

/**
 * Keep an admitted visual editing session mounted across transient serializer
 * states. Source/imported text must still prove an exact dialect round trip,
 * including the explicitly preserved single terminal prose space. Two spaces
 * can encode a hard break, while code and unsupported syntax remain fail-closed.
 */
export function canUseVisualMarkdown(markdown: string, visualSessionActive: boolean): boolean {
  return visualSessionActive ||
    canRoundTripMarkdownExactly(markdown);
}
