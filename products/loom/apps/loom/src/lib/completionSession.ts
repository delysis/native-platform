import { nextSuggestionWord } from './suggestionInteraction';

export interface CompletionCandidate {
  candidateId: string;
  presentationKey: string;
  text: string;
  runId: string;
  targetByte: number;
  insertsOnAccept: boolean;
}

export interface CompletionSession {
  contextKey: string;
  candidates: CompletionCandidate[];
  selectedRunId: string;
  acceptedChunks: string[];
  /** Sticky authority: once insertion starts, base-family churn cannot replace this snapshot. */
  authorityFrozen: boolean;
}

export interface CompletionStep {
  session: CompletionSession;
  text: string;
}

export function completionSessionContextKey(
  projectSessionId: string,
  documentId: string,
  documentEpoch: number,
  mode: 'visual' | 'source'
): string {
  if (
    !projectSessionId ||
    !documentId ||
    !Number.isSafeInteger(documentEpoch) ||
    documentEpoch < 0
  ) {
    return '';
  }
  return `${projectSessionId}:${documentId}:${documentEpoch}:${mode}`;
}

export function startCompletionSession(
  contextKey: string,
  candidates: readonly CompletionCandidate[],
  selectedRunId: string
): CompletionSession | null {
  const snapshots = candidates.map((candidate) => ({ ...candidate }));
  if (!contextKey || snapshots.length === 0 || !snapshots.some((item) => item.runId === selectedRunId)) {
    return null;
  }
  return {
    contextKey,
    candidates: snapshots,
    selectedRunId,
    acceptedChunks: [],
    authorityFrozen: false
  };
}

export function selectedCompletionCandidate(session: CompletionSession): CompletionCandidate | null {
  return session.candidates.find((candidate) => candidate.runId === session.selectedRunId) ?? null;
}

export function acceptedCompletionText(session: CompletionSession): string {
  return session.acceptedChunks.join('');
}

export function remainingCompletionText(session: CompletionSession): string | null {
  const candidate = selectedCompletionCandidate(session);
  if (!candidate) return null;
  const accepted = acceptedCompletionText(session);
  return candidate.text.startsWith(accepted) ? candidate.text.slice(accepted.length) : null;
}

export function consumeCompletionText(
  session: CompletionSession,
  text: string
): CompletionStep | null {
  const remaining = remainingCompletionText(session);
  if (!text || remaining === null || !remaining.startsWith(text)) return null;
  return {
    text,
    session: {
      ...session,
      acceptedChunks: [...session.acceptedChunks, text],
      authorityFrozen: true
    }
  };
}

export function consumeCompletionWord(session: CompletionSession): CompletionStep | null {
  const remaining = remainingCompletionText(session);
  if (remaining === null) return null;
  const word = nextSuggestionWord(remaining);
  return word ? consumeCompletionText(session, word) : null;
}

export function consumeCompletionRemainder(session: CompletionSession): CompletionStep | null {
  const remaining = remainingCompletionText(session);
  return remaining ? consumeCompletionText(session, remaining) : null;
}

export function unconsumeCompletionWord(session: CompletionSession): CompletionStep | null {
  const text = session.acceptedChunks.at(-1);
  if (!text) return null;
  return {
    text,
    session: { ...session, acceptedChunks: session.acceptedChunks.slice(0, -1) }
  };
}

export function cycleCompletionSession(
  session: CompletionSession,
  offset: number
): CompletionSession {
  if (session.acceptedChunks.length > 0 || session.candidates.length < 2) return session;
  const current = session.candidates.findIndex((candidate) => candidate.runId === session.selectedRunId);
  const normalized = current < 0 ? 0 : current;
  const next = (normalized + offset % session.candidates.length + session.candidates.length) %
    session.candidates.length;
  return next === normalized ? session : { ...session, selectedRunId: session.candidates[next].runId };
}

