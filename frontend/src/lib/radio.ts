export const API_BASE = import.meta.env.VITE_API_BASE ?? ''

const SESSION_TOKEN_KEY = 'radio_session_token'
const VIEWER_ID_KEY = 'radio_viewer_id'

export function getSessionToken(): string | null {
  return localStorage.getItem(SESSION_TOKEN_KEY)
}

export function setSessionToken(token: string): void {
  localStorage.setItem(SESSION_TOKEN_KEY, token)
}

export function clearSessionToken(): void {
  localStorage.removeItem(SESSION_TOKEN_KEY)
}

export function authHeaders(): Record<string, string> {
  const token = getSessionToken()
  return token ? { Authorization: `Bearer ${token}` } : {}
}

export interface Song {
  id: string
  title: string
  artist: string
  album?: string | null
  genre?: string | null
  durationSeconds?: number | null
  mimeType?: string | null
  hasCover: boolean
  addedByDid: string
  createdAt: number
  loudnessLufs?: number | null
  loudnessPeak?: number | null
}

export interface RadioAlbum {
  id: string
  title: string
  position: number
  isEnabled: boolean
  tracks: Song[]
}

export interface QueueItem {
  id: string
  position: number
  queuedByDid: string
  songId: string
  title: string
  artist: string
  album?: string | null
  addedByDid: string
}

export interface RadioState {
  currentSongId?: string | null
  status: 'playing' | 'paused' | 'stopped'
  startedAt?: number | null
  pausedAt?: number | null
  positionSeconds: number
  updatedByDid?: string | null
}

export interface RadioSeek {
  positionSeconds: number
}

export interface RadioSnapshot {
  state: RadioState
  currentSong?: Song | null
  queue: QueueItem[]
}

export type RadioEvent = {
  type: 'snapshotChanged'
  snapshot: RadioSnapshot
} | {
  type: 'viewerCountChanged'
  viewerCount?: number
  viewer_count?: number
  listenerDids?: string[]
  listener_dids?: string[]
} | {
  type: 'viewerKeepalive'
}

export interface AlbumInput {
  title: string
  songIds: string[]
}

export interface SongUploadInput {
  file: File
  title: string
  artist: string
  album?: string
  genre?: string
  durationSeconds?: number
  cover?: File | null
  addToQueue: boolean
}

export interface SongMetadataInput {
  title: string
  artist: string
  album?: string | null
  genre?: string | null
  durationSeconds?: number | null
}

export interface UrlSongInput {
  url: string
  title?: string
  artist?: string
  album?: string
  addToQueue: boolean
}

/**
 * Loads the current public radio snapshot.
 * @returns The current radio state and queue.
 * @throws Error When the backend request fails.
 */
export async function fetchRadioSnapshot(): Promise<RadioSnapshot> {
  const response = await fetch(`${API_BASE}/api/radio/state`, { cache: 'no-store' })
  if (!response.ok) {
    throw new Error('failed to load radio state')
  }

  return (await response.json()) as RadioSnapshot
}

/**
 * Loads the current backend seek position in seconds.
 * @returns The current seek position.
 * @throws Error When the backend request fails.
 */
export async function fetchRadioSeek(): Promise<RadioSeek> {
  const response = await fetch(`${API_BASE}/api/radio/seek`, { cache: 'no-store' })
  if (!response.ok) {
    throw new Error('failed to load radio seek')
  }

  return (await response.json()) as RadioSeek
}

/**
 * Opens a websocket for realtime radio events.
 * @returns A connected websocket instance.
 */
export function openRadioSocket(): WebSocket {
  const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
  const host = API_BASE
    ? new URL(API_BASE).host
    : window.location.port === '5173'
      ? `${window.location.hostname}:3000`
      : window.location.host
  return new WebSocket(`${protocol}//${host}/api/radio/ws`)
}

/**
 * Returns this tab's stable viewer id for websocket presence.
 * @returns A UUID-like id scoped to the current browser tab.
 */
export function getRadioViewerId(): string {
  const stored = window.sessionStorage.getItem(VIEWER_ID_KEY)
  if (stored) return stored

  const viewerId = crypto.randomUUID?.() ?? `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`
  window.sessionStorage.setItem(VIEWER_ID_KEY, viewerId)
  return viewerId
}

export function sendRadioViewerHello(socket: WebSocket, viewerId: string, did?: string | null): void {
  socket.send(JSON.stringify({ type: 'viewerHello', viewer_id: viewerId, did: did ?? null }))
}

