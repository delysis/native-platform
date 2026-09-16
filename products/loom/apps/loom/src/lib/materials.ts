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
  if (typeof locator === 'string') return locator;
  if (!locator || typeof locator !== 'object') return '';
  const fields = locator as Record<string, unknown>;
  return Object.entries(fields).filter(([, value]) => typeof value === 'string' || typeof value === 'number')
    .map(([key, value]) => `${key.replaceAll('_', ' ')}: ${value}`).join(' · ');
}

/** A deliberately inserted source stays distinguishable from the writer's instructions. */
export function materialQuotationMarkdown(reference: string, contents: string): string {
  const quoted = normalizeImportedMarkdown(contents).replace(/\n$/u, '').split('\n').map(line => line ? `> ${line}` : '>').join('\n');
  return `${reference}\n\n${quoted}\n`;
}
