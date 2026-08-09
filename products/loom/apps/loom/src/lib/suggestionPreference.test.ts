import { describe, expect, it } from 'vitest';

import { suggestionsEnabledFromStoredPreference } from './suggestionPreference';

describe('suggestionsEnabledFromStoredPreference', () => {
  it('enables quiet local autocomplete for a new project', () => {
    expect(suggestionsEnabledFromStoredPreference(null)).toBe(true);
  });

  it('preserves an explicit author opt-out', () => {
    expect(suggestionsEnabledFromStoredPreference('off')).toBe(false);
  });

  it('keeps explicit on and stale values on the current default', () => {
    expect(suggestionsEnabledFromStoredPreference('on')).toBe(true);
    expect(suggestionsEnabledFromStoredPreference('legacy')).toBe(true);
  });
});
