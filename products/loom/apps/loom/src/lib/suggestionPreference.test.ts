import { describe, expect, it } from 'vitest';

import { suggestionsEnabledFromStoredPreference } from './suggestionPreference';

describe('suggestionsEnabledFromStoredPreference', () => {
  it('enables a new project only under the verified quiet-default contract', () => {
    expect(suggestionsEnabledFromStoredPreference(null, 'quiet_default')).toBe(true);
    expect(suggestionsEnabledFromStoredPreference(null, 'project_opt_in')).toBe(false);
  });

  it('preserves an explicit author opt-out', () => {
    expect(suggestionsEnabledFromStoredPreference('off', 'quiet_default')).toBe(false);
    expect(suggestionsEnabledFromStoredPreference('off', 'project_opt_in')).toBe(false);
  });

  it('honors explicit opt-in under either verified activation', () => {
    expect(suggestionsEnabledFromStoredPreference('on', 'quiet_default')).toBe(true);
    expect(suggestionsEnabledFromStoredPreference('on', 'project_opt_in')).toBe(true);
  });

  it('fails closed without a verified activation or with stale state', () => {
    expect(suggestionsEnabledFromStoredPreference(null)).toBe(false);
    expect(suggestionsEnabledFromStoredPreference('on')).toBe(false);
    expect(suggestionsEnabledFromStoredPreference('legacy', 'quiet_default')).toBe(false);
    expect(
      suggestionsEnabledFromStoredPreference(
        null,
        'future_activation' as never
      )
    ).toBe(false);
  });
});
