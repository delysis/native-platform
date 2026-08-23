export interface VisualCompletionAccessibilityWitness {
  available: boolean;
  optionHeld: boolean;
  fanVisible: boolean;
  inlineHidden: boolean;
  selectedCandidateId: string;
  selectedPresentationKey: string;
  alternativeCandidateIds: string[];
  alternativePresentationKeys: string[];
  alternativeRunIds: string[];
}

export function unavailableVisualCompletionWitness(): VisualCompletionAccessibilityWitness {
  return {
    available: false,
    optionHeld: false,
    fanVisible: false,
    inlineHidden: true,
    selectedCandidateId: '',
    selectedPresentationKey: '',
    alternativeCandidateIds: [],
    alternativePresentationKeys: [],
    alternativeRunIds: []
  };
}
