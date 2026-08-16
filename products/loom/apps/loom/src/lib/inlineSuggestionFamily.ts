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
  verifiedBodyByRun: Record<string, VerifiedBranchBody>;
  liveTextByRun: Record<string, string>;
  currentModel: ModelCapabilitySummary | null | undefined;
  document: OpenDocument | null;
  suggestionsEnabled: boolean;
  promotionReady: boolean;
  dismissedCandidateIds: string[];
  unpresentableVisualKeys: string[];
  manuscriptText: string;
  sourceNewline: VerseNewlineKind | null;
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

  const encoder = new TextEncoder();
  const family: InlineGhostSuggestion[] = [];
  for (const branch of state.branches) {
    if (
      branch.source_revision_id !== state.document.summary.revision_id ||
      branch.model_id !== state.currentModel.model_id ||
      branch.target_start_byte !== targetByte ||
      branch.target_end_byte !== targetByte ||
      branch.selection === 'promote' ||
      branch.selection === 'reject' ||
      !['queued', 'generating', 'ready'].includes(branch.status)
    ) continue;

    const candidateId = `run:${branch.run_id}`;
    if (state.dismissedCandidateIds.includes(candidateId)) continue;
    const verified = verifiedGhostSuggestion(branch, state.verifiedBodyByRun[branch.run_id]);
    const rawText = verified?.text ?? state.liveTextByRun[branch.run_id] ?? branch.text;
    const text = projectInlineCandidateText(
      targetByte,
      editorMode,
      state.manuscriptText,
      rawText,
      state.sourceNewline
    );
    if (!text) continue;
    const rawPresentationKey = verified?.presentationKey ??
      `stream:${branch.run_id}:${encoder.encode(rawText).byteLength}`;
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
