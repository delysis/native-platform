import { parseVisualMarkdown, serializeVisualMarkdown, canRoundTripMarkdownExactly } from './markdownSafety';

/** Conversion is confined to imported content, never arbitrary manuscript source. */
export function normalizeImportedMarkdown(markdown: string): string {
  const converted = serializeVisualMarkdown(parseVisualMarkdown(markdown));
  if (!canRoundTripMarkdownExactly(converted)) throw new Error('The imported text could not be represented safely in the editor.');
  return converted;
}
