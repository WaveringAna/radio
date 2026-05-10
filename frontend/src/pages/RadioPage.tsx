import { createEffect, createMemo, createResource, createSignal, For, Index, onCleanup, Show, untrack } from 'solid-js'
import { Eye, EyeOff, ListPlus, Trash2, Volume2 } from 'lucide-solid'
import { resolveAtprotoProfile, type AtprotoProfile } from '../lib/atproto'
import {
  API_BASE,
  clearQueue,
  controlRadio,
  enqueueAlbum,
  enqueueSong,
  fetchAlbums,
  fetchRadioSnapshot,
  fetchSongs,
  openRadioSocket,
  removeQueueItem,
  reorderQueue,
  type QueueItem,
  type RadioEvent,
  type RadioState,
  type Song,
} from '../lib/radio'
import { AdminUploadPanel } from '../components/AdminUploadPanel'
import { PaginationRow } from '../components/PaginationRow'
import { ProfileAvatar } from '../components/ProfileAvatar'
import { SongCoverThumb } from '../components/SongCoverThumb'
import { EqualizerPanel, createEqualizerController } from '../components/EqualizerPanel'
import { createPagedList } from '../primitives/createPagedList'

interface RadioPageProps {
  isAdmin: boolean
}

interface AlbumAccent {
  primary: string
  secondary: string
  primaryWash: string
  secondaryWash: string
  topWash: string
}

const DEFAULT_ALBUM_ACCENT: AlbumAccent = {
  primary: '190 124 143',
  secondary: '125 104 119',
  primaryWash: 'rgb(190 124 143 / 0%)',
  secondaryWash: 'rgb(125 104 119 / 0%)',
  topWash: 'rgb(190 124 143 / 0%)',
}

function fallbackProfile(did: string): AtprotoProfile {
  return { did, handle: did }
}

function readVolumeCookie(): number {
  const value = document.cookie
    .split('; ')
    .find((cookie) => cookie.startsWith('radio_volume='))
    ?.split('=')[1]
  const volume = Number(value)
  return Number.isFinite(volume) ? Math.min(1, Math.max(0, volume)) : 0.8
}

function writeVolumeCookie(volume: number): void {
  document.cookie = `radio_volume=${volume}; Max-Age=31536000; Path=/; SameSite=Lax`
}

function formatTime(seconds: number | null | undefined): string {
  if (!seconds || seconds < 0) {
    return '0:00'
  }

  const minutes = Math.floor(seconds / 60)
  const remainingSeconds = Math.floor(seconds % 60)
    .toString()
    .padStart(2, '0')
  return `${minutes}:${remainingSeconds}`
}

function rgbToHsl(red: number, green: number, blue: number): { hue: number; saturation: number; lightness: number } {
  const r = red / 255
  const g = green / 255
  const b = blue / 255
  const max = Math.max(r, g, b)
  const min = Math.min(r, g, b)
  const lightness = (max + min) / 2

  if (max === min) {
    return { hue: 0, saturation: 0, lightness }
  }

  const delta = max - min
  const saturation = lightness > 0.5 ? delta / (2 - max - min) : delta / (max + min)
  const hue = (() => {
    if (max === r) return (g - b) / delta + (g < b ? 6 : 0)
    if (max === g) return (b - r) / delta + 2
    return (r - g) / delta + 4
  })() / 6

  return { hue, saturation, lightness }
}

function accentFromRgb(primary: { red: number; green: number; blue: number }, secondary: { red: number; green: number; blue: number }): AlbumAccent {
  return {
    primary: `${primary.red} ${primary.green} ${primary.blue}`,
    secondary: `${secondary.red} ${secondary.green} ${secondary.blue}`,
    primaryWash: `rgb(${primary.red} ${primary.green} ${primary.blue} / 18%)`,
    secondaryWash: `rgb(${secondary.red} ${secondary.green} ${secondary.blue} / 14%)`,
    topWash: `rgb(${primary.red} ${primary.green} ${primary.blue} / 8%)`,
  }
}