export function sendRadioViewerKeepalive(socket: WebSocket, viewerId: string, did?: string | null): void {
  socket.send(JSON.stringify({ type: 'viewerKeepalive', viewer_id: viewerId, did: did ?? null }))
}

const LISTENER_OPT_OUT_KEY = 'radio_listener_opt_out'

export function getListenerOptOut(): boolean {
  return localStorage.getItem(LISTENER_OPT_OUT_KEY) === 'on'
}

export function setListenerOptOut(optOut: boolean): void {
  localStorage.setItem(LISTENER_OPT_OUT_KEY, optOut ? 'on' : 'off')
}

/**
 * Loads all songs added to the radio library.
 * @returns Songs ordered by newest first.
 * @throws Error When the backend request fails.
 */
export async function fetchAlbums(): Promise<RadioAlbum[]> {
  const response = await fetch(`${API_BASE}/api/radio/albums`, { credentials: 'include', headers: authHeaders() })
  if (!response.ok) {
    throw new Error('failed to load album loops')
  }

  return (await response.json()) as RadioAlbum[]
}

/**
 * Creates an album loop from selected songs.
 * @param input Album title and ordered song ids.
 * @returns The created album loop.
 * @throws Error When the backend request fails.
 */
export async function createAlbum(input: AlbumInput): Promise<RadioAlbum> {
  const response = await fetch(`${API_BASE}/api/radio/albums`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...authHeaders() },
    credentials: 'include',
    body: JSON.stringify(input),
  })
  if (!response.ok) {
    throw new Error('failed to create album loop')
  }

  return (await response.json()) as RadioAlbum
}

/**
 * Creates an album loop from matching song metadata.
 * @param album Album metadata value to import.
 * @returns The created album loop.
 * @throws Error When the backend request fails.
 */
export async function createAlbumFromMetadata(album: string): Promise<RadioAlbum> {
  const response = await fetch(`${API_BASE}/api/radio/albums/from-metadata`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...authHeaders() },
    credentials: 'include',
    body: JSON.stringify({ album }),
  })
  if (!response.ok) {
    throw new Error('failed to mass add album')
  }

  return (await response.json()) as RadioAlbum
}

/**
 * Deletes an album loop.
 * @param albumId Album loop id.
 * @returns Remaining album loops.
 * @throws Error When the backend request fails.
 */
export async function deleteAlbum(albumId: string): Promise<RadioAlbum[]> {
  const response = await fetch(`${API_BASE}/api/radio/albums/${albumId}`, {
    method: 'DELETE',
    credentials: 'include',
    headers: authHeaders(),
  })
  if (!response.ok) {
    throw new Error('failed to delete album loop')
  }

  return (await response.json()) as RadioAlbum[]
}

/**
 * Appends songs to an existing album loop, skipping duplicates.
 * @param albumId Album loop id.
 * @param songIds Song ids to add.
 * @returns Updated album.
 * @throws Error When the backend request fails.
 */
export async function addSongsToAlbum(albumId: string, songIds: string[]): Promise<RadioAlbum> {
  const response = await fetch(`${API_BASE}/api/radio/albums/${albumId}/songs`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...authHeaders() },
    credentials: 'include',
    body: JSON.stringify({ songIds }),
  })
  if (!response.ok) {
    throw new Error('failed to add songs to album loop')
  }

  return (await response.json()) as RadioAlbum
}

/**
 * Enables or disables an album loop.
 * @param albumId Album loop id.
 * @param enabled Whether the album should loop.
 * @returns Updated album loops.
 * @throws Error When the backend request fails.
 */
export async function setAlbumEnabled(albumId: string, enabled: boolean): Promise<RadioAlbum[]> {
  const response = await fetch(`${API_BASE}/api/radio/albums/${albumId}/enabled`, {
    method: 'PUT',
    headers: { 'content-type': 'application/json', ...authHeaders() },
    credentials: 'include',
    body: JSON.stringify({ enabled }),
  })
  if (!response.ok) {
    throw new Error('failed to update album loop')
  }

  return (await response.json()) as RadioAlbum[]
}

/**
 * Loads all songs added to the radio library.
 * @returns Songs ordered by newest first.
 * @throws Error When the backend request fails.
 */
export async function fetchSongs(): Promise<Song[]> {
  const response = await fetch(`${API_BASE}/api/songs`)
  if (!response.ok) {
    throw new Error('failed to load songs')
  }

  return (await response.json()) as Song[]
}

