import type { BranchBody, BranchSummary } from './types';

const verifiedBranchBodyBrand: unique symbol = Symbol('verified-branch-body');

/**
 * Renderer-side proof that untrusted IPC text hashes to the immutable blob
 * identity recorded for this exact run. The private symbol prevents ordinary
 * application code from constructing the proof structurally.
 */
export interface VerifiedBranchBody {
  readonly runId: string;
  readonly branchId: string;
  readonly documentId: string;
  readonly candidateId: string;
  readonly sourceRevisionId: string;
  readonly targetStartByte: number;
  readonly targetEndByte: number;
  readonly seed: string;
  readonly modelId: string;
  readonly createdAtUnixMs: number;
  readonly blobId: string;
  readonly byteLength: number;
  readonly text: string;
  readonly [verifiedBranchBodyBrand]: true;
}

function canonicalSha256(value: string): boolean {
  return /^[0-9a-f]{64}$/u.test(value);
}

async function sha256(bytes: Uint8Array): Promise<string> {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) throw new Error('SHA-256 verification is unavailable in this renderer.');
  const owned = new Uint8Array(bytes.byteLength);
  owned.set(bytes);
  const digest = new Uint8Array(await subtle.digest('SHA-256', owned.buffer));
  return Array.from(digest, (byte) => byte.toString(16).padStart(2, '0')).join('');
}

/** Verify every body identity before its text can enter suggestion selection. */
export async function verifyBranchBody(
  body: BranchBody,
  expected: BranchSummary
): Promise<VerifiedBranchBody | null> {
  if (
    !expected.output_blob_id ||
    expected.output_byte_len === null ||
    !canonicalSha256(expected.output_blob_id) ||
    body.run_id !== expected.run_id ||
    body.branch_id !== expected.branch_id ||
    body.document_id !== expected.document_id ||
    !expected.candidate_id ||
    body.candidate_id !== expected.candidate_id ||
    body.source_revision_id !== expected.source_revision_id ||
    body.target_start_byte !== expected.target_start_byte ||
    body.target_end_byte !== expected.target_end_byte ||
    body.seed !== expected.seed ||
    body.model_id !== expected.model_id ||
    body.created_at_unix_ms !== expected.created_at_unix_ms ||
    body.output_blob_id !== expected.output_blob_id ||
    body.byte_len !== expected.output_byte_len ||
    !Number.isSafeInteger(body.target_start_byte) ||
    body.target_start_byte < 0 ||
    !Number.isSafeInteger(body.target_end_byte) ||
    body.target_end_byte < body.target_start_byte ||
    !Number.isSafeInteger(body.created_at_unix_ms)
  ) return null;

  const bytes = new TextEncoder().encode(body.text);
  if (bytes.byteLength !== body.byte_len) return null;
  if (await sha256(bytes) !== body.output_blob_id) return null;

  return Object.freeze({
    runId: body.run_id,
    branchId: body.branch_id,
    documentId: body.document_id,
    candidateId: body.candidate_id,
    sourceRevisionId: body.source_revision_id,
    targetStartByte: body.target_start_byte,
    targetEndByte: body.target_end_byte,
    seed: body.seed,
    modelId: body.model_id,
    createdAtUnixMs: body.created_at_unix_ms,
    blobId: body.output_blob_id,
    byteLength: body.byte_len,
    text: body.text,
    [verifiedBranchBodyBrand]: true as const
  });
}

export function verifiedBodyMatchesBranch(
  body: VerifiedBranchBody | undefined,
  branch: BranchSummary
): body is VerifiedBranchBody {
  return Boolean(
    body &&
    body.runId === branch.run_id &&
    body.branchId === branch.branch_id &&
    body.documentId === branch.document_id &&
    body.candidateId === branch.candidate_id &&
    body.sourceRevisionId === branch.source_revision_id &&
    body.targetStartByte === branch.target_start_byte &&
    body.targetEndByte === branch.target_end_byte &&
    body.seed === branch.seed &&
    body.modelId === branch.model_id &&
    body.createdAtUnixMs === branch.created_at_unix_ms &&
    body.blobId === branch.output_blob_id &&
    body.byteLength === branch.output_byte_len
  );
}
