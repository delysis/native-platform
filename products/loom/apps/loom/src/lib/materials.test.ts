import { describe, expect, it } from 'vitest';
import { materialReferenceMarkdown, evidenceReferenceMarkdown, importedMaterialMarkdown, materialQuotationMarkdown, materialLocatorLabel, materialWritingDocument, materialPdfPageText } from './materials';
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
  it('shows human locations without exposing SQLite identities or repeating the source paragraph', () => {
    const locator = { block_id: 'D09A34582350094985c267ccb', block_index: 1776, doc_id: 'd1114ed2-dbca-4efd-99df-502c2ef39eee', kind: 'sqlite_block', location_path: 'This is an entire source paragraph. It should never be repeated as a technical location label above the same text.', byte_start: 39827 };
    expect(materialLocatorLabel(locator)).toBe('Passage 1777');
    expect(materialLocatorLabel({ ...locator, location_path: 'Part 2 / Beginnings' })).toBe('Part 2 / Beginnings');
    expect(materialLocatorLabel({ ...locator, location_path: 'D09A34582350094985c267ccb' })).toBe('Passage 1777');
    expect(materialLocatorLabel({ doc_id: locator.doc_id, kind: 'sqlite_block', byte_start: 10 })).toBe('');
    expect(materialLocatorLabel({ page_start: 4, page_end: 7 })).toBe('Pages 4–7');
    expect(materialLocatorLabel({ pdf_pages: [{ number: 1 }, { number: 3 }] })).toBe('Pages 1, 3');
  });
  it('retains passage identity separately from collection identity', () => {
    expect(evidenceReferenceMarkdown({ id: 'b'.repeat(64), title: 'Chapter 2' })).toBe(`[@Chapter 2](loom-evidence:${'b'.repeat(64)})`);
  });
  it('labels retained writing by path and only offers current navigation within the same project', () => {
    const locator = { kind: 'document_revision', project_id: 'project-a', document_id: 'document-a', revision_id: 'old-revision', path: 'Notes/rain.md', start_byte: 13 };
    expect(materialLocatorLabel(locator)).toBe('Notes/rain.md');
    expect(materialWritingDocument(locator, 'project-a')).toBe('document-a');
    expect(materialWritingDocument(locator, 'project-b')).toBeNull();
    expect(materialWritingDocument({ ...locator, kind: 'sqlite_block' }, 'project-a')).toBeNull();
  });
  it('reads PDF page ranges as exact UTF-8 and refuses split code points or invalid ranges', () => {
    const prefix = 'Page one.\n';
    const text = '\uFEFFcafé 🖋\r\n';
    const bytes = new TextEncoder().encode(prefix + text);
    const page = { number: 3, start_byte: new TextEncoder().encode(prefix).length, end_byte: bytes.length };
    expect(materialPdfPageText(bytes, page)).toBe(text);
    expect(materialPdfPageText(bytes, { ...page, start_byte: page.start_byte + 1 })).toBeNull();
    expect(materialPdfPageText(bytes, { ...page, end_byte: bytes.length + 1 })).toBeNull();
    expect(materialPdfPageText(bytes, { ...page, number: 0 })).toBeNull();
  });
});