function extractAlbumAccent(image: HTMLImageElement): AlbumAccent {
  const canvas = document.createElement('canvas')
  const width = 28
  const height = 28
  canvas.width = width
  canvas.height = height

  const context = canvas.getContext('2d', { willReadFrequently: true })
  if (!context) return DEFAULT_ALBUM_ACCENT
  context.drawImage(image, 0, 0, width, height)

  const pixels = context.getImageData(0, 0, width, height).data
  const buckets = new Map<string, { red: number; green: number; blue: number; count: number; score: number }>()
  let neutralRed = 0
  let neutralGreen = 0
  let neutralBlue = 0
  let neutralCount = 0

  for (let index = 0; index < pixels.length; index += 4) {
    const alpha = pixels[index + 3]
    if (alpha < 180) continue

    const red = pixels[index]
    const green = pixels[index + 1]
    const blue = pixels[index + 2]
    const { saturation, lightness } = rgbToHsl(red, green, blue)
    if (lightness >= 0.12 && lightness <= 0.88) {
      neutralRed += red
      neutralGreen += green
      neutralBlue += blue
      neutralCount += 1
    }
    if (saturation < 0.22 || lightness < 0.12 || lightness > 0.88) continue

    const key = `${Math.round(red / 24)}:${Math.round(green / 24)}:${Math.round(blue / 24)}`
    const existing = buckets.get(key) ?? { red: 0, green: 0, blue: 0, count: 0, score: 0 }
    existing.red += red
    existing.green += green
    existing.blue += blue
    existing.count += 1
    existing.score += saturation * (1 - Math.abs(lightness - 0.52))
    buckets.set(key, existing)
  }

  const ranked = [...buckets.values()]
    .map((bucket) => ({
      red: Math.round(bucket.red / bucket.count),
      green: Math.round(bucket.green / bucket.count),
      blue: Math.round(bucket.blue / bucket.count),
      score: bucket.score * Math.log2(bucket.count + 1),
    }))
    .sort((left, right) => right.score - left.score)

  if (ranked.length === 0 && neutralCount > 0) {
    const neutral = {
      red: Math.round(neutralRed / neutralCount),
      green: Math.round(neutralGreen / neutralCount),
      blue: Math.round(neutralBlue / neutralCount),
    }
    const lift = 24
    return accentFromRgb(neutral, {
      red: Math.min(255, neutral.red + lift),
      green: Math.min(255, neutral.green + lift),
      blue: Math.min(255, neutral.blue + lift),
    })
  }

  const primary = ranked[0] ?? { red: 255, green: 55, blue: 95 }
  const secondary = ranked.find((candidate) => Math.abs(candidate.red - primary.red) + Math.abs(candidate.green - primary.green) + Math.abs(candidate.blue - primary.blue) > 80)
    ?? ranked[1]
    ?? { red: 255, green: 149, blue: 0 }

  return accentFromRgb(primary, secondary)
}

/**
 * Renders the public radio view with admin-only upload and playback controls.
 * @param props Current viewer permissions.
 * @returns The radio page view.
 */
