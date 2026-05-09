export type ThemePreference = 'system' | 'light' | 'dark'

const THEME_STORAGE_KEY = 'radio.theme'

/**
 * Reads the persisted theme preference.
 * @returns Stored theme preference, defaulting to system.
 */
export function readThemePreference(): ThemePreference {
  const stored = window.localStorage.getItem(THEME_STORAGE_KEY)
  return stored === 'light' || stored === 'dark' || stored === 'system' ? stored : 'system'
}

/**
 * Persists and applies the theme preference.
 * @param preference Theme preference to apply.
 */
export function applyThemePreference(preference: ThemePreference): void {
  window.localStorage.setItem(THEME_STORAGE_KEY, preference)
  document.documentElement.dataset.theme = preference
}

/**
 * Returns the next theme preference in the switcher cycle.
 * @param preference Current theme preference.
 * @returns The next theme preference.
 */
export function nextThemePreference(preference: ThemePreference): ThemePreference {
  switch (preference) {
    case 'system':
      return 'light'
    case 'light':
      return 'dark'
    case 'dark':
      return 'system'
  }
}
