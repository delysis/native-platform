export type StoredSuggestionPreference = 'on' | 'off' | null;

/**
 * Local autocomplete is the quiet default. Only an explicit persisted opt-out
 * disables it; missing or stale browser state must not recreate onboarding.
 */
export function suggestionsEnabledFromStoredPreference(value: string | null): boolean {
  return value !== 'off';
}
