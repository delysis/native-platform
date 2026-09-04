import { parseVisualMarkdown, serializeVisualMarkdown, canRoundTripMarkdownExactly } from './markdownSafety';
import type { ContextAttachment } from './types';

/** Conversion is confined to imported content, never arbitrary manuscript source. */
export function normalizeImportedMarkdown(markdown: string): string {
  const converted = serializeVisualMarkdown(parseVisualMarkdown(markdown));
  if (!canRoundTripMarkdownExactly(converted)) throw new Error('The imported text could not be represented safely in the editor.');
  return converted;
}

export function editableImportMarkdown(attachment: ContextAttachment): string {
  const parts = [
    attachment.editable_markdown == null ? null : normalizeImportedMarkdown(attachment.editable_markdown),
    attachment.media_markdown
  ].filter((part): part is string => Boolean(part));
  return parts.length > 0 ? parts.join('\n\n') : attachment.inline_markdown;
}
