import { normalizeImportedMarkdown } from './importedMarkdown';
import type { ContextAttachment, ContextAttachmentPresentation } from './types';

export interface MaterialEntry {
  id: string;
  name: string;
  reference: string;
  kind: 'attachment' | 'library' | 'folder' | 'collection';
  retention?: 'ordinary' | 'protected';
  pinned: boolean;
  available: boolean;
  source_path: string | null;
  attachment_id: string | null;
  workspace_path?: string | null;
  /** Native mutable-metadata observation; never part of link/evidence identity. */
  metadata_revision?: string | null;
}
export interface MaterialEvidence {
  id: string;
  reference: string;
  material_id: string;
  retention?: 'ordinary' | 'protected';
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

/** Navigation only. Native reads remain authoritative for all source bytes. */
export interface MaterialNavigation {
  query: string;
  pageIndex: number;
  pdfText: boolean;
  sourceRevision: string | null;
  pdfPageCount: number;
  evidenceId: string | null;
  member: { occurrenceId: string; snapshotId: string } | null;
}

export type MaterialPdfPage = NonNullable<ContextAttachmentPresentation['pdf_pages']>[number];

/** Refuse malformed boundaries instead of changing source bytes with replacement characters. */
export function materialPdfPageText(bytes: Uint8Array, page: MaterialPdfPage): string | null {
  if (!Number.isSafeInteger(page.number) || page.number < 1 ||
      !Number.isSafeInteger(page.start_byte) || !Number.isSafeInteger(page.end_byte) ||
      page.start_byte < 0 || page.end_byte < page.start_byte || page.end_byte > bytes.length) return null;
  try { return new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes.subarray(page.start_byte, page.end_byte)); }
  catch { return null; }
}

export function materialLocatorPage(locator: unknown): number | null {
  if (!locator || typeof locator !== 'object') return null;
  const fields = locator as Record<string, unknown>;
  const page = fields.page_number ?? fields.page ?? fields.page_start ?? mappedPageNumbers(fields)[0];
  return typeof page === 'number' && Number.isSafeInteger(page) && page > 0 ? page : null;
}

function mappedPageNumbers(fields: Record<string, unknown>): number[] {
  if (!Array.isArray(fields.pdf_pages)) return [];
  return fields.pdf_pages.flatMap(page => page && typeof page === 'object' &&
    typeof page.number === 'number' && Number.isSafeInteger(page.number) && page.number > 0 ? [page.number] : []);
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
  if (fields.kind === 'document_revision' && typeof fields.path === 'string') return fields.path;
  const mappedPages = mappedPageNumbers(fields);
  if (mappedPages.length) return mappedPages.length === 1 ? `Page ${mappedPages[0]}`
    : `Pages ${mappedPages.length <= 4 ? mappedPages.join(', ') : `${mappedPages[0]}…${mappedPages.at(-1)}`}`;
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

/** Navigation opens current writing; retained evidence keeps its original revision. */
export function materialWritingDocument(locator: unknown, projectId: string): string | null {
  if (!locator || typeof locator !== 'object') return null;
  const fields = locator as Record<string, unknown>;
  return fields.kind === 'document_revision' && fields.project_id === projectId &&
    typeof fields.document_id === 'string' && fields.document_id.length > 0 && fields.document_id.length <= 128
    ? fields.document_id : null;
}

/** A deliberately inserted source stays distinguishable from the writer's instructions. */
export function materialQuotationMarkdown(reference: string, contents: string): string {
  const quoted = normalizeImportedMarkdown(contents).replace(/\n$/u, '').split('\n').map(line => line ? `> ${line}` : '>').join('\n');
  return `${reference}\n\n${quoted}\n`;
}
