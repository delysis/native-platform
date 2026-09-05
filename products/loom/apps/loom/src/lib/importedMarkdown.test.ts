import { describe, expect, it } from 'vitest';
import { editableImportMarkdown, normalizeImportedMarkdown } from './importedMarkdown';
import { canRoundTripMarkdownExactly } from './markdownSafety';
import type { ContextAttachment } from './types';

describe('editable import projection', () => {
  it.each(['# Heading\n\nA **bold** sentence.\n', 'Name,Count\nFixture,42\n', '| Name | Count |\n| --- | --- |\n| Fixture | 42 |', 'Unicode: café • 界 🖋.\n'])('round trips converted imported text: %s', source => {
    const converted = normalizeImportedMarkdown(source);
    expect(canRoundTripMarkdownExactly(converted)).toBe(true);
    expect(converted).not.toContain('loom-attachment:');
  });
  it('selects extracted content instead of the stored paperclip identity', () => {
    expect(editableImportMarkdown({ editable_markdown: 'Editable body.\n', inline_markdown: '[paperclip](loom-attachment:id)' } as ContextAttachment)).toBe('Editable body.');
  });
});
