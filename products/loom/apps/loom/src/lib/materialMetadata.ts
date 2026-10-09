import type { MaterialEntry } from './materials';

/** This is an optimistic metadata observation, not source/path authority. */
export function canEditMaterialMetadata(material: Readonly<MaterialEntry>): boolean {
  return material.kind !== 'folder' && material.retention !== 'protected' &&
    typeof material.metadata_revision === 'string' && /^[a-f0-9]{64}$/u.test(material.metadata_revision);
}
export function materialMetadataRevision(material: Readonly<MaterialEntry>): string {
  if (!canEditMaterialMetadata(material)) throw new Error('Select this source again before changing its metadata.');
  return material.metadata_revision!;
}

/** Names are display text, not filesystem paths. Never trim/truncate user input. */
export function materialDisplayNameError(name: string): string | null {
  return !name.trim() || new TextEncoder().encode(name).length > 512 || /[\u0000-\u001f\u007f-\u009f]/u.test(name)
    ? 'A source name must contain 1–512 printable UTF-8 bytes.' : null;
}

export interface MaterialRenameRequest {
  readonly projectId: string;
  readonly sessionId: string;
  readonly id: string;
  readonly expectedMetadataRevision: string;
  readonly requestId: string;
  readonly name: string;
}
export interface MaterialRenameReceipt {
  readonly project_id: string;
  readonly session_id: string;
  readonly request_id: string;
  readonly expected_metadata_revision: string;
  readonly material: MaterialEntry;
}

/** Native mutations may change metadata, never stable source identity. */
export function materialSourceIsUnchanged(before: Readonly<MaterialEntry>, after: Readonly<MaterialEntry>): boolean {
  return before.id === after.id && before.reference === after.reference && before.kind === after.kind &&
    before.retention === after.retention && before.source_path === after.source_path &&
    before.attachment_id === after.attachment_id && before.workspace_path === after.workspace_path;
}

/** Re-reading the same native generation must not revoke an active row lease.
 * A changed generation (including remove/re-add of identical bytes) never shares.
 */
export function sameMaterialObservation(before: Readonly<MaterialEntry>, after: Readonly<MaterialEntry>): boolean {
  return canEditMaterialMetadata(before) && canEditMaterialMetadata(after) &&
    before.metadata_revision === after.metadata_revision && materialSourceIsUnchanged(before, after) &&
    before.name === after.name && before.pinned === after.pinned && before.available === after.available;
}

export function materialRenameReceiptMatches(
  request: MaterialRenameRequest, before: Readonly<MaterialEntry>, receipt: MaterialRenameReceipt
): boolean {
  const after = receipt?.material;
  return receipt?.project_id === request.projectId && receipt.session_id === request.sessionId &&
    receipt.request_id === request.requestId && receipt.expected_metadata_revision === request.expectedMetadataRevision &&
    Boolean(after && request.id === before.id && materialSourceIsUnchanged(before, after) &&
      after.name === request.name && after.pinned === before.pinned && after.available === before.available &&
      canEditMaterialMetadata(after) && (after.name === before.name || after.metadata_revision !== before.metadata_revision));
}
