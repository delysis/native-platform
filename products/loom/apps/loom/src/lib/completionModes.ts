export interface CompletionModes {
  autocomplete: boolean;
  shuttle: boolean;
}

export function completionEngineEnabled(modes: CompletionModes): boolean {
  return modes.autocomplete || modes.shuttle;
}

export function inlineGhostHidden(modes: CompletionModes): boolean {
  return !modes.autocomplete || modes.shuttle;
}
