import {
  acceptedCompletionText,
  advanceCompletionExhaustionLatch,
  completionPresentation,
  completionSessionMatchesPresentation,
  consumeCompletionText,
  cycleCompletionSession,
  insertAtUtf8Boundary,
  removeBeforeUtf8Boundary,
  selectedCompletionCandidate,
  startCompletionSession,
  synchronizeCompletionCandidates,
  unconsumeCompletionWord,
  updateCompletionCandidate,
  type CompletionSession
} from './completionSession';
import {
  armCompletionGeneration,
  bindCompletionGenerationAnchor,
  disarmCompletionGeneration,
  type CompletionGenerationIntent,
  type CompletionGenerationTrigger
} from './completionGenerationIntent';
import type { InlineGhostSuggestion } from './inlineSuggestionFamily';
import {
  cycleSuggestionIndex,
  type CompletionInsertionAction,
  type SuggestionAlternative
} from './suggestionInteraction';
import type { VerseNewlineKind } from './verseCodec';

export interface AutocompleteRetryTicket {
  projectId: string;
  sessionId: string;
  documentId: string;
  sourceRevisionId: string;
  visibleBlobId: string;
  documentEpoch: number;
  editVersion: number;
  intentEpoch: number;
  mode: 'visual' | 'source';
  targetByte: number;
  modelId: string;
  sourceNewline: VerseNewlineKind | null;
  waitsRemaining: number;
}

export type CompletionSchedule =
  | { kind: 'edit_pause'; editVersion: number }
  | { kind: 'exhausted_retry'; ticket: AutocompleteRetryTicket };

export interface CompletionActionWitness {
  sequence: number;
  kind: CompletionInsertionAction;
  context_key: string;
  run_id: string;
  candidate_id: string;
  presentation_key: string;
  inserted_utf8_bytes: number;
  accepted_utf8_bytes: number;
}

/**
 * Renderer-private completion authority. App.svelte owns one value and may
 * replace it only with the result of a pure transition below. Timers, editor
 * mutation, announcements, cancellation IPC, and weave IPC remain effects
 * interpreted by App.svelte; they are never hidden inside this state.
 */
export interface CompletionControllerState {
  session: CompletionSession | null;
  activeRunId: string | null;
  pendingText: string | null;
  handledExhaustionKey: string;
  actionSequence: number;
  lastAction: CompletionActionWitness | null;
  generationIntent: CompletionGenerationIntent | null;
  navigationPending: boolean;
  intentEpoch: number;
  dismissedCandidateIds: string[];
  unpresentableVisualKeys: string[];
  scheduled: CompletionSchedule | null;
}

export type CompletionControllerEffect =
  | { kind: 'announce'; message: string }
  | { kind: 'cancel_active_branches' }
  | {
      kind: 'schedule_generation';
      editVersion: number;
      delayMs: number;
      trigger: CompletionGenerationTrigger;
    };

export interface CompletionControllerTransition {
  state: CompletionControllerState;
  effects: CompletionControllerEffect[];
}

export interface CompletionControllerView {
  boundSession: CompletionSession | null;
  activeFamily: InlineGhostSuggestion[];
  selected: InlineGhostSuggestion | null;
  alternatives: SuggestionAlternative[];
  unconsumeText: string;
  witnessSelected: InlineGhostSuggestion | null;
}

export function initialCompletionControllerState(): CompletionControllerState {
  return {
    session: null,
    activeRunId: null,
    pendingText: null,
    handledExhaustionKey: '',
    actionSequence: 0,
    lastAction: null,
    generationIntent: null,
    navigationPending: false,
    intentEpoch: 0,
    dismissedCandidateIds: [],
    unpresentableVisualKeys: [],
    scheduled: null
  };
}

export function clearCompletionSession(
  state: CompletionControllerState
): CompletionControllerState {
  if (state.session === null && state.pendingText === null) return state;
  return { ...state, session: null, pendingText: null };
}

