import type { BranchCard } from './types';
import { candidateTextIsSurfaceable } from './candidateSurface';

export interface VerifiedGhostSuggestion {
  candidateId: string;
  presentationKey: string;
  text: string;
}

export interface GhostSuggestionSelection {
  active: boolean;
  branches: readonly BranchCard[];
  hydratedBlobByRun: Readonly<Record<string, string>>;
  dismissedCandidateIds: readonly string[];
  targetByte: number | null;
}

export interface GhostReviewAffordance {
  visible: boolean;
  label: string;
  ariaLabel: string;
}

export function ghostReviewAffordance(
  activeGhost: boolean,
  reviewableCount: number
): GhostReviewAffordance {
  if (!Number.isSafeInteger(reviewableCount) || reviewableCount <= 0) {
    return { visible: false, label: '', ariaLabel: '' };
  }
  if (!activeGhost) {
    const suffix = reviewableCount === 1 ? '' : 's';
    return {
      visible: true,
      label: `${reviewableCount} alternative${suffix}`,
      ariaLabel: `Review ${reviewableCount} writing alternative${suffix}`
    };
  }

  const alternatives = reviewableCount - 1;
  if (alternatives <= 0) {
    return {
      visible: true,
      label: 'Review',
      ariaLabel: 'Review the current writing suggestion'
    };
  }
  const suffix = alternatives === 1 ? '' : 's';
  return {
    visible: true,
    label: `${alternatives} more`,
    ariaLabel: `Review the current writing suggestion and ${alternatives} more alternative${suffix}`
  };
}

export function visibleVerifiedGhostSuggestion(
  suggestion: VerifiedGhostSuggestion | null,
  renderedPresentationKey: string
): VerifiedGhostSuggestion | null {
  return suggestion?.presentationKey === renderedPresentationKey ? suggestion : null;
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

/**
 * Select the first displayable continuation from an explicit reactive snapshot.
 *
 * Keep every input explicit: Svelte's legacy reactivity cannot discover state
 * read indirectly through a no-argument component helper.
 */
export function selectVerifiedGhostSuggestion(
  selection: GhostSuggestionSelection
): VerifiedGhostSuggestion | null {
  if (
    !selection.active ||
    selection.targetByte === null ||
    !Number.isSafeInteger(selection.targetByte) ||
    selection.targetByte < 0
  ) return null;

  const dismissed = new Set(selection.dismissedCandidateIds);
  for (const branch of selection.branches) {
    if (
      !branch.candidate_id ||
      dismissed.has(branch.candidate_id) ||
      branch.target_start_byte !== selection.targetByte ||
      branch.target_end_byte !== selection.targetByte
    ) continue;
    const verified = verifiedGhostSuggestion(
      branch,
      selection.hydratedBlobByRun[branch.run_id]
    );
    if (verified && candidateTextIsSurfaceable(verified.text)) return verified;
  }
  return null;
}