export default function RadioPage(props: RadioPageProps) {
  const [showAdminTools, setShowAdminTools] = createSignal(true)
  const [snapshot, { mutate, refetch }] = createResource(fetchRadioSnapshot)
  const [songs, { refetch: refetchSongs }] = createResource(fetchSongs)
  const [albums, { refetch: refetchAlbums }] = createResource(() => props.isAdmin && showAdminTools(), (enabled) => (enabled ? fetchAlbums() : []))
  const [pageError, setPageError] = createSignal<string | null>(null)
  const [profiles, setProfiles] = createSignal<Record<string, AtprotoProfile>>({})
  const inFlightDids = new Set<string>()
  const [volume, setVolume] = createSignal(readVolumeCookie())
  const [hasStarted, setHasStarted] = createSignal(false)
  const [isAudioPlaying, setIsAudioPlaying] = createSignal(false)
  const [clock, setClock] = createSignal(Date.now())
  const [songFilterArtist, setSongFilterArtist] = createSignal('')
  const [songFilterGenre, setSongFilterGenre] = createSignal('')
  const [songFilterDid, setSongFilterDid] = createSignal('')
  const [selectedSongIds, setSelectedSongIds] = createSignal<string[]>([])
  const [draggingQueueId, setDraggingQueueId] = createSignal<string | null>(null)
  const [albumAccent, setAlbumAccent] = createSignal<AlbumAccent>(DEFAULT_ALBUM_ACCENT)
  const [ambientLayers, setAmbientLayers] = createSignal<[AlbumAccent, AlbumAccent]>([DEFAULT_ALBUM_ACCENT, DEFAULT_ALBUM_ACCENT])
  const [activeAmbientLayer, setActiveAmbientLayer] = createSignal<0 | 1>(0)
  let lastAmbientKey = ''

  // Local playback state. Frontend self-advances through localQueue; backend
  // resyncs only on admin actions (detected via playbackKey diff).
  const [localCurrentSong, setLocalCurrentSong] = createSignal<Song | null>(null)
  const [localQueue, setLocalQueue] = createSignal<QueueItem[]>([])
  let consumedQueueIds = new Set<string>()
  let lastPlaybackKey: string | null = null
  let audioRef: HTMLAudioElement | undefined
  const equalizer = createEqualizerController(() => audioRef)

  const playbackKey = (state: RadioState | undefined) =>
    state ? `${state.currentSongId ?? ''}|${state.status}|${state.startedAt ?? ''}|${state.pausedAt ?? ''}` : ''

  const lookupSong = (id: string): Song | null =>
    (songs() ?? []).find((song) => song.id === id) ?? null

  const queueItemAsSong = (item: QueueItem): Song =>
    lookupSong(item.songId) ?? {
      id: item.songId,
      title: item.title,
      artist: item.artist,
      album: item.album ?? null,
      genre: null,
      durationSeconds: null,
      mimeType: null,
      hasCover: false,
      addedByDid: item.addedByDid,
      createdAt: 0,
    }

  const seekAudioTo = async (positionSeconds: number): Promise<void> => {
    if (!audioRef) return
    if (audioRef.readyState >= 1) {
      audioRef.currentTime = positionSeconds
      return
    }
    const element = audioRef
    await new Promise<void>((resolve) => {
      element.addEventListener('loadedmetadata', () => {
        element.currentTime = positionSeconds
        resolve()
      }, { once: true })
      element.addEventListener('error', () => resolve(), { once: true })
    })
  }

  const applyBackendPlayback = async (state: RadioState, song: Song | null, songChanged: boolean) => {
    if (!audioRef) return
    if (!song || state.status === 'stopped') {
      audioRef.pause()
      return
    }
    if (songChanged) {
      const expected = `${API_BASE}/api/songs/${song.id}/audio`
      if (audioRef.src !== expected && !audioRef.src.endsWith(`/api/songs/${song.id}/audio`)) {
        audioRef.src = expected
        audioRef.load()
      }
      await seekAudioTo(Math.max(0, state.positionSeconds))
    }
    if (state.status === 'playing' && hasStarted()) {
      void audioRef.play().catch(() => undefined)
    } else if (state.status === 'paused') {
      audioRef.pause()
    }
  }

  createEffect(() => {
    const interval = window.setInterval(() => setClock(Date.now()), 1000)
    onCleanup(() => window.clearInterval(interval))
  })

  // Merges incoming queue items with the current localQueue, reusing the
  // existing object reference whenever an id is unchanged. Snapshots arrive
  // with all-new objects on every WS push, so without this <For> would remount
  // every row on every broadcast.
  const mergeQueue = (incoming: QueueItem[]): QueueItem[] => {
    const existing = new Map(untrack(() => localQueue()).map((item) => [item.id, item]))
    return incoming.map((item) => {
      const prev = existing.get(item.id)
      if (
        prev &&
        prev.songId === item.songId &&
        prev.position === item.position &&
        prev.queuedByDid === item.queuedByDid &&
        prev.title === item.title &&
        prev.artist === item.artist &&
        prev.album === item.album &&
        prev.addedByDid === item.addedByDid
      ) {
        return prev
      }
      return item
    })
  }

  const applyMergedQueue = (incoming: QueueItem[]) => {
    const filtered = incoming.filter((item) => !consumedQueueIds.has(item.id))
    setLocalQueue(mergeQueue(filtered))
  }

  // Snapshot diff: cold-start init, admin-action resync, queue-only merge.
  // Compare song id against what's locally playing, not against the previous
  // snapshot — the frontend self-advances ahead of the backend, so a snapshot
  // arriving with the song we already moved to should not trigger a reload.
  createEffect(() => {
    const snap = snapshot()
    if (!snap) return
    const key = playbackKey(snap.state)
    const isFirst = lastPlaybackKey === null

    if (isFirst || key !== lastPlaybackKey) {
      const prevSongId = untrack(() => localCurrentSong())?.id ?? null
      const newSongId = snap.currentSong?.id ?? null
      const songChanged = prevSongId !== newSongId

      lastPlaybackKey = key
      if (songChanged) {
        consumedQueueIds = new Set()
        setLocalQueue(snap.queue)
        setLocalCurrentSong(snap.currentSong ?? null)
      } else {
        applyMergedQueue(snap.queue)
      }
      if (!isFirst) {
        void applyBackendPlayback(snap.state, snap.currentSong ?? null, songChanged)
      }
    } else {
      applyMergedQueue(snap.queue)
    }
  })

  createEffect(() => {
    let socket: WebSocket | null = null
    let reconnectTimer: number | null = null
    let reconnectAttempt = 0
    let cancelled = false

    const connect = () => {
      if (cancelled) return
      socket = openRadioSocket()

      socket.addEventListener('open', () => {
        reconnectAttempt = 0
      })

      socket.addEventListener('message', (message) => {
        const event = JSON.parse(message.data) as RadioEvent
        if (event.type === 'snapshotChanged') {
          mutate(event.snapshot)
          // Don't refetch songs/albums here — most snapshot events are queue
          // mutations that don't affect the library. Local actions that do
          // change the library refetch explicitly.
        }
      })

      const scheduleReconnect = () => {
        if (cancelled || reconnectTimer !== null) return
        // Refetch on every drop so state stays fresh while we wait to reopen.
        void refetch()
        void refetchSongs()
        void refetchAlbums()
        const delay = Math.min(30000, 500 * 2 ** Math.min(reconnectAttempt, 6))
        reconnectAttempt += 1
        reconnectTimer = window.setTimeout(() => {
          reconnectTimer = null
          connect()
        }, delay)
      }

      socket.addEventListener('close', scheduleReconnect)
      socket.addEventListener('error', () => socket?.close())
    }

    connect()

    onCleanup(() => {
      cancelled = true
      if (reconnectTimer !== null) window.clearTimeout(reconnectTimer)
      socket?.close()
    })
  })

  createEffect(() => {
    const currentSongDid = snapshot()?.currentSong?.addedByDid
    const dids = [
      ...(songs() ?? []).map((song) => song.addedByDid),
      ...(currentSongDid ? [currentSongDid] : []),
      ...(snapshot()?.queue ?? []).flatMap((item) => [item.addedByDid, item.queuedByDid]),
    ].filter((did, index, values) => values.indexOf(did) === index && !profiles()[did] && !inFlightDids.has(did))

    for (const did of dids) {
      inFlightDids.add(did)
      void resolveAtprotoProfile(did)
        .then((profile) => setProfiles((current) => ({ ...current, [did]: profile })))
        .finally(() => inFlightDids.delete(did))
    }
  })

  const currentSong = () => localCurrentSong()

  createEffect(() => {
    const song = currentSong()
    if (!song?.hasCover) {
      setAlbumAccent(DEFAULT_ALBUM_ACCENT)
      return
    }

    let cancelled = false
    const image = new Image()
    image.src = `${API_BASE}/api/songs/${song.id}/cover`
    image.onload = () => {
      if (cancelled) return
      try {
        setAlbumAccent(extractAlbumAccent(image))
      } catch {
        setAlbumAccent(DEFAULT_ALBUM_ACCENT)
      }
    }
    image.onerror = () => {
      if (!cancelled) setAlbumAccent(DEFAULT_ALBUM_ACCENT)
    }

    onCleanup(() => {
      cancelled = true
      image.onload = null
      image.onerror = null
    })
  })

  createEffect(() => {
    const accent = albumAccent()
    const key = `${accent.primary}|${accent.secondary}|${accent.topWash}`
    if (key === lastAmbientKey) return
    lastAmbientKey = key

    const nextLayer = untrack(activeAmbientLayer) === 0 ? 1 : 0
    setAmbientLayers(([first, second]) => (nextLayer === 0 ? [accent, second] : [first, accent]))
    window.requestAnimationFrame(() => setActiveAmbientLayer(nextLayer))
  })

  const ambientLayerStyle = (accent: AlbumAccent) =>
    `--ambient-a-wash: ${accent.primaryWash}; --ambient-b-wash: ${accent.secondaryWash}; --ambient-top-wash: ${accent.topWash};`

  const currentAudioUrl = () => {
    const songId = localCurrentSong()?.id
    return songId ? `${API_BASE}/api/songs/${songId}/audio` : undefined
  }
  const nextAudioUrl = () => {
    const next = localQueue()[0]
    return next ? `${API_BASE}/api/songs/${next.songId}/audio` : undefined
  }
  const livePositionSeconds = () => {
    // Re-evaluate every second for queue progress labels.
    void clock()
    if (audioRef && Number.isFinite(audioRef.currentTime)) {
      return audioRef.currentTime
    }
    return Math.max(0, snapshot()?.state.positionSeconds ?? 0)
  }
  const profileFor = (did: string) => profiles()[did] ?? fallbackProfile(did)
  const queuePageSize = 6
  const filteredSongs = createMemo(() => {
    const filterArtist = songFilterArtist().trim().toLowerCase()
    const filterGenre = songFilterGenre().trim().toLowerCase()
    const filterDid = songFilterDid().trim().toLowerCase()
    return (songs() ?? []).filter((song) => {
      if (filterArtist && !song.artist.toLowerCase().includes(filterArtist)) return false
      if (filterGenre && !song.genre?.toLowerCase().includes(filterGenre)) return false
      if (filterDid) {
        const profile = profileFor(song.addedByDid)
        if (!song.addedByDid.toLowerCase().includes(filterDid) && !profile.handle.toLowerCase().includes(filterDid)) return false
      }
      return true
    })
  })
  const songsPaging = createPagedList(filteredSongs, 6)
  const upNextPaging = createPagedList(localQueue, queuePageSize)
  const queueControlPaging = createPagedList(() => snapshot()?.queue ?? [], queuePageSize)
  const albumsPaging = createPagedList(() => albums() ?? [], 6)

  const startListening = async () => {
    if (!audioRef) return
    setHasStarted(true)
    audioRef.volume = volume()
    const snap = snapshot()
    if (snap?.state) {
      await seekAudioTo(Math.max(0, snap.state.positionSeconds))
    }
    // play() must happen synchronously inside the user-gesture call stack on
    // iOS, so do not await Web Audio setup before it. The equalizer graph is
    // attached lazily by EqualizerPanel — routing through MediaElementSource
    // upfront would make iOS suspend the audio when the tab backgrounds.
    void audioRef.play().catch(() => undefined)
  }

  const advanceLocally = () => {
    const queue = localQueue()
    if (queue.length === 0) {
      setLocalCurrentSong(null)
      return
    }
    const next = queue[0]
    consumedQueueIds.add(next.id)
    setLocalCurrentSong(queueItemAsSong(next))
    setLocalQueue(queue.slice(1))

    window.setTimeout(() => {
      if (audioRef && hasStarted()) {
        audioRef.currentTime = 0
        void audioRef.play().catch(() => undefined)
      }
    }, 0)
  }

  const handleAudioEnded = () => advanceLocally()

  // If we ran out of songs and admin later adds one, kick playback again.
  createEffect(() => {
    if (localCurrentSong() === null && localQueue().length > 0 && hasStarted()) {
      advanceLocally()
    }
  })

  createEffect(() => {
    void songFilterArtist()
    void songFilterGenre()
    void songFilterDid()
    songsPaging.setPage(0)
  })

  createEffect(() => {
    writeVolumeCookie(volume())
    if (audioRef) {
      audioRef.volume = volume()
    }
  })


  createEffect(() => {
    if (!('mediaSession' in navigator)) return
    const song = currentSong()
    if (!song) {
      navigator.mediaSession.metadata = null
      return
    }
    navigator.mediaSession.metadata = new MediaMetadata({
      title: song.title,
      artist: song.artist,
      album: song.album ?? undefined,
      artwork: song.hasCover
        ? [{ src: `${API_BASE}/api/songs/${song.id}/cover`, sizes: '512x512', type: 'image/jpeg' }]
        : [],
    })
    navigator.mediaSession.setActionHandler('play', () => void startListening())
    navigator.mediaSession.setActionHandler('pause', () => audioRef?.pause())
    navigator.mediaSession.setActionHandler('stop', () => audioRef?.pause())
  })

  createEffect(() => {
    if (!('mediaSession' in navigator)) return
    if (isAudioPlaying()) {
      navigator.mediaSession.playbackState = 'playing'
    } else if (currentSong()) {
      navigator.mediaSession.playbackState = 'paused'
    } else {
      navigator.mediaSession.playbackState = 'none'
    }
  })

  // iOS Safari can pause a backgrounded <audio> on tab switch / lock screen.
  // When the page becomes visible again, resume if the user previously chose
  // to listen and we still have audio loaded.
  createEffect(() => {
    const onVisible = () => {
      if (document.visibilityState !== 'visible') return
      if (!audioRef || !hasStarted()) return
      if (audioRef.paused && snapshot()?.state.status === 'playing') {
        void audioRef.play().catch(() => undefined)
      }
    }
    document.addEventListener('visibilitychange', onVisible)
    onCleanup(() => document.removeEventListener('visibilitychange', onVisible))
  })

  const sendControl = async (action: 'play' | 'pause' | 'stop' | 'skip') => {
    try {
      setPageError(null)
      mutate(await controlRadio(action, 'explicit_admin_action'))
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'radio control faceplanted.')
    }
  }

  const addSongToQueue = async (songId: string) => {
    try {
      setPageError(null)
      mutate(await enqueueSong(songId))
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'queue add faceplanted.')
    }
  }

  const addAlbumToQueue = async (songIds: string[]) => {
    try {
      setPageError(null)
      mutate(await enqueueAlbum(songIds))
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'album queue add faceplanted.')
    }
  }

  const removeFromQueue = async (queueId: string) => {
    try {
      setPageError(null)
      mutate(await removeQueueItem(queueId))
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'queue remove faceplanted.')
    }
  }

  const clearTheQueue = async () => {
    try {
      setPageError(null)
      mutate(await clearQueue())
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'clear queue faceplanted.')
    }
  }

  const toggleSongSelection = (songId: string, checked: boolean) => {
    setSelectedSongIds((current) => (checked ? [...current, songId] : current.filter((id) => id !== songId)))
  }

  const addSelectedToQueue = async () => {
    const ids = selectedSongIds()
    if (ids.length === 0) return
    try {
      setPageError(null)
      mutate(await enqueueAlbum(ids))
      setSelectedSongIds([])
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'multi add faceplanted.')
    }
  }

  const handleQueueDrop = async (targetQueueId: string) => {
    const sourceId = draggingQueueId()
    setDraggingQueueId(null)
    if (!sourceId || sourceId === targetQueueId) return
    const queue = snapshot()?.queue ?? []
    const ids = queue.map((item) => item.id)
    const sourceIndex = ids.indexOf(sourceId)
    const targetIndex = ids.indexOf(targetQueueId)
    if (sourceIndex < 0 || targetIndex < 0) return
    const reordered = [...ids]
    reordered.splice(sourceIndex, 1)
    reordered.splice(targetIndex, 0, sourceId)
    try {
      setPageError(null)
      mutate(await reorderQueue(reordered))
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'reorder faceplanted.')
    }
  }

  return (
    <>
      <div class="album-ambient" aria-hidden="true">
        <div class="album-ambient-layer" classList={{ active: activeAmbientLayer() === 0 }} style={ambientLayerStyle(ambientLayers()[0])} />
        <div class="album-ambient-layer" classList={{ active: activeAmbientLayer() === 1 }} style={ambientLayerStyle(ambientLayers()[1])} />
      </div>
      <section class="radio-page">
        <div class="now-playing-card">
        <div class="art-shell">
          <Show when={currentSong()?.hasCover} fallback={<div class="art-glow" />}>
            <img class="album-cover" src={`${API_BASE}/api/songs/${currentSong()?.id}/cover`} alt="" />
          </Show>
          <div class="station-id-card" aria-hidden="true">
            <span>goetic relay</span>
            <strong>nkp-ritual band</strong>
            <small>sigil id: {currentSong()?.id.slice(0, 8) ?? 'awaiting'}</small>
          </div>
        </div>
        <div class="nowplaying-waveform" aria-hidden="true">
          <Index each={equalizer.visualizerBars()}>
            {(bar) => <span style={`--wave: ${bar()}`} />}
          </Index>
        </div>
        <p class="eyebrow">now playing // live rite</p>
        <h1>{currentSong()?.title ?? 'nothing playing yet'}</h1>
        <p class="subtitle">{currentSong()?.artist ?? 'queue something lovely'}</p>
        <Show when={currentSong()?.album}>{(album) => <p class="muted">{album()}</p>}</Show>
        <audio
          ref={audioRef}
          class="radio-audio"
          crossOrigin="anonymous"
          src={currentAudioUrl() ?? ''}
          preload="auto"
          onPlay={() => setIsAudioPlaying(true)}
          onPause={() => setIsAudioPlaying(false)}
          onEnded={handleAudioEnded}
        />
        <Show when={nextAudioUrl()}>
          {(url) => <audio src={url()} preload="auto" crossOrigin="anonymous" aria-hidden="true" style="display:none" />}
        </Show>
        <div class="listener-controls">
          <div class="listen-attribution-row">
            <Show when={currentSong() && snapshot()?.state.status === 'playing'}>
              <button class="listen-button" type="button" onClick={() => void startListening()}>
                {isAudioPlaying() ? 'listening live' : 'tap to listen'}
              </button>
            </Show>
            <Show when={currentSong()}>
              {(song) => {
                const profile = () => profileFor(song().addedByDid)
                return (
                  <a class="track-attribution" href={`https://bsky.app/profile/${profile().handle}`} target="_blank" rel="noreferrer">
                    <ProfileAvatar profile={profile()} class="track-attribution-avatar" title={`@${profile().handle}`} />
                    <span>
                      uploaded by <strong>{profile().displayName || `@${profile().handle}`}</strong>
                    </span>
                  </a>
                )
              }}
            </Show>
          </div>
          <label class="volume-control local-volume">
            <Volume2 size={17} />
            <input
              type="range"
              min="0"
              max="1"
              step="0.01"
              value={volume()}
              style={`--volume-progress: ${volume() * 100}%`}
              onInput={(event) => {
                setVolume(event.currentTarget.valueAsNumber)
                if (audioRef) {
                  audioRef.volume = event.currentTarget.valueAsNumber
                }
              }}
            />
          </label>
        </div>
      </div>

      <aside class="radio-panel">
        <section class="glass-card chat-preview">
          <p class="eyebrow">chat</p>
          <p class="muted">coming later</p>
        </section>

        <section class="glass-card up-next-card">
          <div class="section-heading">
            <p class="eyebrow">up next</p>
            <span>{snapshot()?.state.status ?? 'loading'}</span>
          </div>
          <Show when={!snapshot.loading} fallback={<p class="muted">loading queue...</p>}>
            <ul class="queue-list">
              <For each={upNextPaging.paged()} fallback={<li class="muted">queue is empty</li>}>
                {(item, index) => {
                  const profile = () => profileFor(item.queuedByDid)
                  const hasCover = () => (songs() ?? []).some((song) => song.id === item.songId && song.hasCover)
                  return (
                    <li>
                      <span class="queue-number">{upNextPaging.page() * queuePageSize + index() + 1}</span>
                      <SongCoverThumb songId={item.songId} hasCover={hasCover()} />
                      <div class="up-next-copy">
                        <span class="up-next-title">{item.title}</span>
                        <small class="up-next-artist">{item.artist || 'unknown artist'}</small>
                      </div>
                      <ProfileAvatar profile={profile()} class="up-next-profile-avatar" title={`@${profile().handle}`} />
                    </li>
                  )
                }}
              </For>
            </ul>
            <Show when={upNextPaging.pageCount() > 1}>
              <PaginationRow page={upNextPaging.page()} pageCount={upNextPaging.pageCount()} onPageChange={upNextPaging.setPage} compact />
            </Show>
          </Show>
        </section>

        <section class="glass-card equalizer-card">
          <EqualizerPanel controller={equalizer} />
        </section>

        <Show when={props.isAdmin}>
          <button
            class="admin-visibility-toggle"
            type="button"
            aria-pressed={!showAdminTools()}
            onClick={() => setShowAdminTools((visible) => !visible)}
          >
            <span>{showAdminTools() ? 'hide admin' : 'show admin'}</span>
            <Show when={showAdminTools()} fallback={<Eye size={17} />}>
              <EyeOff size={17} />
            </Show>
          </button>
        </Show>

        <Show when={props.isAdmin && showAdminTools()}>
          <AdminUploadPanel
            onTransport={(action) => void sendControl(action)}
            onSongAdded={() => void refetchSongs()}
            error={pageError()}
            onError={setPageError}
          />

          <section class="glass-card">
            <div class="section-heading">
              <p class="eyebrow">queue control</p>
              <Show
                when={(snapshot()?.queue.length ?? 0) > 0}
                fallback={<span>0</span>}
              >
                <button class="pill-button subtle clear-queue-pill" type="button" onClick={() => void clearTheQueue()}>
                  clear ({snapshot()?.queue.length})
                </button>
              </Show>
            </div>
            <Show when={currentSong()}>
              {(song) => (
                <div class="queue-progress">
                  <span>{song().title}</span>
                  <small>
                    {formatTime(Math.min(livePositionSeconds(), song().durationSeconds ?? Infinity))} / {formatTime(song().durationSeconds)}
                  </small>
                </div>
              )}
            </Show>
            <ul class="song-list">
              <For each={queueControlPaging.paged()} fallback={<li class="list-empty">queue is empty</li>}>
                {(item) => {
                  const profile = () => profileFor(item.queuedByDid)
                  return (
                    <li
                      draggable={true}
                      classList={{ 'queue-drag-source': draggingQueueId() === item.id }}
                      onDragStart={(e) => {
                        setDraggingQueueId(item.id)
                        e.dataTransfer?.setData('text/plain', item.id)
                      }}
                      onDragOver={(e) => e.preventDefault()}
                      onDrop={(e) => {
                        e.preventDefault()
                        void handleQueueDrop(item.id)
                      }}
                      onDragEnd={() => setDraggingQueueId(null)}
                    >
                      <ProfileAvatar profile={profile()} />
                      <div class="song-copy">
                        <span>{item.title}</span>
                        <small>{item.artist}</small>
                      </div>
                      <small class="profile-handle">@{profile().handle}</small>
                      <button class="icon-button" type="button" aria-label="remove from queue" onClick={() => void removeFromQueue(item.id)}>
                        <Trash2 size={17} />
                      </button>
                    </li>
                  )
                }}
              </For>
            </ul>
            <Show when={queueControlPaging.pageCount() > 1}>
              <PaginationRow page={queueControlPaging.page()} pageCount={queueControlPaging.pageCount()} onPageChange={queueControlPaging.setPage} compact />
            </Show>
          </section>
        </Show>
      </aside>

      <Show when={props.isAdmin && showAdminTools()}>
        <section class="bottom-radio-tools">
          <section class="glass-card">
            <div class="section-heading">
              <p class="eyebrow">songs added</p>
              <span>{filteredSongs().length}{filteredSongs().length !== (songs()?.length ?? 0) ? ` / ${songs()?.length ?? 0}` : ''}</span>
            </div>
            <div class="song-filters">
              <input
                placeholder="artist"
                value={songFilterArtist()}
                onInput={(e) => setSongFilterArtist(e.currentTarget.value)}
              />
              <input
                placeholder="genre"
                value={songFilterGenre()}
                onInput={(e) => setSongFilterGenre(e.currentTarget.value)}
              />
              <input
                placeholder="@handle or did"
                value={songFilterDid()}
                onInput={(e) => setSongFilterDid(e.currentTarget.value)}
              />
            </div>
            <Show when={selectedSongIds().length > 0}>
              <div class="multi-add-row">
                <button class="pill-button" type="button" onClick={() => void addSelectedToQueue()}>
                  add {selectedSongIds().length} to queue
                </button>
                <button class="pill-button subtle" type="button" onClick={() => setSelectedSongIds([])}>
                  clear selection
                </button>
              </div>
            </Show>
            <Show when={!songs.loading} fallback={<p class="list-empty">loading songs...</p>}>
              <ul class="song-list">
                <For each={songsPaging.paged()} fallback={<li class="list-empty">no songs added yet</li>}>
                  {(song) => {
                    const profile = () => profileFor(song.addedByDid)
                    return (
                      <li>
                        <label class="multi-add-cell">
                          <input
                            type="checkbox"
                            checked={selectedSongIds().includes(song.id)}
                            onChange={(event) => toggleSongSelection(song.id, event.currentTarget.checked)}
                          />
                          <ProfileAvatar profile={profile()} />
                        </label>
                        <div class="song-copy">
                          <span>{song.title}</span>
                          <small>{song.artist}{song.genre ? ` · ${song.genre}` : ''}</small>
                        </div>
                        <small class="profile-handle">@{profile().handle}</small>
                        <button class="icon-button" type="button" aria-label="add to queue" onClick={() => void addSongToQueue(song.id)}>
                          <ListPlus size={18} />
                        </button>
                      </li>
                    )
                  }}
                </For>
              </ul>
              <Show when={songsPaging.pageCount() > 1}>
                <PaginationRow page={songsPaging.page()} pageCount={songsPaging.pageCount()} onPageChange={songsPaging.setPage} />
              </Show>
            </Show>
          </section>

          <section class="glass-card">
            <div class="section-heading">
              <p class="eyebrow">queue albums</p>
              <span>{albums()?.length ?? 0}</span>
            </div>
            <Show when={!albums.loading} fallback={<p class="list-empty">loading albums...</p>}>
              <ul class="song-list album-loop-list">
                <For each={albumsPaging.paged()} fallback={<li class="list-empty">no album loops yet</li>}>
                  {(album) => (
                    <li>
                      <div class="song-copy">
                        <span>{album.title}</span>
                        <small>{album.tracks.length} tracks · {album.tracks.map((track) => track.title).join(' → ')}</small>
                      </div>
                      <button
                        class="icon-button"
                        type="button"
                        aria-label="queue album"
                        disabled={album.tracks.length === 0}
                        onClick={() => void addAlbumToQueue(album.tracks.map((track) => track.id))}
                      >
                        <ListPlus size={18} />
                      </button>
                    </li>
                  )}
                </For>
              </ul>
              <Show when={albumsPaging.pageCount() > 1}>
                <PaginationRow page={albumsPaging.page()} pageCount={albumsPaging.pageCount()} onPageChange={albumsPaging.setPage} />
              </Show>
            </Show>
          </section>
        </section>
        </Show>
      </section>
    </>
  )
}