export function resetCompletionSurface(
  state: CompletionControllerState
): CompletionControllerState {
  const cleared = clearCompletionSession(state);
  if (
    cleared.lastAction === null &&
    cleared.unpresentableVisualKeys.length === 0
  ) return cleared;
  return {
    ...cleared,
    lastAction: null,
    unpresentableVisualKeys: []
  };
}

export function resetCompletionDiscovery(
  state: CompletionControllerState
): CompletionControllerState {
  if (
    state.session === null &&
    state.pendingText === null &&
    state.dismissedCandidateIds.length === 0 &&
    state.unpresentableVisualKeys.length === 0
  ) return state;
  return {
    ...state,
    session: null,
    pendingText: null,
    dismissedCandidateIds: [],
    unpresentableVisualKeys: []
  };
}

export function clearUnpresentableVisualKeys(
  state: CompletionControllerState
): CompletionControllerState {
  return state.unpresentableVisualKeys.length === 0
    ? state
    : { ...state, unpresentableVisualKeys: [] };
}

export function setDismissedCompletionCandidates(
  state: CompletionControllerState,
  candidateIds: readonly string[]
): CompletionControllerState {
  const dismissedCandidateIds = [...candidateIds];
  const unchanged = dismissedCandidateIds.length === state.dismissedCandidateIds.length &&
    dismissedCandidateIds.every(
      (candidateId, index) => candidateId === state.dismissedCandidateIds[index]
    );
  return unchanged ? state : { ...state, dismissedCandidateIds };
}

export function reconcileCompletionController(
  state: CompletionControllerState,
  contextKey: string,
  family: readonly InlineGhostSuggestion[]
): CompletionControllerState {
  let session = state.session;
  let pendingText = state.pendingText;
  if (session?.contextKey !== contextKey) {
    session = null;
    pendingText = null;
  }
  if (contextKey) {
    if (!session && family.length > 0) {
      session = startCompletionSession(contextKey, family, family[0].runId);
    } else if (session) {
      session = synchronizeCompletionCandidates(session, family);
      if (!session) pendingText = null;
    }
  }
  const activeFamily = completionActiveFamily(session, pendingText, family);
  const activeRunId = activeFamily.length > 0 &&
      !activeFamily.some((candidate) => candidate.runId === state.activeRunId)
    ? activeFamily[0].runId
    : state.activeRunId;
  if (
    session === state.session &&
    pendingText === state.pendingText &&
    activeRunId === state.activeRunId
  ) return state;
  return { ...state, session, pendingText, activeRunId };
}

export function completionControllerView(
  state: CompletionControllerState,
  contextKey: string,
  baseFamily: readonly InlineGhostSuggestion[]
): CompletionControllerView {
  const boundSession = state.session?.contextKey === contextKey ? state.session : null;
  const activeFamily = completionActiveFamily(boundSession, state.pendingText, baseFamily);
  const selected = activeFamily.find((candidate) => candidate.runId === state.activeRunId) ??
    activeFamily[0] ?? null;
  const witnessSelected = boundSession
    ? selectedCompletionCandidate(boundSession) as InlineGhostSuggestion | null
    : null;
  return {
    boundSession,
    activeFamily,
    selected,
    alternatives: activeFamily.map((candidate) => ({
      candidateId: candidate.candidateId,
      presentationKey: candidate.presentationKey,
      text: candidate.text,
      runId: candidate.runId
    })),
    unconsumeText: boundSession?.acceptedChunks.at(-1) ?? '',
    witnessSelected
  };
}

function completionActiveFamily(
  session: CompletionSession | null,
  pendingText: string | null,
  baseFamily: readonly InlineGhostSuggestion[]
): InlineGhostSuggestion[] {
  if (pendingText !== null) return [];
  if (!session) return [...baseFamily];
  if (session.acceptedChunks.length === 0) {
    return session.candidates as InlineGhostSuggestion[];
  }
  const presentation = completionPresentation(session) as InlineGhostSuggestion | null;
  return presentation ? [presentation] : [];
}

