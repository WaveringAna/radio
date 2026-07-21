import type { RadioAlbum, Song } from '../../shared/lib/radio'
import { normalizeTitleForUi } from './helpers'

export interface ArtistEntry {
  /** Normalized grouping key — spellings that differ only by case or punctuation collapse. */
  key: string
  /** The most common raw spelling, so the UI shows what the library actually says. */
  name: string
  songs: Song[]
  albums: RadioAlbum[]
  totalSeconds: number
  haystack: string
}

/**
 * Groups the library by artist. Artists aren't a stored entity — they're
 * derived from `song.artist` — so this is the whole implementation of the
 * Artists tab's data model.
 */
export function buildArtistIndex(songs: Song[], albums: RadioAlbum[]): ArtistEntry[] {
  const groups = new Map<string, { spellings: Map<string, number>; songs: Song[] }>()

  for (const song of songs) {
    const raw = song.artist?.trim()
    if (!raw) continue
    const key = normalizeTitleForUi(raw) || raw.toLowerCase()
    let group = groups.get(key)
    if (!group) {
      group = { spellings: new Map(), songs: [] }
      groups.set(key, group)
    }
    group.spellings.set(raw, (group.spellings.get(raw) ?? 0) + 1)
    group.songs.push(song)
  }

  const entries: ArtistEntry[] = []
  for (const [key, group] of groups) {
    const name = [...group.spellings.entries()].sort((a, b) => b[1] - a[1])[0][0]
    const songIds = new Set(group.songs.map((song) => song.id))
    const artistAlbums = albums.filter((album) => album.tracks.some((track) => songIds.has(track.id)))
    entries.push({
      key,
      name,
      songs: group.songs,
      albums: artistAlbums,
      totalSeconds: group.songs.reduce((sum, song) => sum + (song.durationSeconds ?? 0), 0),
      haystack: `${name} ${group.songs.map((song) => song.title).join(' ')}`.toLowerCase(),
    })
  }

  return entries.sort((a, b) => a.name.localeCompare(b.name))
}
