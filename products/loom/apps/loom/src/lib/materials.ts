import { normalizeImportedMarkdown } from './importedMarkdown';
import type { ContextAttachment, ContextAttachmentPresentation } from './types';

export interface MaterialEntry {
  id: string;
  name: string;
  reference: string;
  kind: 'attachment' | 'library';
  pinned: boolean;
  available: boolean;
  source_path: string | null;
  attachment_id: string | null;
}
export interface MaterialEvidence {
  id: string;
  reference: string;
  material_id: string;
  title: string;
  text: string;
  source_revision: string;
  text_sha256: string;
  locator: unknown;
  complete?: boolean;
  warnings?: string[];
}
export interface MaterialRead {
  material: MaterialEntry;
  text: string;
  complete: boolean;
  warnings: string[];
  source_revision: string;
  evidence: MaterialEvidence[];
  presentation?: ContextAttachmentPresentation | null;
}
export interface MaterialSearch {
  material: MaterialEntry;
  query: string;
  hits: MaterialEvidence[];
  complete: boolean;
  warnings: string[];
}

/** Labels are presentation; stable link destinations carry the bound identity. */
function referenceLabel(name: string): string {
  return `@${name.replace(/[\\[\]`*_<>]/gu, '\\$&').replace(/[\r\n]/gu, ' ')}`;
}
export function materialReferenceMarkdown(material: Pick<MaterialEntry, 'id' | 'name'>): string {
  return `[${referenceLabel(material.name)}](loom-material:${material.id})`;
}
export function evidenceReferenceMarkdown(evidence: Pick<MaterialEvidence, 'id' | 'title'>): string {
  return `[${referenceLabel(evidence.title)}](loom-evidence:${evidence.id})`;
}

/** Import is reference-first; native media keeps its existing inline object. */
export function importedMaterialMarkdown(attachment: ContextAttachment, material: MaterialEntry): string {
  return attachment.media_markdown || materialReferenceMarkdown(material);
}

export function isDatabasePath(path: string): boolean {
  return /\.(?:sqlite3?|db)$/iu.test(path);
}

export function materialLocatorLabel(locator: unknown): string {
  if (!locator || typeof locator !== 'object') return '';
  const fields = locator as Record<string, unknown>;
  const page = fields.page_number ?? fields.page ?? fields.page_start;
  const lastPage = fields.page_end;
  if (typeof page === 'number' && Number.isSafeInteger(page) && page > 0) {
    return typeof lastPage === 'number' && Number.isSafeInteger(lastPage) && lastPage > page
      ? `Pages ${page}–${lastPage}` : `Page ${page}`;
  }
  // Location metadata is retained in full, but opaque identities and paragraph
  // dumps are not useful navigation labels in a writing surface.
  for (const field of ['heading', 'section', 'location_path']) {
    const value = fields[field];
    if (typeof value !== 'string') continue;
    const heading = value.trim();
    if (heading && heading.length <= 72 && !/[\r\n]/u.test(heading) && heading.split(/\s+/u).length <= 10 &&
        !/[.!?]$/u.test(heading) && !heading.startsWith('/') && !/^[a-z]+:\/\//iu.test(heading) &&
        !/^[a-z0-9_-]{16,}$/iu.test(heading)) return heading;
  }
  const index = fields.block_index;
  return typeof index === 'number' && Number.isSafeInteger(index) && index >= 0 && index < Number.MAX_SAFE_INTEGER
    ? `Passage ${index + 1}` : '';
}

/** A deliberately inserted source stays distinguishable from the writer's instructions. */
export function materialQuotationMarkdown(reference: string, contents: string): string {
  const quoted = normalizeImportedMarkdown(contents).replace(/\n$/u, '').split('\n').map(line => line ? `> ${line}` : '>').join('\n');
  return `${reference}\n\n${quoted}\n`;
}