/**
 * Adds an existing song to the queue.
 * @param songId Song id to enqueue.
 * @returns The created queue item.
 * @throws Error When the backend request fails.
 */
export async function enqueueAlbum(songIds: string[]): Promise<RadioSnapshot> {
  const response = await fetch(`${API_BASE}/api/radio/queue/album`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...authHeaders() },
    credentials: 'include',
    body: JSON.stringify({ songIds }),
  })

  if (!response.ok) {
    throw new Error('failed to queue album')
  }

  return (await response.json()) as RadioSnapshot
}

export async function enqueueSong(songId: string): Promise<RadioSnapshot> {
  const response = await fetch(`${API_BASE}/api/radio/queue`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...authHeaders() },
    credentials: 'include',
    body: JSON.stringify({ songId }),
  })

  if (!response.ok) {
    throw new Error('failed to add song to queue')
  }

  return (await response.json()) as RadioSnapshot
}

/**
 * Removes a queue item.
 * @param queueId Queue item id to remove.
 * @returns The updated radio snapshot.
 * @throws Error When the backend request fails.
 */
export async function removeQueueItem(queueId: string): Promise<RadioSnapshot> {
  const response = await fetch(`${API_BASE}/api/radio/queue/${queueId}`, {
    method: 'DELETE',
    credentials: 'include',
    headers: authHeaders(),
  })

  if (!response.ok) {
    throw new Error('failed to remove queue item')
  }

  return (await response.json()) as RadioSnapshot
}

/**
 * Clears every item from the queue.
 * @returns The updated radio snapshot.
 * @throws Error When the backend request fails.
 */
export async function clearQueue(): Promise<RadioSnapshot> {
  const response = await fetch(`${API_BASE}/api/radio/queue`, {
    method: 'DELETE',
    credentials: 'include',
    headers: authHeaders(),
  })

  if (!response.ok) {
    throw new Error('failed to clear queue')
  }

  return (await response.json()) as RadioSnapshot
}

/**
 * Reorders the queue using the supplied ordered list of queue ids.
 * @param queueIds Queue item ids in the desired order.
 * @returns The updated radio snapshot.
 * @throws Error When the backend request fails.
 */
export async function reorderQueue(queueIds: string[]): Promise<RadioSnapshot> {
  const response = await fetch(`${API_BASE}/api/radio/queue/reorder`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...authHeaders() },
    credentials: 'include',
    body: JSON.stringify({ queueIds }),
  })

  if (!response.ok) {
    throw new Error('failed to reorder queue')
  }

  return (await response.json()) as RadioSnapshot
}

/**
 * Uploads an audio file as a new song.
 * @param input Song upload form values.
 * @returns The created song.
 * @throws Error When upload fails.
 */
export async function uploadSong(input: SongUploadInput): Promise<Song> {
  const formData = new FormData()
  formData.set('file', input.file)
  formData.set('title', input.title)
  formData.set('artist', input.artist)
  formData.set('album', input.album ?? '')
  if (input.genre) {
    formData.set('genre', input.genre)
  }
  if (input.durationSeconds !== undefined) {
    formData.set('durationSeconds', String(input.durationSeconds))
  }
  formData.set('addToQueue', String(input.addToQueue))

  const response = await fetch(`${API_BASE}/api/songs`, {
    method: 'POST',
    body: formData,
    credentials: 'include',
    headers: authHeaders(),
  })

  if (!response.ok) {
    throw new Error('song upload failed')
  }

  const song = (await response.json()) as Song
  if (input.cover) {
    return uploadSongCover(song.id, input.cover)
  }

  return song
}

/**
 * Uploads or replaces a song album cover.
 * @param songId Song id to update.
 * @param cover Cover image file.
 * @returns Updated song metadata.
 * @throws Error When upload fails.
 */
export async function uploadSongCover(songId: string, cover: File): Promise<Song> {
  const formData = new FormData()
  formData.set('cover', cover)

  const response = await fetch(`${API_BASE}/api/songs/${songId}/cover`, {
    method: 'PUT',
    body: formData,
    credentials: 'include',
    headers: authHeaders(),
  })

  if (!response.ok) {
    throw new Error('cover upload failed')
  }

  return (await response.json()) as Song
}

/**
 * Updates editable song metadata.
 * @param songId Song id to update.
 * @param input Updated song metadata.
 * @returns Updated song metadata.
 * @throws Error When update fails.
 */