export function updateCompletionCandidate(
  session: CompletionSession,
  runId: string,
  text: string,
  presentationKey: string
): CompletionSession | null {
  // A consumed candidate is an immutable authorization snapshot. Late stream
  // chunks must not rewrite either the reversible text or its presentation
  // identity, including after every accepted chunk has been unconsumed.
  if (session.authorityFrozen) return session;
  const accepted = acceptedCompletionText(session);
  if (runId === session.selectedRunId && !text.startsWith(accepted)) return null;
  let changed = false;
  const candidates = session.candidates.map((candidate) => {
    if (candidate.runId !== runId || (candidate.text === text && candidate.presentationKey === presentationKey)) {
      return candidate;
    }
    changed = true;
    return { ...candidate, text, presentationKey };
  });
  return changed ? { ...session, candidates } : session;
}

/**
 * Grow or refresh an unconsumed family without changing its selected identity.
 * Once any text has been consumed, the original immutable snapshot remains the
 * only reversible continuation authority.
 */
export function synchronizeCompletionCandidates(
  session: CompletionSession,
  candidates: readonly CompletionCandidate[]
): CompletionSession | null {
  // An empty authoritative family means an unconsumed presentation was
  // dismissed or became ineligible. Only an already-authorized insertion may
  // retain its immutable session long enough to reverse or continue it.
  if (session.authorityFrozen) {
    const selected = selectedCompletionCandidate(session);
    const remaining = remainingCompletionText(session);
    const acceptedBytes = new TextEncoder().encode(acceptedCompletionText(session)).byteLength;
    const nextTarget = selected ? selected.targetByte + acceptedBytes : -1;
    const oldRunIds = new Set(session.candidates.map((candidate) => candidate.runId));
    const oldCandidateIds = new Set(session.candidates.map((candidate) => candidate.candidateId));
    const oldPresentationKeys = new Set(
      session.candidates.map((candidate) => candidate.presentationKey)
    );
    const freshExhaustedFamily = remaining === '' &&
      candidates.length > 0 &&
      candidates.every((candidate) =>
        candidate.targetByte === nextTarget &&
        !oldRunIds.has(candidate.runId) &&
        !oldCandidateIds.has(candidate.candidateId) &&
        !oldPresentationKeys.has(candidate.presentationKey)
      );
    return freshExhaustedFamily
      ? startCompletionSession(session.contextKey, candidates, candidates[0].runId)
      : session;
  }
  if (candidates.length === 0) return null;
  const snapshots = candidates.map((candidate) => ({ ...candidate }));
  const selectedRunId = snapshots.some((candidate) => candidate.runId === session.selectedRunId)
    ? session.selectedRunId
    : snapshots[0].runId;
  const unchanged = selectedRunId === session.selectedRunId &&
    snapshots.length === session.candidates.length &&
    snapshots.every((candidate, index) => {
      const previous = session.candidates[index];
      return previous &&
        previous.candidateId === candidate.candidateId &&
        previous.presentationKey === candidate.presentationKey &&
        previous.text === candidate.text &&
        previous.runId === candidate.runId &&
        previous.targetByte === candidate.targetByte &&
        previous.insertsOnAccept === candidate.insertsOnAccept;
    });
  return unchanged ? session : { ...session, candidates: snapshots, selectedRunId };
}

export function completionPresentation(session: CompletionSession): CompletionCandidate | null {
  const selected = selectedCompletionCandidate(session);
  const remaining = remainingCompletionText(session);
  if (!selected || remaining === null) return null;
  const accepted = acceptedCompletionText(session);
  const acceptedBytes = new TextEncoder().encode(accepted).byteLength;
  return {
    ...selected,
    presentationKey: `${selected.presentationKey}:session:${acceptedBytes}`,
    text: remaining,
    targetByte: selected.targetByte + acceptedBytes,
    insertsOnAccept: true
  };
}

