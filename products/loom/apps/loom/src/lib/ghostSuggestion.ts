import type { BranchCard } from './types';
import type { VerifiedBranchBody } from './branchBodyProof';
import { verifiedBodyMatchesBranch } from './branchBodyProof';
import {
  candidateSurfaceDecision,
  type CandidateSurfaceDecision
} from './candidateSurface';

export interface VerifiedGhostSuggestion {
  candidateId: string;
  presentationKey: string;
  targetByte: number;
  text: string;
}

export interface GhostSuggestionSelection {
  active: boolean;
  branches: readonly BranchCard[];
  verifiedBodyByRun: Readonly<Record<string, VerifiedBranchBody>>;
  dismissedCandidateIds: readonly string[];
  /**
   * Immutable presentation identities rejected by the active editor surface.
   * Candidate selection must skip them so a contextually unfaithful first
   * branch cannot starve later exact alternatives.
   */
  unpresentablePresentationKeys: readonly string[];
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

export function visibleVerifiedGhostSuggestion(
  suggestion: VerifiedGhostSuggestion | null,
  renderedPresentationKey: string
): VerifiedGhostSuggestion | null {
  return suggestion?.presentationKey === renderedPresentationKey ? suggestion : null;
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
    !body.text ||
    !/\S/u.test(body.text)
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
  const unpresentable = new Set(selection.unpresentablePresentationKeys);
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
      !verifiedBodyMatchesBranch(selection.verifiedBodyByRun[branch.run_id], branch)
    ) {
      awaitingHydration.push(branch.run_id);
      continue;
    }
    const verified = verifiedGhostSuggestion(
      branch,
      selection.verifiedBodyByRun[branch.run_id]
    );
    if (!verified) {
      exhausted.push({ candidateId, reason: 'invalid' });
      continue;
    }
    if (unpresentable.has(verified.presentationKey)) {
      exhausted.push({ candidateId, reason: 'unpresentable' });
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
