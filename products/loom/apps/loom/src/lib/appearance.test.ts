import { describe, expect, it } from 'vitest';
import {
  appearancePreference,
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
});