export function refreshCompletionCandidate(
  state: CompletionControllerState,
  expected: CompletionSession,
  runId: string,
  text: string,
  presentationKey: string
): CompletionControllerState {
  if (state.session !== expected) return state;
  const session = updateCompletionCandidate(expected, runId, text, presentationKey);
  if (session === state.session) return state;
  return {
    ...state,
    session,
    pendingText: session ? state.pendingText : null
  };
}

export interface CompletionInsertionInput {
  contextKey: string;
  family: readonly InlineGhostSuggestion[];
  eligible: InlineGhostSuggestion | null;
  candidateId: string;
  presentationKey: string;
  text: string;
  action: CompletionInsertionAction;
  manuscriptText: string;
  promotionReady: boolean;
}

export interface CompletionAuthorization {
  state: CompletionControllerState;
  authorized: boolean;
}

export function authorizeCompletionInsertion(
  state: CompletionControllerState,
  input: CompletionInsertionInput
): CompletionAuthorization {
  const eligible = input.eligible;
  if (
    !eligible ||
    eligible.candidateId !== input.candidateId ||
    eligible.presentationKey !== input.presentationKey ||
    !input.text ||
    !eligible.text.startsWith(input.text) ||
    state.pendingText !== null ||
    (!state.session && !input.promotionReady)
  ) return { state, authorized: false };
  const matchingSession = state.session && completionSessionMatchesPresentation(
    state.session,
    input.contextKey,
    eligible
  ) ? state.session : null;
  const baseState = state.session && !matchingSession
    ? { ...state, session: null, pendingText: null }
    : state;
  const initial = matchingSession ?? startCompletionSession(
    input.contextKey,
    input.family,
    eligible.runId
  );
  if (!initial) return { state: baseState, authorized: false };
  const consumed = consumeCompletionText(initial, input.text);
  if (!consumed) return { state, authorized: false };
  const expected = insertAtUtf8Boundary(
    input.manuscriptText,
    eligible.targetByte,
    input.text
  );
  if (expected === null) return { state, authorized: false };
  const actionSequence = baseState.actionSequence + 1;
  const encoder = new TextEncoder();
  return {
    authorized: true,
    state: {
      ...baseState,
      session: consumed.session,
      pendingText: expected,
      actionSequence,
      lastAction: {
        sequence: actionSequence,
        kind: input.action,
        context_key: input.contextKey,
        run_id: eligible.runId,
        candidate_id: eligible.candidateId,
        presentation_key: eligible.presentationKey,
        inserted_utf8_bytes: encoder.encode(input.text).byteLength,
        accepted_utf8_bytes: encoder.encode(
          acceptedCompletionText(consumed.session)
        ).byteLength
      }
    }
  };
}

export interface CompletionUnconsumeInput {
  eligible: InlineGhostSuggestion | null;
  candidateId: string;
  presentationKey: string;
  text: string;
  manuscriptText: string;
}

export function authorizeCompletionUnconsume(
  state: CompletionControllerState,
  input: CompletionUnconsumeInput
): CompletionAuthorization {
  const eligible = input.eligible;
  if (
    !state.session ||
    !eligible ||
    eligible.candidateId !== input.candidateId ||
    eligible.presentationKey !== input.presentationKey ||
    state.pendingText !== null
  ) return { state, authorized: false };
  const step = unconsumeCompletionWord(state.session);
  if (!step || step.text !== input.text) return { state, authorized: false };
  const expected = removeBeforeUtf8Boundary(
    input.manuscriptText,
    eligible.targetByte,
    input.text
  );
  if (expected === null) return { state, authorized: false };
  return {
    authorized: true,
    state: { ...state, session: step.session, pendingText: expected }
  };
}

