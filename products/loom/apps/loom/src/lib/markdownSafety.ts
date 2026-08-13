import { defaultMarkdownParser, defaultMarkdownSerializer } from 'prosemirror-markdown';
import type { Node as ProseMirrorNode } from 'prosemirror-model';

/**
 * Parse Loom's exact visual Markdown dialect. That dialect deliberately
 * reserves every raw U+0009 as manuscript indentation, including at a line
 * edge; it does not assign CommonMark's tab-indented-code meaning to the same
 * byte. Character references are decoded in prose but remain literal in fenced
 * or inline code, so unsupported tabbed code stays outside the visual subset.
 */
export function parseVisualMarkdown(markdown: string): ProseMirrorNode {
  return defaultMarkdownParser.parse(markdown.replaceAll('\t', '&#9;'));
}

export function canRoundTripMarkdownExactly(markdown: string): boolean {
  try {
    return defaultMarkdownSerializer.serialize(parseVisualMarkdown(markdown)) === markdown;
  } catch {
    return false;
  }
}

/**
 * Keep an admitted visual editing session mounted across transient serializer
 * states such as a trailing space. Source/imported text still has to prove an
 * exact dialect parse/serialize round trip before it can enter the editor.
 */
export function canUseVisualMarkdown(markdown: string, visualSessionActive: boolean): boolean {
  return visualSessionActive || canRoundTripMarkdownExactly(markdown);
}
