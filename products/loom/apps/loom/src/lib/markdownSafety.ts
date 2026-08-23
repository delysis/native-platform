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
  let parserSource = markdown.replaceAll('\t', '&#9;');
  if (differsOnlyByHarmlessTerminalProseSpace(markdown)) {
    parserSource = `${parserSource.slice(0, -1)}&#32;`;
  }
  return defaultMarkdownParser.parse(parserSource);
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
  return normalizeVisualMarkdownSource(defaultMarkdownSerializer.serialize(document));
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
