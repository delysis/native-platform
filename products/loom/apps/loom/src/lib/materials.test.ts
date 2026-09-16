import { describe, expect, it } from 'vitest';
import { materialReferenceMarkdown, evidenceReferenceMarkdown, importedMaterialMarkdown, materialQuotationMarkdown } from './materials';
import { parseVisualMarkdown, serializeVisualMarkdown } from './markdownSafety';
import type { MaterialEntry } from './materials';
import type { ContextAttachment } from './types';
const material: MaterialEntry = { id: 'material-' + 'a'.repeat(64), name: 'A [real] *source*', reference: '@"materials/source#a"', kind: 'attachment', pinned: false, available: true, source_path: null, attachment_id: 'a'.repeat(64) };
describe('retained source references', () => {
  it('escapes untrusted labels while preserving a resolvable immutable link through the editor', () => {
    const markdown = materialReferenceMarkdown(material);
    const parsed = parseVisualMarkdown(markdown);
    expect(parsed.textContent).toBe('@A [real] *source*');
    expect(serializeVisualMarkdown(parsed)).toContain(`loom-material:${material.id}`);
    expect(parsed.firstChild?.firstChild?.marks[0].attrs.href).toBe(`loom-material:${material.id}`);
  });
  it('keeps PDF extraction out of ordinary import and retains native audio inline', () => {
    const attachment = { editable_markdown: 'An entire private book.', inline_markdown: 'old', media_markdown: null } as ContextAttachment;
    expect(importedMaterialMarkdown(attachment, material)).toBe(materialReferenceMarkdown(material));
    expect(importedMaterialMarkdown({ ...attachment, media_markdown: '![Audio: voice](loom-attachment:source/media)' }, material)).toBe('![Audio: voice](loom-attachment:source/media)');
  });
  it('keeps imported mentions quoted through the visual editor round trip', () => {
    const markdown = materialQuotationMarkdown(materialReferenceMarkdown(material), 'From a source: @Another\n\n@YetAnother');
    const parsed = parseVisualMarkdown(markdown);
    expect(parsed.child(1).type.name).toBe('blockquote');
    expect(parsed.child(1).textContent).toContain('@Another');
    const restored = serializeVisualMarkdown(parsed);
    expect(restored).toContain('> From a source: @Another');
    expect(restored).toContain('> @YetAnother');
  });
  it('retains passage identity separately from collection identity', () => {
    expect(evidenceReferenceMarkdown({ id: 'b'.repeat(64), title: 'Chapter 2' })).toBe(`[@Chapter 2](loom-evidence:${'b'.repeat(64)})`);
  });
});
