import { defaultMarkdownParser, defaultMarkdownSerializer } from 'prosemirror-markdown';

export function canRoundTripMarkdownExactly(markdown: string): boolean {
  try {
    return defaultMarkdownSerializer.serialize(defaultMarkdownParser.parse(markdown)) === markdown;
  } catch {
    return false;
  }
}

/**
 * Keep an admitted visual editing session mounted across transient serializer
 * states such as a trailing space. Source/imported text still has to prove an
 * exact parse/serialize round trip before it can enter the visual editor.
 */
export function canUseVisualMarkdown(markdown: string, visualSessionActive: boolean): boolean {
  return visualSessionActive || canRoundTripMarkdownExactly(markdown);
}
