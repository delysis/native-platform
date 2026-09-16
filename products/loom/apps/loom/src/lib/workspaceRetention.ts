import type { DocumentSummary } from './types';

/** Retention is not navigation: only explicit pins promote run documents to the tree. */
export function visibleWorkspaceDocuments(documents: DocumentSummary[], retainedOutputIds: readonly string[], pinned: ReadonlySet<string>): DocumentSummary[] {
  const outputs = new Set(retainedOutputIds);
  return documents.filter(document => !outputs.has(document.document_id) || pinned.has(document.document_id));
}
const key = (projectId: string) => `loom.pinned-outputs.${projectId}`;
export function readPinnedOutputs(storage: Pick<Storage, 'getItem'>, projectId: string): Set<string> {
  try {
    const value: unknown = JSON.parse(storage.getItem(key(projectId)) ?? '[]');
    return new Set(Array.isArray(value) ? value.filter((id): id is string => typeof id === 'string' && id.length > 0 && id.length <= 256).slice(0, 1024) : []);
  } catch { return new Set(); }
}
export function rememberPinnedOutputs(storage: Pick<Storage, 'setItem'>, projectId: string, ids: ReadonlySet<string>): void {
  storage.setItem(key(projectId), JSON.stringify([...ids].slice(0, 1024)));
}
