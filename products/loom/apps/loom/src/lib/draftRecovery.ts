export interface StaleDraftIdentity {
  document_id: string;
  source_revision_id: string;
  version: string;
  text: string;
}

export interface DraftWriteIdentity {
  documentId: string;
  sourceRevisionId: string;
  expectedVersion: string;
  text: string;
}

/**
 * True only for the exact write that atomically replaces a stale draft with
 * the same recovered bytes bound to a different, current source revision.
 */
export function writeRebindsStaleDraft(
  stale: StaleDraftIdentity | null,
  write: DraftWriteIdentity
): boolean {
  return Boolean(
    stale &&
      stale.document_id === write.documentId &&
      stale.version === write.expectedVersion &&
      stale.text === write.text &&
      stale.source_revision_id !== write.sourceRevisionId
  );
}
