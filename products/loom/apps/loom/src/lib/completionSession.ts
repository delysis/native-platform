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
  return { contextKey, candidates: snapshots, selectedRunId, acceptedChunks: [] };
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
    session: { ...session, acceptedChunks: [...session.acceptedChunks, text] }
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

export function completionPresentation(session: CompletionSession): CompletionCandidate | null {
  const selected = selectedCompletionCandidate(session);
  const remaining = remainingCompletionText(session);
  if (!selected || remaining === null || !remaining) return null;
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

export function completionShouldRequestNextBatch(
  session: CompletionSession,
  editorMutationPending: boolean,
  selectedCandidateReady: boolean
): boolean {
  return !editorMutationPending &&
    selectedCandidateReady &&
    remainingCompletionText(session) === '';
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
