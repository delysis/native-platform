export type CompletionGenerationTrigger =
  | 'document_edit'
  | 'explicit_enable'
  | 'candidate_exhausted'
  | 'document_open'
  | 'model_ready'
  | 'retry';

export interface CompletionGenerationIntent {
  contextKey: string;
  editVersion: number;
  trigger: CompletionGenerationTrigger;
  anchorByte: number | null;
}

export function armCompletionGeneration(
  contextKey: string,
  editVersion: number,
  trigger: CompletionGenerationTrigger,
  anchorByte: number | null = null
): CompletionGenerationIntent | null {
  if (
    !contextKey ||
    !Number.isSafeInteger(editVersion) ||
    editVersion < 0 ||
    (anchorByte !== null && (!Number.isSafeInteger(anchorByte) || anchorByte < 0))
  ) return null;
  return { contextKey, editVersion, trigger, anchorByte };
}

export function completionGenerationIsArmed(
  intent: CompletionGenerationIntent | null,
  contextKey: string,
  editVersion: number
): boolean {
  return Boolean(
    intent &&
    intent.contextKey === contextKey &&
    intent.editVersion === editVersion
  );
}

export function disarmCompletionGeneration(): null {
  return null;
}

export function bindCompletionGenerationAnchor(
  intent: CompletionGenerationIntent | null,
  contextKey: string,
  editVersion: number,
  anchorByte: number
): CompletionGenerationIntent | null {
  if (
    !completionGenerationIsArmed(intent, contextKey, editVersion) ||
    !Number.isSafeInteger(anchorByte) ||
    anchorByte < 0
  ) return intent;
  if (!intent || intent.anchorByte !== null) return intent;
  return { ...intent, anchorByte };
}
