import { verifiedBodyMatchesBranch, type VerifiedBranchBody } from './branchBodyProof';
import { candidateTextIsSurfaceable } from './candidateSurface';
import { completionTextAtBoundary } from './completionSession';
import {
  verifiedGhostSuggestion,
  type AutocompleteDisposition,
  type AutocompleteExhaustionReason
} from './ghostSuggestion';
import { visualGhostTextMayBePlainProse, visualGhostTextSafePrefix } from './ghostText';
import { sourceGhostPresentationCompatible } from './sourceGhostText';
import type { SuggestionAlternative } from './suggestionInteraction';
import type {
  BranchCard,
  ModelCapabilitySummary,
  OpenDocument
} from './types';
import type { VerseNewlineKind } from './verseCodec';

export interface InlineGhostSuggestion extends SuggestionAlternative {
  runId: string;
  targetByte: number;
  insertsOnAccept: boolean;
}

export interface InlineSuggestionState {
  branches: BranchCard[];
  /** Exact live weave command authority. Null/absent derives the newest durable family. */
  authoritativeFamilyId?: string | null;
  /** Multiple live authorities; presence (even empty) forbids durable fallback. */
  authoritativeFamilyIds?: readonly string[];
  /** Context edits require a newly admitted family, not historical manuscript matches. */
  requireExplicitFamily?: boolean;
  verifiedBodyByRun: Record<string, VerifiedBranchBody>;
  liveTextByRun: Record<string, string>;
  /** Missing sequence authority fails closed instead of using byte length as identity. */
  liveTextSequenceByRun?: Record<string, string>;
  currentModel: Pick<ModelCapabilitySummary, 'model_id'> | null | undefined;
  document: OpenDocument | null;
  suggestionsEnabled: boolean;
  promotionReady: boolean;
  dismissedCandidateIds: string[];
  unpresentableVisualKeys: string[];
  manuscriptText: string;
  sourceNewline: VerseNewlineKind | null;
}

const WEAVE_FAMILY_SIZE = 4;

export type InlineFamilyPhase =
  | { kind: 'inactive' }
  | { kind: 'stale_scope' }
  | { kind: 'awaiting_family' }
  | { kind: 'awaiting_hydration'; runIds: readonly string[] }
  | { kind: 'pending'; runIds: readonly string[] }
  | { kind: 'ready' }
  | { kind: 'dismissed'; candidateIds: readonly string[] }
  | {
      kind: 'terminal_shortfall';
      candidates: readonly {
        candidateId: string;
        reason: AutocompleteExhaustionReason;
      }[];
    };

export interface InlineFamilyEvaluation {
  candidates: InlineGhostSuggestion[];
  phase: InlineFamilyPhase;
  disposition: AutocompleteDisposition;
}

function branchBelongsToSuggestionScope(
  branch: BranchCard,
  targetByte: number,
  state: InlineSuggestionState
): boolean {
  return Boolean(
    state.document &&
    state.currentModel &&
    branch.document_id === state.document.summary.document_id &&
    branch.source_revision_id === state.document.summary.revision_id &&
    branch.model_id === state.currentModel.model_id &&
    branch.target_start_byte === targetByte &&
    branch.target_end_byte === targetByte
  );
}

function completeInlineFamilyIds(targetByte: number, state: InlineSuggestionState): Set<string> {
  const families = new Map<string, { count: number; runIds: Set<string> }>();
  for (const branch of state.branches) {
    if (!branch.weave_command_id || !branchBelongsToSuggestionScope(branch, targetByte, state)) continue;
    const family = families.get(branch.weave_command_id) ?? { count: 0, runIds: new Set() };
    family.count += 1;
    family.runIds.add(branch.run_id);
    families.set(branch.weave_command_id, family);
  }
  return new Set([...families].filter(([, family]) =>
    family.count === WEAVE_FAMILY_SIZE && family.runIds.size === WEAVE_FAMILY_SIZE
  ).map(([id]) => id));
}

