import type { AtprotoProfile } from '../../shared/lib/atproto'

export function fallbackProfile(did: string): AtprotoProfile {
  return { did, handle: did }
}

export function formatTime(seconds: number | null | undefined): string {
  if (!seconds || seconds < 0) return '0:00'
  const minutes = Math.floor(seconds / 60)
  const remainder = Math.floor(seconds % 60).toString().padStart(2, '0')
  return `${minutes}:${remainder}`
}

export function formatClockTime(msFromNow: number): string {
  const at = new Date(Date.now() + msFromNow)
  let hours = at.getHours()
  const minutes = at.getMinutes()
  const ampm = hours >= 12 ? 'PM' : 'AM'
  hours = hours % 12 || 12
  return `${hours}:${minutes < 10 ? '0' + minutes : minutes} ${ampm}`
}

/**
 * Folds accents, punctuation, case, and runs of whitespace out of a title so
 * near-identical spellings collapse onto one grouping key.
 */
export function normalizeTitleForUi(title: string): string {
  return title
    .normalize('NFD')
    .replace(/[\u0300-\u036f]/g, '')
    .toLowerCase()
    .replace(/[^a-z0-9\s]/g, '')
    .trim()
    .replace(/\s+/g, ' ')
}
