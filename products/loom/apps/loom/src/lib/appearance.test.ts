import { describe, expect, it } from 'vitest';
import {
  APPEARANCE_PREFERENCE_KEY,
  appearancePreference,
  loadAppearancePreference,
  persistAppearancePreference,
  resolveAppearance,
  toggledAppearance
} from './appearance';

describe('appearance preference', () => {
  it('defaults invalid and absent values to the system', () => {
    expect(appearancePreference(null)).toBe('system');
    expect(appearancePreference('legacy')).toBe('system');
  });

  it('resolves system changes while explicit choices remain stable', () => {
    expect(resolveAppearance('system', false)).toBe('light');
    expect(resolveAppearance('system', true)).toBe('dark');
    expect(resolveAppearance('light', true)).toBe('light');
    expect(resolveAppearance('dark', false)).toBe('dark');
  });

  it('toggles from the currently resolved appearance', () => {
    expect(toggledAppearance('system', true)).toBe('light');
    expect(toggledAppearance('system', false)).toBe('dark');
    expect(toggledAppearance('dark', true)).toBe('light');
  });

  it('persists explicit choices until the author explicitly returns to system', () => {
    const values = new Map<string, string>();
    const storage = {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => { values.set(key, value); }
    };

    const host = { localStorage: storage };
    expect(loadAppearancePreference(host)).toBe('system');
    expect(persistAppearancePreference(host, 'light')).toBe(true);
    expect(values.get(APPEARANCE_PREFERENCE_KEY)).toBe('light');
    expect(loadAppearancePreference(host)).toBe('light');
    expect(resolveAppearance(loadAppearancePreference(host), true)).toBe('light');

    expect(persistAppearancePreference(host, 'dark')).toBe(true);
    expect(resolveAppearance(loadAppearancePreference(host), false)).toBe('dark');

    expect(persistAppearancePreference(host, 'system')).toBe(true);
    expect(loadAppearancePreference(host)).toBe('system');
    expect(resolveAppearance(loadAppearancePreference(host), true)).toBe('dark');
  });

  it('fails safely to system when preference storage is unavailable', () => {
    const host = Object.create(null) as { localStorage: Storage };
    Object.defineProperty(host, 'localStorage', {
      get: () => { throw new Error('storage unavailable'); }
    });
    expect(loadAppearancePreference(host)).toBe('system');
    expect(persistAppearancePreference(host, 'dark')).toBe(false);
  });
});
