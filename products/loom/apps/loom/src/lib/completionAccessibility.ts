import type { InlineGhostObservation } from './inlineGhostObservation';

export interface VisualCompletionAccessibilityWitness {
  available: boolean;
  optionHeld: boolean;
  fanVisible: boolean;
  lensPinned: boolean;
  inlineHidden: boolean;
  inline: InlineGhostObservation | null;
  selectedCandidateId: string;
  selectedPresentationKey: string;
  alternativeCandidateIds: string[];
  alternativePresentationKeys: string[];
  alternativeRunIds: string[];
}

export interface VisualSelectionAccessibilityWitness {
  available: boolean;
  epoch: number;
  selectionKind: string;
  from: number;
  to: number;
  empty: boolean;
  allVisibleText: boolean;
  caretAtEnd: boolean;
  caretByteOffset: number | null;
}

export function unavailableVisualCompletionWitness(): VisualCompletionAccessibilityWitness {
  return {
    available: false,
    optionHeld: false,
    fanVisible: false,
    lensPinned: false,
    inlineHidden: true,
    inline: null,
    selectedCandidateId: '',
    selectedPresentationKey: '',
    alternativeCandidateIds: [],
    alternativePresentationKeys: [],
    alternativeRunIds: []
  };
}

export function unavailableVisualSelectionWitness(epoch = 0): VisualSelectionAccessibilityWitness {
  return {
    available: false,
    epoch,
    selectionKind: '',
    from: -1,
    to: -1,
    empty: true,
    allVisibleText: false,
    caretAtEnd: false,
    caretByteOffset: null
  };
}
