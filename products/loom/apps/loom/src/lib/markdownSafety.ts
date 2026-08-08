import { defaultMarkdownParser, defaultMarkdownSerializer } from 'prosemirror-markdown';

export function canRoundTripMarkdownExactly(markdown: string): boolean {
  try {
    return defaultMarkdownSerializer.serialize(defaultMarkdownParser.parse(markdown)) === markdown;
  } catch {
    return false;
  }
}
