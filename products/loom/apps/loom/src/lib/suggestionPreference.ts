import type { SuggestionActivation } from './types';

export type StoredSuggestionPreference = 'on' | 'off' | null;

/**
 * An explicit preference is interpreted only after native policy activation
 * has been verified. Missing, stale, or unknown state fails closed.
 */
export function suggestionsEnabledFromStoredPreference(
  value: string | null,
  activation: SuggestionActivation | null = null
): boolean {
  if (activation !== 'project_opt_in' && activation !== 'quiet_default') {
    return false;
  }
  if (value === 'off') {
    return false;
  }
  if (value === 'on') {
    return true;
  }
  return value === null && activation === 'quiet_default';
}
