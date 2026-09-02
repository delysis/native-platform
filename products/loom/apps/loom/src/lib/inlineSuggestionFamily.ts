import type { VerifiedBranchBody } from './branchBodyProof';
import { candidateTextIsSurfaceable } from './candidateSurface';
import { completionTextAtBoundary } from './completionSession';
import { verifiedGhostSuggestion } from './ghostSuggestion';
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
  verifiedBodyByRun: Record<string, VerifiedBranchBody>;
  liveTextByRun: Record<string, string>;
  /** Missing sequence authority fails closed instead of using byte length as identity. */
  liveTextSequenceByRun?: Record<string, string>;
  currentModel: ModelCapabilitySummary | null | undefined;
  document: OpenDocument | null;
  suggestionsEnabled: boolean;
  promotionReady: boolean;
  dismissedCandidateIds: string[];
  unpresentableVisualKeys: string[];
  manuscriptText: string;
  sourceNewline: VerseNewlineKind | null;
}

const WEAVE_FAMILY_SIZE = 4;

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

/**
 * Resolve one explicit four-run weave authority. ULIDs sort chronologically,
 * so reopen recovery chooses the greatest complete family ID without relying
 * on renderer timestamps, branch adjacency, or array slicing.
 */
export function authoritativeInlineFamilyId(
  targetByte: number,
  state: InlineSuggestionState
): string | null {
  const families = new Map<string, { count: number; runIds: Set<string> }>();
  for (const branch of state.branches) {
    if (!branch.weave_command_id || !branchBelongsToSuggestionScope(branch, targetByte, state)) {
      continue;
    }
    const family = families.get(branch.weave_command_id) ?? { count: 0, runIds: new Set() };
    family.count += 1;
    family.runIds.add(branch.run_id);
    families.set(branch.weave_command_id, family);
  }
  const complete = (familyId: string): boolean => {
    const family = families.get(familyId);
    return Boolean(
      family &&
      family.count === WEAVE_FAMILY_SIZE &&
      family.runIds.size === WEAVE_FAMILY_SIZE
    );
  };
  if (state.authoritativeFamilyId) {
    return complete(state.authoritativeFamilyId) ? state.authoritativeFamilyId : null;
  }
  let newest: string | null = null;
  for (const familyId of families.keys()) {
    if (complete(familyId) && (newest === null || familyId > newest)) newest = familyId;
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
  if (
    !state.suggestionsEnabled ||
    !state.promotionReady ||
    targetByte === null ||
    !state.document ||
    !state.currentModel
  ) return [];

  const familyId = authoritativeInlineFamilyId(targetByte, state);
  if (!familyId) return [];

  const family: InlineGhostSuggestion[] = [];
  for (const branch of state.branches) {
    if (
      branch.weave_command_id !== familyId ||
      !branchBelongsToSuggestionScope(branch, targetByte, state) ||
      branch.selection === 'promote' ||
      branch.selection === 'reject' ||
      !['queued', 'generating', 'ready'].includes(branch.status)
    ) continue;

    const candidateId = `run:${branch.run_id}`;
    if (state.dismissedCandidateIds.includes(candidateId)) continue;
    const verified = verifiedGhostSuggestion(branch, state.verifiedBodyByRun[branch.run_id]);
    const liveText = state.liveTextByRun[branch.run_id];
    const liveSequence = state.liveTextSequenceByRun?.[branch.run_id];
    const hasLiveProjection = liveText !== undefined && liveSequence !== undefined;
    const rawText = verified?.text ?? (hasLiveProjection ? liveText : branch.text);
    const text = projectInlineCandidateText(
      targetByte,
      editorMode,
      state.manuscriptText,
      rawText,
      state.sourceNewline
    );
    if (!text) continue;
    const rawPresentationKey = verified?.presentationKey ??
      (hasLiveProjection ? `stream:${branch.run_id}:${liveSequence}` : `branch:${branch.branch_id}`);
    const presentationKey = projectedInlinePresentationKey(rawPresentationKey, rawText, text);
    if (editorMode === 'visual') {
      if (
        state.unpresentableVisualKeys.includes(presentationKey)
      ) continue;
    }

    family.push({
      candidateId,
      presentationKey,
      text,
      runId: branch.run_id,
      targetByte,
      insertsOnAccept: !verified || text !== rawText
    });
  }
  return family;
}