/** Reopen recovers the newest complete weave, independently of branch ordering. */
export function authoritativeInlineFamilyId(
  targetByte: number,
  state: InlineSuggestionState
): string | null {
  if (state.requireExplicitFamily && !state.authoritativeFamilyId) return null;
  const complete = completeInlineFamilyIds(targetByte, state);
  if (state.authoritativeFamilyId) {
    return complete.has(state.authoritativeFamilyId) ? state.authoritativeFamilyId : null;
  }
  let newest: string | null = null;
  for (const familyId of complete) {
    if (newest === null || familyId > newest) newest = familyId;
  }
  return newest;
}

/**
 * Project immutable model output onto one exact editor boundary. Initial
 * candidate discovery and every later streaming refresh must use this same
 * function: otherwise an editor-owned separator can vanish on refresh and
 * make a valid partially consumed session look divergent.
 */
export function projectInlineCandidateText(
  targetByte: number,
  editorMode: 'visual' | 'source',
  manuscriptText: string,
  rawText: string,
  sourceNewline: VerseNewlineKind | null
): string | null {
  const candidateText = editorMode === 'visual'
    ? visualGhostTextSafePrefix(rawText)
    : rawText;
  const text = candidateText === null
    ? null
    : completionTextAtBoundary(manuscriptText, targetByte, candidateText);
  if (!text || !candidateTextIsSurfaceable(text)) return null;
  if (editorMode === 'visual') {
    return visualGhostTextMayBePlainProse(text) ? text : null;
  }
  return sourceGhostPresentationCompatible(manuscriptText, text, sourceNewline)
    ? text
    : null;
}

export function projectedInlinePresentationKey(
  rawPresentationKey: string,
  rawText: string,
  projectedText: string
): string {
  return projectedText === rawText
    ? rawPresentationKey
    : `${rawPresentationKey}:prose-prefix:${new TextEncoder().encode(projectedText).byteLength}`;
}

export function inlineSuggestionFamily(
  targetByte: number | null,
  editorMode: 'visual' | 'source',
  state: InlineSuggestionState
): InlineGhostSuggestion[] {
  return evaluateInlineSuggestionFamily(targetByte, editorMode, state).candidates;
}

/**
 * Evaluate one exact presentation surface once. Rendering and recovery consume
 * this same value so a partially presentable terminal family cannot disappear
 * while an easier branch-only predicate reports that a suggestion is available.
 */