export function cycleCompletion(
  state: CompletionControllerState,
  family: readonly InlineGhostSuggestion[],
  offset: number
): CompletionControllerTransition {
  if (state.session) {
    const session = cycleCompletionSession(state.session, offset);
    if (session === state.session) return { state, effects: [] };
    const index = session.candidates.findIndex(
      (candidate) => candidate.runId === session.selectedRunId
    );
    return {
      state: { ...state, session, activeRunId: session.selectedRunId },
      effects: [{
        kind: 'announce',
        message: `Suggestion ${index + 1} of ${session.candidates.length}`
      }]
    };
  }
  if (family.length < 2) return { state, effects: [] };
  const current = family.findIndex((candidate) => candidate.runId === state.activeRunId);
  const next = cycleSuggestionIndex(family.length, current, offset);
  if (next < 0) return { state, effects: [] };
  return {
    state: { ...state, activeRunId: family[next].runId },
    effects: [{
      kind: 'announce',
      message: `Suggestion ${next + 1} of ${family.length}`
    }]
  };
}

export function dismissCompletion(
  state: CompletionControllerState,
  contextKey: string,
  eligible: InlineGhostSuggestion | null,
  candidateId: string,
  presentationKey: string
): CompletionAuthorization {
  if (
    !eligible ||
    eligible.candidateId !== candidateId ||
    eligible.presentationKey !== presentationKey
  ) return { state, authorized: false };
  const session = state.session && completionSessionMatchesPresentation(
    state.session,
    contextKey,
    eligible
  ) ? null : state.session;
  const dismissedCandidateIds = state.dismissedCandidateIds.includes(candidateId)
    ? state.dismissedCandidateIds
    : [...state.dismissedCandidateIds, candidateId];
  return {
    authorized: true,
    state: {
      ...state,
      session,
      pendingText: session ? state.pendingText : null,
      dismissedCandidateIds
    }
  };
}

export interface RejectVisualPresentationInput {
  mode: 'visual' | 'source';
  eligible: InlineGhostSuggestion | null;
  candidateId: string;
  presentationKey: string;
  surfaceKey: string;
  currentSurfaceKey: string;
  anchorByte: number;
}

export function rejectVisualPresentation(
  state: CompletionControllerState,
  input: RejectVisualPresentationInput
): CompletionControllerState {
  if (
    input.mode !== 'visual' ||
    input.eligible?.candidateId !== input.candidateId ||
    input.eligible.presentationKey !== input.presentationKey ||
    input.eligible.targetByte !== input.anchorByte ||
    input.surfaceKey !== input.currentSurfaceKey ||
    state.unpresentableVisualKeys.includes(input.presentationKey)
  ) return state;
  return {
    ...state,
    unpresentableVisualKeys: [
      ...state.unpresentableVisualKeys,
      input.presentationKey
    ].slice(-64)
  };
}

export interface TextMutationDecision {
  state: CompletionControllerState;
  completionOwned: boolean;
  cancelActiveBranches: boolean;
}

export function observeTextMutation(
  state: CompletionControllerState,
  text: string,
  currentText: string,
  visualMutationWasInvalidated: boolean
): TextMutationDecision {
  const completionOwned = state.pendingText === text;
  let next: CompletionControllerState = { ...state, pendingText: null };
  if (text === currentText) {
    return { state: next, completionOwned, cancelActiveBranches: false };
  }
  if (!completionOwned && !visualMutationWasInvalidated) {
    next = { ...next, intentEpoch: next.intentEpoch + 1 };
  }
  if (!completionOwned) {
    next = {
      ...next,
      session: null,
      dismissedCandidateIds: [],
      unpresentableVisualKeys: []
    };
  }
  return { state: next, completionOwned, cancelActiveBranches: !completionOwned };
}

export function invalidateVisualMutation(
  state: CompletionControllerState
): CompletionControllerTransition {
  return {
    state: {
      ...state,
      session: null,
      intentEpoch: state.intentEpoch + 1,
      dismissedCandidateIds: [],
      unpresentableVisualKeys: []
    },
    effects: [{ kind: 'cancel_active_branches' }]
  };
}

