import type { BranchCard } from './types';
import {
  candidateSurfaceDecision,
  type CandidateSurfaceDecision
} from './candidateSurface';

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
  presentationCompatible?: (text: string) => boolean;
}

export type AutocompleteExhaustionReason =
  | Exclude<CandidateSurfaceDecision, { surface: true }>['reason']
  | 'invalid'
  | 'unpresentable';

export type AutocompleteDisposition =
  | { kind: 'inactive' }
  | { kind: 'awaiting_candidates' }
  | { kind: 'awaiting_hydration'; runIds: readonly string[] }
  | { kind: 'available'; suggestion: VerifiedGhostSuggestion }
  | {
      kind: 'exhausted';
      candidates: readonly {
        candidateId: string;
        reason: AutocompleteExhaustionReason;
      }[];
    };

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
  const disposition = autocompleteDisposition(selection);
  return disposition.kind === 'available' ? disposition.suggestion : null;
}

/**
 * Keep the absence of inline text typed. Inactive editing, generation still in
 * flight, immutable-body hydration, and a fully rejected family are different
 * states and must never collapse into one nullable "ready" flag.
 */
export function autocompleteDisposition(
  selection: GhostSuggestionSelection
): AutocompleteDisposition {
  if (
    !selection.active ||
    selection.targetByte === null ||
    !Number.isSafeInteger(selection.targetByte) ||
    selection.targetByte < 0
  ) return { kind: 'inactive' };

  const dismissed = new Set(selection.dismissedCandidateIds);
  const exactBranches = selection.branches.filter((branch) =>
    Boolean(
      branch.candidate_id &&
      !dismissed.has(branch.candidate_id) &&
      branch.target_start_byte === selection.targetByte &&
      branch.target_end_byte === selection.targetByte
    )
  );
  if (exactBranches.length === 0) return { kind: 'awaiting_candidates' };

  const awaitingHydration: string[] = [];
  const exhausted: Array<{
    candidateId: string;
    reason: AutocompleteExhaustionReason;
  }> = [];
  for (const branch of exactBranches) {
    const candidateId = branch.candidate_id;
    if (!candidateId) continue;
    if (
      !branch.output_blob_id ||
      branch.output_byte_len === null ||
      selection.hydratedBlobByRun[branch.run_id] !== branch.output_blob_id
    ) {
      awaitingHydration.push(branch.run_id);
      continue;
    }
    const verified = verifiedGhostSuggestion(
      branch,
      selection.hydratedBlobByRun[branch.run_id]
    );
    if (!verified) {
      exhausted.push({ candidateId, reason: 'invalid' });
      continue;
    }
    const surface = candidateSurfaceDecision(verified.text);
    if (surface.surface) {
      if (
        selection.presentationCompatible &&
        !selection.presentationCompatible(verified.text)
      ) {
        exhausted.push({ candidateId, reason: 'unpresentable' });
        continue;
      }
      return { kind: 'available', suggestion: verified };
    }
    exhausted.push({ candidateId, reason: surface.reason });
  }
  if (awaitingHydration.length > 0) {
    return { kind: 'awaiting_hydration', runIds: awaitingHydration };
  }
  return { kind: 'exhausted', candidates: exhausted };
}
