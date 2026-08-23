export interface CompletionModes {
  autocomplete: boolean;
  shuttle: boolean;
}

export function completionEngineEnabled(modes: CompletionModes): boolean {
  return modes.autocomplete || modes.shuttle;
}

export function inlineGhostHidden(modes: CompletionModes): boolean {
  return modes.shuttle || !modes.autocomplete;
}

/** Mode toggles share one engine; only the off-to-on edge mints work. */
export function completionEngineBecameEnabled(
  previous: CompletionModes,
  next: CompletionModes
): boolean {
  return !completionEngineEnabled(previous) && completionEngineEnabled(next);
}

/** A cached session dies only when the shared engine crosses from on to off. */
export function completionEngineBecameDisabled(
  previous: CompletionModes,
  next: CompletionModes
): boolean {
  return completionEngineEnabled(previous) && !completionEngineEnabled(next);
}
