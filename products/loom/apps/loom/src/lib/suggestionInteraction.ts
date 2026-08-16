export interface SuggestionAlternative {
  candidateId: string;
  presentationKey: string;
  text: string;
}

/**
 * Return one writer-visible step from a continuation. Leading whitespace stays
 * attached to the word that follows it, while punctuation immediately after a
 * word travels with that word. This keeps repeated partial acceptance from
 * producing orphan spaces or punctuation.
 */
export function nextSuggestionWord(text: string): string | null {
  if (!text || !/\S/u.test(text)) return null;
  const match = text.match(
    /^\s*(?:[^\s\p{L}\p{N}\p{M}_]*[\p{L}\p{N}\p{M}_]+(?:['’\-][\p{L}\p{N}\p{M}_]+)*|[^\s\p{L}\p{N}\p{M}_]+)(?:[^\s\p{L}\p{N}\p{M}_]*)(?:\s+|$)/u
  );
  if (match?.[0]) return match[0];

  // A live stream may currently end halfway through a word. It is still safe
  // to advance that visible scalar sequence when the writer explicitly asks.
  const visible = text.match(/^\s*\S+/u)?.[0] ?? null;
  return visible && /\S/u.test(visible) ? visible : null;
}

export function cycleSuggestionIndex(length: number, current: number, offset: number): number {
  if (!Number.isSafeInteger(length) || length <= 0) return -1;
  const normalizedCurrent = Number.isSafeInteger(current) && current >= 0 && current < length
    ? current
    : 0;
  return (normalizedCurrent + offset % length + length) % length;
}