export async function updateSongMetadata(songId: string, input: SongMetadataInput): Promise<Song> {
  const response = await fetch(`${API_BASE}/api/songs/${songId}`, {
    method: 'PUT',
    headers: { 'content-type': 'application/json', ...authHeaders() },
    credentials: 'include',
    body: JSON.stringify(input),
  })

  if (!response.ok) {
    throw new Error('failed to update song metadata')
  }

  return (await response.json()) as Song
}

/**
 * Deletes a song from the library and queue.
 * @param songId Song id to delete.
 * @returns Updated radio snapshot.
 * @throws Error When deletion fails.
 */
export async function deleteSong(songId: string): Promise<RadioSnapshot> {
  const response = await fetch(`${API_BASE}/api/songs/${songId}`, {
    method: 'DELETE',
    credentials: 'include',
    headers: authHeaders(),
  })

  if (!response.ok) {
    throw new Error('failed to delete song')
  }

  return (await response.json()) as RadioSnapshot
}

/**
 * Sends an admin radio control action.
 * @param action Playback action to perform.
 * @returns The updated radio snapshot.
 * @throws Error When the control request fails.
 */
export interface SubsonicCreds {
  serverUrl: string
  username: string
  password: string
}

export interface SubsonicSongResult {
  id: string
  title: string
  artist: string
  album?: string | null
  durationSeconds?: number | null
  coverArtId?: string | null
}

const SUBSONIC_CREDS_KEY = 'radio_subsonic_creds'

export function loadSubsonicCreds(): SubsonicCreds {
  try {
    return JSON.parse(localStorage.getItem(SUBSONIC_CREDS_KEY) ?? '{}') as SubsonicCreds
  } catch {
    return { serverUrl: '', username: '', password: '' }
  }
}

export function saveSubsonicCreds(creds: SubsonicCreds): void {
  localStorage.setItem(SUBSONIC_CREDS_KEY, JSON.stringify(creds))
}

export async function searchSubsonic(creds: SubsonicCreds, query: string): Promise<SubsonicSongResult[]> {
  const response = await fetch(`${API_BASE}/api/subsonic/search`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...authHeaders() },
    credentials: 'include',
    body: JSON.stringify({ ...creds, query }),
  })
  if (!response.ok) throw new Error('subsonic search failed')
  return (await response.json()) as SubsonicSongResult[]
}

export async function importFromSubsonic(
  creds: SubsonicCreds,
  songId: string,
  coverArtId: string | null | undefined,
  addToQueue: boolean,
): Promise<Song> {
  const response = await fetch(`${API_BASE}/api/songs/from-subsonic`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...authHeaders() },
    credentials: 'include',
    body: JSON.stringify({ ...creds, songId, coverArtId, addToQueue }),
  })
  if (!response.ok) {
    const data = await response.json().catch(() => ({})) as { error?: string }
    throw new Error(data.error ?? 'subsonic import failed')
  }
  return (await response.json()) as Song
}

export async function importFromSubsonicShare(
  shareUrl: string,
  addToQueue: boolean,
): Promise<Song> {
  const response = await fetch(`${API_BASE}/api/songs/from-subsonic-share`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...authHeaders() },
    credentials: 'include',
    body: JSON.stringify({ shareUrl, addToQueue }),
  })
  if (!response.ok) {
    const data = await response.json().catch(() => ({})) as { error?: string }
    throw new Error(data.error ?? 'subsonic share import failed')
  }
  return (await response.json()) as Song
}

export async function uploadSongFromUrl(input: UrlSongInput): Promise<Song> {
  const response = await fetch(`${API_BASE}/api/songs/from-url`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...authHeaders() },
    credentials: 'include',
    body: JSON.stringify(input),
  })

  if (!response.ok) {
    const data = await response.json().catch(() => ({})) as { error?: string }
    throw new Error(data.error === 'url_fetch_failed' ? 'could not fetch audio from that url.' : 'url import failed')
  }

  return (await response.json()) as Song
}

export async function controlRadio(
  action: 'play' | 'pause' | 'stop' | 'skip',
  intent: 'explicit_admin_action',
): Promise<RadioSnapshot> {
  const response = await fetch(`${API_BASE}/api/radio/control/${action}`, {
    method: 'POST',
    credentials: 'include',
    headers: { 'content-type': 'application/json', ...authHeaders() },
    body: JSON.stringify({ intent }),
  })

  if (!response.ok) {
    throw new Error('radio control failed')
  }

  return (await response.json()) as RadioSnapshot
}