export function completionActivityExists(
  state: CompletionControllerState,
  external: {
    weaveStarting: boolean;
    activeBranchCount: number;
    selected: InlineGhostSuggestion | null;
  }
): boolean {
  return Boolean(
    state.generationIntent ||
    state.session ||
    state.pendingText !== null ||
    state.scheduled ||
    external.weaveStarting ||
    external.activeBranchCount > 0 ||
    external.selected
  );
}

export function invalidateCompletionNavigation(
  state: CompletionControllerState,
  mode: 'visual' | 'source',
  active: boolean,
  activeBranchCount: number
): CompletionControllerTransition {
  const navigationPending = mode === 'visual';
  if (!active) {
    return navigationPending === state.navigationPending
      ? { state, effects: [] }
      : { state: { ...state, navigationPending }, effects: [] };
  }
  return {
    state: {
      ...state,
      session: null,
      pendingText: null,
      dismissedCandidateIds: [],
      unpresentableVisualKeys: [],
      generationIntent: disarmCompletionGeneration(),
      scheduled: null,
      navigationPending,
      intentEpoch: state.intentEpoch + 1
    },
    effects: activeBranchCount > 0 ? [{ kind: 'cancel_active_branches' }] : []
  };
}

export function settleCompletionNavigation(
  state: CompletionControllerState,
  exactAnchorAvailable: boolean
): { state: CompletionControllerState; scheduleFresh: boolean } {
  if (!state.navigationPending || !exactAnchorAvailable) {
    return { state, scheduleFresh: false };
  }
  return {
    state: { ...state, navigationPending: false },
    scheduleFresh: true
  };
}

export function bindCompletionAnchor(
  state: CompletionControllerState,
  contextKey: string,
  editVersion: number,
  anchorByte: number
): CompletionControllerState {
  const generationIntent = bindCompletionGenerationAnchor(
    state.generationIntent,
    contextKey,
    editVersion,
    anchorByte
  );
  return generationIntent === state.generationIntent
    ? state
    : { ...state, generationIntent };
}

export function armCompletionScheduleIntent(
  state: CompletionControllerState,
  contextKey: string,
  editVersion: number,
  trigger: CompletionGenerationTrigger,
  anchorByte: number | null
): CompletionControllerState {
  const generationIntent = armCompletionGeneration(
    contextKey,
    editVersion,
    trigger,
    anchorByte
  );
  return generationIntent === state.generationIntent
    ? state
    : { ...state, generationIntent };
}

export function setCompletionSchedule(
  state: CompletionControllerState,
  scheduled: CompletionSchedule | null
): CompletionControllerState {
  return scheduled === state.scheduled ? state : { ...state, scheduled };
}

export function cancelCompletionSchedule(
  state: CompletionControllerState
): CompletionControllerState {
  return {
    ...state,
    scheduled: null,
    generationIntent: disarmCompletionGeneration(),
    intentEpoch: state.intentEpoch + 1
  };
}

export function completionExhausted(
  state: CompletionControllerState,
  key: string,
  editVersion: number,
  delayMs: number
): CompletionControllerTransition {
  const edge = advanceCompletionExhaustionLatch(state.handledExhaustionKey, key);
  const next = edge.handledKey === state.handledExhaustionKey
    ? state
    : { ...state, handledExhaustionKey: edge.handledKey };
  return {
    state: next,
    effects: edge.shouldSchedule ? [{
      kind: 'schedule_generation',
      editVersion,
      delayMs,
      trigger: 'candidate_exhausted'
    }] : []
  };
}

export function shuttleScheduleKey(
  enabled: boolean,
  windowFocused: boolean,
  candidate: InlineGhostSuggestion | null,
  acceptedChunkCount: number,
  editVersion: number,
  mode: 'visual' | 'source'
): string {
  return enabled && windowFocused && candidate
    ? `${candidate.candidateId}:${acceptedChunkCount}:${editVersion}:${mode}`
    : '';
}
