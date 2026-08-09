import type { BranchCard } from './types';

export interface VerifiedGhostSuggestion {
  candidateId: string;
  presentationKey: string;
  text: string;
}

export function verifiedGhostSuggestion(
  branch: BranchCard | null,
  hydratedBlobId: string | undefined
): VerifiedGhostSuggestion | null {
  if (
    !branch ||
    branch.status !== 'ready' ||
    !branch.candidate_id ||
    !branch.output_blob_id ||
    branch.output_byte_len === null ||
    branch.output_byte_len < 0 ||
    hydratedBlobId !== branch.output_blob_id ||
    !branch.text ||
    !/\S/u.test(branch.text)
  ) return null;

  const actualBytes = new TextEncoder().encode(branch.text).byteLength;
  if (actualBytes !== branch.output_byte_len) return null;

  return {
    candidateId: branch.candidate_id,
    presentationKey: `${branch.candidate_id}:${branch.output_blob_id}`,
    text: branch.text
  };
}
