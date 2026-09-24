/** Observation only. None of these values may authorize an edit or generation. */
export interface TextBoundaryDelta {
  readonly before_utf8_bytes: number;
  readonly after_utf8_bytes: number;
  readonly equal: boolean;
  readonly first_changed_byte: number | null;
  readonly removed_utf8_bytes: number;
  readonly inserted_utf8_bytes: number;
  readonly exact_terminal_space_removed: boolean;
}

/** Compare bytes, without retaining manuscript text, event.data or transaction slices. */
export function textBoundaryDelta(before: string, after: string): TextBoundaryDelta {
  const encoder = new TextEncoder();
  const left = encoder.encode(before);
  const right = encoder.encode(after);
  let start = 0;
  while (start < left.length && start < right.length && left[start] === right[start]) start += 1;
  let leftEnd = left.length;
  let rightEnd = right.length;
  while (leftEnd > start && rightEnd > start && left[leftEnd - 1] === right[rightEnd - 1]) {
    leftEnd -= 1;
    rightEnd -= 1;
  }
  return {
    before_utf8_bytes: left.length,
    after_utf8_bytes: right.length,
    equal: before === after,
    first_changed_byte: before === after ? null : start,
    removed_utf8_bytes: leftEnd - start,
    inserted_utf8_bytes: rightEnd - start,
    // This is a byte relationship, NOT a judgement about user intent. An
    // authorized Backspace can produce it too; correlate the input receipts.
    exact_terminal_space_removed: before === `${after} `
  };
}

export type BoundaryObservationKind =
  | 'visual_mount'
  | 'visual_transaction'
  | 'visual_projection'
  | 'visual_external_value'
  | 'source_input'
  | 'app_update_text'
  | 'controller_insertion'
  | 'shuttle_schedule'
  | 'shuttle_fire'
  | 'visual_shuttle_attempt'
  | 'source_shuttle_attempt';

export interface BoundaryObservation {
  readonly kind: BoundaryObservationKind;
  readonly delta?: TextBoundaryDelta;
  /** Only metadata/identities: never manuscript text, input data or key characters. */
  readonly facts: Readonly<Record<string, string | number | boolean | null>>;
}

export interface BoundaryScope {
  readonly session_id: string;
  readonly document_id: string;
  readonly document_epoch: number;
  readonly edit_version: number;
  readonly source_revision_id: string;
  readonly visible_blob_id: string;
}

export interface BoundaryEntry extends BoundaryObservation {
  readonly sequence: number;
  readonly observed_at_ms: number;
  readonly scope: BoundaryScope;
}

export interface CompletionBoundaryTrace {
  readonly sequence: number;
  readonly dropped_entries: number;
  readonly entries: readonly BoundaryEntry[];
  /** Pinned separately so later polling/streaming cannot erase the first loss. */
  readonly first_terminal_space_loss: BoundaryEntry | null;
}

export const COMPLETION_BOUNDARY_TRACE_LIMIT = 12;

export function emptyCompletionBoundaryTrace(): CompletionBoundaryTrace {
  return { sequence: 0, dropped_entries: 0, entries: [], first_terminal_space_loss: null };
}

export function appendCompletionBoundaryObservation(
  trace: CompletionBoundaryTrace,
  observation: BoundaryObservation,
  scope: BoundaryScope,
  observedAtMs: number
): CompletionBoundaryTrace {
  const entry: BoundaryEntry = {
    ...observation,
    facts: { ...observation.facts },
    ...(observation.delta ? { delta: { ...observation.delta } } : {}),
    scope: { ...scope },
    sequence: trace.sequence + 1,
    observed_at_ms: observedAtMs
  };
  const entries = [...trace.entries, entry];
  const dropped = Math.max(0, entries.length - COMPLETION_BOUNDARY_TRACE_LIMIT);
  return {
    sequence: entry.sequence,
    dropped_entries: trace.dropped_entries + dropped,
    entries: entries.slice(dropped),
    first_terminal_space_loss: trace.first_terminal_space_loss ??
      (entry.delta?.exact_terminal_space_removed ? entry : null)
  };
}

/** Only classify controls needed for this investigation; do not log typed characters. */
export function boundaryKeyKind(event: Pick<KeyboardEvent, 'key' | 'metaKey' | 'shiftKey'>): string {
  if (event.metaKey && event.shiftKey && event.key.toLowerCase() === 'g') return 'suggestions_shortcut';
  if (event.metaKey && event.shiftKey && event.key.toLowerCase() === 'j') return 'shuttle_shortcut';
  switch (event.key) {
    case 'Backspace': case 'Delete': case 'Enter': case 'Tab': case 'Escape':
    case 'ArrowLeft': case 'ArrowRight': case 'ArrowUp': case 'ArrowDown':
    case 'Alt': case 'Meta': case 'Control': case 'Shift':
      return event.key;
    default: return 'other';
  }
}