export function evaluateInlineSuggestionFamily(
  targetByte: number | null,
  editorMode: 'visual' | 'source',
  state: InlineSuggestionState
): InlineFamilyEvaluation {
  if (
    !state.suggestionsEnabled ||
    !state.promotionReady ||
    targetByte === null ||
    !state.document ||
    !state.currentModel
  ) return {
    candidates: [],
    phase: { kind: 'inactive' },
    disposition: { kind: 'inactive' }
  };

  let familyIds: Set<string>;
  if (state.authoritativeFamilyIds !== undefined) {
    const complete = completeInlineFamilyIds(targetByte, state);
    familyIds = new Set(state.authoritativeFamilyIds.filter(id => complete.has(id)));
  } else {
    const familyId = authoritativeInlineFamilyId(targetByte, state);
    familyIds = new Set(familyId ? [familyId] : []);
  }
  if (!familyIds.size) {
    const requestedIds = state.authoritativeFamilyIds ??
      (state.authoritativeFamilyId ? [state.authoritativeFamilyId] : []);
    const matching = requestedIds.some((familyId) => state.branches.some((branch) =>
      branch.weave_command_id === familyId && branchBelongsToSuggestionScope(branch, targetByte, state)
    ));
    return {
      candidates: [],
      phase: { kind: requestedIds.length > 0 && !matching ? 'stale_scope' : 'awaiting_family' },
      disposition: { kind: 'awaiting_candidates' }
    };
  }

  const family: InlineGhostSuggestion[] = [];
  const awaitingHydration = new Set<string>();
  const pending = new Set<string>();
  const terminalShortfall: Array<{
    candidateId: string;
    reason: AutocompleteExhaustionReason;
  }> = [];
  const dismissed = new Set<string>();
  for (const familyId of familyIds) {
    const batch: InlineGhostSuggestion[] = [];
    const branches = state.branches.filter((branch) =>
      branch.weave_command_id === familyId && branchBelongsToSuggestionScope(branch, targetByte, state)
    );
    for (const branch of branches) {
      if (
        branch.selection === 'promote' ||
        branch.selection === 'reject'
      ) {
        dismissed.add(`run:${branch.run_id}`);
        continue;
      }

      const candidateId = `run:${branch.run_id}`;
      if (!['queued', 'generating', 'ready'].includes(branch.status)) {
        terminalShortfall.push({ candidateId, reason: 'invalid' });
        continue;
      }
      const body = state.verifiedBodyByRun[branch.run_id];
      // Terminal metadata can arrive before its immutable body. A usable live
      // projection may remain visible, but a partial cannot prove exhaustion
      // and spend a retry while the complete candidate is still being verified.
      if (branch.status === 'ready' && branch.output_blob_id &&
          !verifiedBodyMatchesBranch(body, branch)) awaitingHydration.add(branch.run_id);
      const verified = verifiedGhostSuggestion(branch, body);
      const liveText = state.liveTextByRun[branch.run_id];
      const liveSequence = state.liveTextSequenceByRun?.[branch.run_id];
      const hasLiveProjection = liveText !== undefined && liveSequence !== undefined;
      const rawText = verified?.text ?? (hasLiveProjection ? liveText : branch.text);
      if (!rawText) {
        if (branch.status === 'queued' || branch.status === 'generating') {
          pending.add(branch.run_id);
        } else if (!awaitingHydration.has(branch.run_id)) {
          terminalShortfall.push({ candidateId, reason: 'invalid' });
        }
        continue;
      }
      const text = projectInlineCandidateText(
        targetByte,
        editorMode,
        state.manuscriptText,
        rawText,
        state.sourceNewline
      );
      if (!text) {
        if (branch.status === 'queued' || branch.status === 'generating') {
          pending.add(branch.run_id);
        } else {
          terminalShortfall.push({ candidateId, reason: 'unpresentable' });
        }
        continue;
      }
      const rawPresentationKey = verified?.presentationKey ??
        (hasLiveProjection ? `stream:${branch.run_id}:${liveSequence}` : `branch:${branch.branch_id}`);
      const presentationKey = projectedInlinePresentationKey(rawPresentationKey, rawText, text);
      if (editorMode === 'visual') {
        if (
          state.unpresentableVisualKeys.includes(presentationKey)
        ) {
          if (branch.status === 'queued' || branch.status === 'generating') {
            pending.add(branch.run_id);
          } else {
            terminalShortfall.push({ candidateId, reason: 'unpresentable' });
          }
          continue;
        }
      }

      batch.push({
        candidateId,
        presentationKey,
        text,
        runId: branch.run_id,
        targetByte,
        insertsOnAccept: !verified || text !== rawText
      });
    }
    // A weave is a four-sample choice, not four independently arriving choices.
    // Keep incomplete streaming families private until every slot can be shown.
    if (batch.length === WEAVE_FAMILY_SIZE) {
      const visible = batch.filter((candidate) => {
        const isDismissed = state.dismissedCandidateIds.includes(candidate.candidateId);
        if (isDismissed) dismissed.add(candidate.candidateId);
        return !isDismissed;
      });
      family.push(...visible);
    }
  }
  if (family.length > 0) return {
    candidates: family,
    phase: { kind: 'ready' },
    disposition: { kind: 'available', suggestion: family[0] }
  };
  if (awaitingHydration.size > 0) return {
    candidates: [],
    phase: { kind: 'awaiting_hydration', runIds: [...awaitingHydration] },
    disposition: { kind: 'awaiting_hydration', runIds: [...awaitingHydration] }
  };
  if (pending.size > 0) return {
    candidates: [],
    phase: { kind: 'pending', runIds: [...pending] },
    disposition: { kind: 'awaiting_candidates' }
  };
  if (terminalShortfall.length > 0) return {
    candidates: [],
    phase: { kind: 'terminal_shortfall', candidates: terminalShortfall },
    disposition: { kind: 'exhausted', candidates: terminalShortfall }
  };
  if (dismissed.size > 0) return {
    candidates: [],
    phase: { kind: 'dismissed', candidateIds: [...dismissed] },
    disposition: { kind: 'exhausted', candidates: [] }
  };
  return {
    candidates: [],
    phase: { kind: 'awaiting_family' },
    disposition: { kind: 'awaiting_candidates' }
  };
}
