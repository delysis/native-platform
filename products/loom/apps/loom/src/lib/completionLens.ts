export interface CompletionLensState {
  momentary: boolean;
  pinned: boolean;
}

export type CompletionLensEvent =
  | { kind: 'option_down'; alternativeCount: number }
  | { kind: 'release_option' }
  | { kind: 'toggle_pin'; alternativeCount: number }
  | { kind: 'close' }
  | { kind: 'alternatives_changed'; alternativeCount: number };

export const CLOSED_COMPLETION_LENS: CompletionLensState = Object.freeze({
  momentary: false,
  pinned: false
});

/**
 * The lens is an explicit two-latch state machine. Physical Option state is
 * transient and must fail closed on lifecycle loss; a mouse/AX pin is an
 * intentional user choice and survives an ordinary native-window blur.
 */
export function reduceCompletionLens(
  state: CompletionLensState,
  event: CompletionLensEvent
): CompletionLensState {
  switch (event.kind) {
    case 'option_down':
      return event.alternativeCount > 1
        ? { ...state, momentary: true }
        : CLOSED_COMPLETION_LENS;
    case 'release_option':
      return state.momentary ? { ...state, momentary: false } : state;
    case 'toggle_pin':
      return event.alternativeCount > 1
        ? { ...state, pinned: !state.pinned }
        : CLOSED_COMPLETION_LENS;
    case 'alternatives_changed':
      return event.alternativeCount > 1 ? state : CLOSED_COMPLETION_LENS;
    case 'close':
      return CLOSED_COMPLETION_LENS;
  }
}

export function completionLensVisible(state: CompletionLensState): boolean {
  return state.momentary || state.pinned;
}
