import { expect, it } from 'vitest';
import { resolveAppearance, toggledAppearance } from './appearance';
it('follows system changes unless overridden for this session', () => {
  expect(resolveAppearance('system', false)).toBe('light');
  expect(resolveAppearance('system', true)).toBe('dark');
  const override = toggledAppearance('system', true);
  expect(resolveAppearance(override, true)).toBe('light');
  expect(resolveAppearance(override, false)).toBe('light');
});
