export type AppearancePreference = 'system' | 'light' | 'dark';
export type ResolvedAppearance = 'light' | 'dark';

export const APPEARANCE_PREFERENCE_KEY = 'loom.appearance.v1';

export interface AppearancePreferenceStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

export interface AppearancePreferenceHost {
  readonly localStorage: AppearancePreferenceStorage;
}

export function appearancePreference(value: string | null): AppearancePreference {
  return value === 'light' || value === 'dark' || value === 'system' ? value : 'system';
}

export function loadAppearancePreference(
  host: AppearancePreferenceHost
): AppearancePreference {
  try {
    return appearancePreference(host.localStorage.getItem(APPEARANCE_PREFERENCE_KEY));
  } catch {
    return 'system';
  }
}

export function persistAppearancePreference(
  host: AppearancePreferenceHost,
  preference: AppearancePreference
): boolean {
  try {
    host.localStorage.setItem(APPEARANCE_PREFERENCE_KEY, preference);
    return true;
  } catch {
    return false;
  }
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
