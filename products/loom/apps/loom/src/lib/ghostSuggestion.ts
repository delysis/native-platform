import type { BranchCard } from './types';
import type { VerifiedBranchBody } from './branchBodyProof';
import { verifiedBodyMatchesBranch } from './branchBodyProof';

export interface VerifiedGhostSuggestion {
  candidateId: string;
  presentationKey: string;
  targetByte: number;
  text: string;
}

export function verifiedGhostSuggestion(
  branch: BranchCard | null,
  body: VerifiedBranchBody | undefined
): VerifiedGhostSuggestion | null {
  if (
    !branch ||
    branch.status !== 'ready' ||
    !branch.candidate_id ||
    !branch.output_blob_id ||
    branch.output_byte_len === null ||
    branch.output_byte_len < 0 ||
    !Number.isSafeInteger(branch.target_start_byte) ||
    branch.target_start_byte < 0 ||
    branch.target_end_byte !== branch.target_start_byte ||
    !verifiedBodyMatchesBranch(body, branch) ||
    branch.text !== body.text ||
    !body.text
  ) return null;

  const actualBytes = new TextEncoder().encode(body.text).byteLength;
  if (actualBytes !== branch.output_byte_len) return null;

  return {
    candidateId: branch.candidate_id,
    presentationKey: `${branch.candidate_id}:${branch.output_blob_id}`,
    targetByte: branch.target_start_byte,
    text: body.text
  };
}