/**
 * Prove that a cached session belongs to the exact surface presentation asking
 * to consume it. A non-null session from another document, mode, candidate, or
 * accepted offset must never authorize an insertion.
 */
export function completionSessionMatchesPresentation(
  session: CompletionSession,
  contextKey: string,
  candidate: CompletionCandidate
): boolean {
  if (!contextKey || session.contextKey !== contextKey) return false;
  const expected = session.acceptedChunks.length === 0
    ? selectedCompletionCandidate(session)
    : completionPresentation(session);
  return Boolean(
    expected &&
    expected.candidateId === candidate.candidateId &&
    expected.presentationKey === candidate.presentationKey &&
    expected.runId === candidate.runId &&
    expected.targetByte === candidate.targetByte &&
    expected.text === candidate.text &&
    expected.insertsOnAccept === candidate.insertsOnAccept
  );
}

export function completionShouldRequestNextBatch(
  session: CompletionSession,
  editorMutationPending: boolean,
  selectedCandidateReady: boolean
): boolean {
  return !editorMutationPending &&
    selectedCandidateReady &&
    remainingCompletionText(session) === '';
}

export interface CompletionExhaustionLatch {
  handledKey: string;
  shouldSchedule: boolean;
}

/** Edge-trigger exhaustion, resetting authority after rollback or handoff. */
export function advanceCompletionExhaustionLatch(
  handledKey: string,
  currentKey: string
): CompletionExhaustionLatch {
  if (!currentKey) return { handledKey: '', shouldSchedule: false };
  if (currentKey === handledKey) return { handledKey, shouldSchedule: false };
  return { handledKey: currentKey, shouldSchedule: true };
}

export function utf8ByteBoundaryToStringIndex(text: string, targetBytes: number): number | null {
  if (!Number.isSafeInteger(targetBytes) || targetBytes < 0) return null;
  const encoder = new TextEncoder();
  let bytes = 0;
  for (let index = 0; index <= text.length;) {
    if (bytes === targetBytes) return index;
    if (index === text.length) break;
    const codePoint = text.codePointAt(index);
    if (codePoint === undefined) return null;
    const character = String.fromCodePoint(codePoint);
    bytes += encoder.encode(character).byteLength;
    if (bytes > targetBytes) return null;
    index += character.length;
  }
  return bytes === targetBytes ? text.length : null;
}

/**
 * Project a standalone chat response onto a manuscript insertion boundary.
 * Chat templates conventionally begin assistant text without leading
 * whitespace, while an editor completion often needs one. The generated
 * candidate remains immutable; this pure presentation projection makes the
 * editor-owned separator explicit and reversible.
 */
export function completionTextAtBoundary(
  manuscript: string,
  targetBytes: number,
  candidate: string
): string | null {
  const index = utf8ByteBoundaryToStringIndex(manuscript, targetBytes);
  if (index === null) return null;
  const previous = Array.from(manuscript.slice(0, index)).at(-1);
  const first = Array.from(candidate)[0];
  if (!previous || !first || /\s/u.test(previous) || /\s/u.test(first)) return candidate;
  const closesProse = /[\p{L}\p{N}\p{Pe}.,!?;:'’”"]/u.test(previous);
  const startsProse = /[\p{L}\p{N}\p{Ps}'‘“"]/u.test(first);
  return closesProse && startsProse ? ` ${candidate}` : candidate;
}

export function insertAtUtf8Boundary(text: string, targetBytes: number, insertion: string): string | null {
  const index = utf8ByteBoundaryToStringIndex(text, targetBytes);
  return index === null ? null : `${text.slice(0, index)}${insertion}${text.slice(index)}`;
}

export function removeBeforeUtf8Boundary(text: string, targetBytes: number, removal: string): string | null {
  const index = utf8ByteBoundaryToStringIndex(text, targetBytes);
  if (index === null || !text.slice(0, index).endsWith(removal)) return null;
  return `${text.slice(0, index - removal.length)}${text.slice(index)}`;
}
