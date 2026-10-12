import { describe, expect, it } from 'vitest';
import { normalizeImportedMarkdown } from './importedMarkdown';
import { canRoundTripMarkdownExactly } from './markdownSafety';

describe('editable import projection', () => {
  it.each(['# Heading\n\nA **bold** sentence.\n', 'Name,Count\nFixture,42\n', '| Name | Count |\n| --- | --- |\n| Fixture | 42 |', 'Unicode: café • 界 🖋.\n'])('round trips converted imported text: %s', source => {
    const converted = normalizeImportedMarkdown(source);
    expect(canRoundTripMarkdownExactly(converted)).toBe(true);
    expect(converted).not.toContain('loom-attachment:');
  });
});
