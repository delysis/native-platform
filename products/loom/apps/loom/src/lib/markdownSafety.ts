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

function differsOnlyByHarmlessTerminalProseSpace(markdown: string): boolean {
  if (!markdown.endsWith(' ') || markdown.endsWith('  ')) return false;

  try {
    const parsed = parseVisualMarkdown(markdown);
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
 * Remove the one source byte that the admitted visual dialect cannot display.
 *
 * A terminal ASCII space in prose has no rendered or Markdown meaning, but it
 * changes the backend completion boundary. Leaving that invisible byte in the
 * project while the visual caret sits before it makes every otherwise valid
 * completion look stale. Normalize only after the parser and serializer prove
 * this exact one-byte discrepancy; meaningful whitespace remains untouched.
 */
export function normalizeVisualMarkdownSource(markdown: string): string {
  return differsOnlyByHarmlessTerminalProseSpace(markdown)
    ? markdown.slice(0, -1)
    : markdown;
}

/**
 * Serialize the live ProseMirror document to the one canonical byte sequence
 * that both persistence and completion anchoring consume. Local edits and
 * externally restored Markdown must cross the same normalization boundary.
 */
export function serializeVisualMarkdown(document: ProseMirrorNode): string {
  return normalizeVisualMarkdownSource(defaultMarkdownSerializer.serialize(document));
}

/**
 * Keep an admitted visual editing session mounted across transient serializer
 * states. Source/imported text must still prove an exact dialect round trip,
 * except for one terminal ASCII space that the serializer demonstrably drops
 * from a prose text block. Two spaces can encode a hard break, while code and
 * unsupported syntax remain fail-closed.
 */
export function canUseVisualMarkdown(markdown: string, visualSessionActive: boolean): boolean {
  return visualSessionActive ||
    canRoundTripMarkdownExactly(markdown) ||
    differsOnlyByHarmlessTerminalProseSpace(markdown);
}
