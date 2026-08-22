export type AppearancePreference = 'system' | 'light' | 'dark';
export type ResolvedAppearance = 'light' | 'dark';

export function appearancePreference(value: string | null): AppearancePreference {
  return value === 'light' || value === 'dark' || value === 'system' ? value : 'system';
}

export function resolveAppearance(
  preference: AppearancePreference,
  systemDark: boolean
): ResolvedAppearance {
  return preference === 'system' ? (systemDark ? 'dark' : 'light') : preference;
}

export function toggledAppearance(
  preference: AppearancePreference,
  systemDark: boolean
): AppearancePreference {
  return resolveAppearance(preference, systemDark) === 'dark' ? 'light' : 'dark';
}
