import { createEffect, createResource, createSignal, For, Index, onCleanup, Show, untrack } from 'solid-js'
import { Eye, Send, Volume2 } from 'lucide-solid'
import { resolveAtprotoProfile, type AtprotoProfile } from '../../shared/lib/atproto'
import {
  API_BASE,
  fetchRadioSnapshot,
  fetchSongs,
  getListenerOptOut,
  getRadioViewerId,
  getSessionToken,
  MAX_CHAT_BODY_LEN,
  openChatSocket,
  openRadioSocket,
  sendChatMessage,
  sendRadioViewerHello,
  sendRadioViewerKeepalive,
  type ChatEvent,
  type ChatMessage,
  type QueueItem,
  type RadioEvent,
  type RadioState,
  type Song,
} from '../../shared/lib/radio'
import { fetchSession } from '../../shared/lib/auth'
import { PaginationRow } from '../../shared/components/PaginationRow'
import { ProfileAvatar } from '../../shared/components/ProfileAvatar'
import { SongCoverThumb } from '../../shared/components/SongCoverThumb'
import { EqualizerPanel, createEqualizerController } from '../../features/equalizer/EqualizerPanel'
import { createPagedList } from '../../shared/primitives/createPagedList'

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

function setElementVolume(audioElement: HTMLAudioElement, nextVolume: number): boolean {
  audioElement.volume = nextVolume
  return Math.abs(audioElement.volume - nextVolume) < 0.001
}

// Mobile browsers suspend <audio> in the background once it's routed through a
// Web Audio graph (MediaElementSource). iOS Safari is strict about it; Android
// is inconsistent but vulnerable on lock screen / battery saver. On mobile we
// defer attaching the equalizer graph until the user opts in. Desktop keeps
// visualizer + EQ from the start.
export function isMobileDevice(): boolean {
  if (typeof navigator === 'undefined') return false
  if (/Android|iPad|iPhone|iPod|Mobi/i.test(navigator.userAgent)) return true
  // iPadOS reports as Macintosh; touch points disambiguate it from real Macs.
  return /Macintosh/.test(navigator.userAgent) && navigator.maxTouchPoints > 1
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

function themeColorFromAccent(accent: AlbumAccent): string {
  const [r, g, b] = accent.primary.split(' ').map(Number)
  if (![r, g, b].every(Number.isFinite)) return '#1e1e1e'
  // Blend the primary toward #1e1e1e ~78% so the status bar reads as a darkened
  // tint of the album rather than the saturated color.
  const blend = (channel: number, base: number) => Math.round(channel * 0.22 + base * 0.78)
  const toHex = (value: number) => value.toString(16).padStart(2, '0')
  return `#${toHex(blend(r, 0x1e))}${toHex(blend(g, 0x1e))}${toHex(blend(b, 0x1e))}`
}

function setMetaThemeColor(color: string): void {
  const existing = document.querySelector<HTMLMetaElement>('meta[name="theme-color"]')
  const meta = existing ?? document.createElement('meta')
  meta.name = 'theme-color'
  meta.content = color
  if (!existing) document.head.append(meta)
}

function accentFromRgb(primary: { red: number; green: number; blue: number }, secondary: { red: number; green: number; blue: number }): AlbumAccent {
  return {
    primary: `${primary.red} ${primary.green} ${primary.blue}`,
    secondary: `${secondary.red} ${secondary.green} ${secondary.blue}`,
    primaryWash: `rgb(${primary.red} ${primary.green} ${primary.blue} / 30%)`,
    secondaryWash: `rgb(${secondary.red} ${secondary.green} ${secondary.blue} / 24%)`,
    topWash: `rgb(${primary.red} ${primary.green} ${primary.blue} / 12%)`,
  }
}

function ambientImageDataUrl(accent: AlbumAccent): string {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1200 1800" preserveAspectRatio="none"><defs><radialGradient id="glow" cx="50%" cy="40%" r="42%"><stop offset="0" stop-color="rgb(${accent.primary})" stop-opacity="0.62"/><stop offset="0.5" stop-color="rgb(${accent.primary})" stop-opacity="0.26"/><stop offset="0.85" stop-color="rgb(${accent.primary})" stop-opacity="0.04"/><stop offset="1" stop-color="rgb(${accent.primary})" stop-opacity="0"/></radialGradient><radialGradient id="warm" cx="50%" cy="40%" r="28%"><stop offset="0" stop-color="rgb(${accent.secondary})" stop-opacity="0.4"/><stop offset="1" stop-color="rgb(${accent.secondary})" stop-opacity="0"/></radialGradient></defs><rect width="1200" height="1800" fill="url(#glow)"/><rect width="1200" height="1800" fill="url(#warm)"/></svg>`
  return `data:image/svg+xml,${encodeURIComponent(svg)}`
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
 * Renders the public listener radio view.
 * @returns The radio page view.
 */
export default function RadioPage() {
  const [snapshot, { mutate, refetch }] = createResource(fetchRadioSnapshot)
  const [songs, { refetch: refetchSongs }] = createResource(fetchSongs)
  const [profiles, setProfiles] = createSignal<Record<string, AtprotoProfile>>({})
  const inFlightDids = new Set<string>()
  const [volume, setVolume] = createSignal(readVolumeCookie())
  const [hasStarted, setHasStarted] = createSignal(false)
  const [isAudioPlaying, setIsAudioPlaying] = createSignal(false)
  const [viewerCount, setViewerCount] = createSignal(0)
  const [listenerDids, setListenerDids] = createSignal<string[]>([])
  const [chatMessages, setChatMessages] = createSignal<ChatMessage[]>([])
  const [chatDraft, setChatDraft] = createSignal('')
  const [chatConnected, setChatConnected] = createSignal(false)
  let chatSocket: WebSocket | null = null
  let chatLogRef: HTMLDivElement | undefined
  const [listenerOverflowOpen, setListenerOverflowOpen] = createSignal(false)
  const MAX_VISIBLE_LISTENERS = 8
  const visibleListenerDids = () => listenerDids().slice(0, MAX_VISIBLE_LISTENERS)
  const overflowListenerDids = () => listenerDids().slice(MAX_VISIBLE_LISTENERS)
  const [albumAccent, setAlbumAccent] = createSignal<AlbumAccent>(DEFAULT_ALBUM_ACCENT)
  const [ambientLayers, setAmbientLayers] = createSignal<[AlbumAccent, AlbumAccent]>([DEFAULT_ALBUM_ACCENT, DEFAULT_ALBUM_ACCENT])
  const [activeAmbientLayer, setActiveAmbientLayer] = createSignal<0 | 1>(0)
  let lastAmbientKey = ''
  const [volumeOverlayActive, setVolumeOverlayActive] = createSignal(false)
  let volumeOverlayTimeout: ReturnType<typeof setTimeout> | null = null
  let volumeOverlayInitialized = false

  const volumeMeterChars = () => {
    const v = Math.max(0, Math.min(1, volume()))
    const cells = 14
    const filled = Math.round(v * cells)
    return '█'.repeat(filled) + '▁'.repeat(cells - filled)
  }

  // Local playback state. Frontend self-advances through localQueue; backend
  // resyncs only on admin actions (detected via playbackKey diff).
  const [localCurrentSong, setLocalCurrentSong] = createSignal<Song | null>(null)
  const [localQueue, setLocalQueue] = createSignal<QueueItem[]>([])
  let consumedQueueIds = new Set<string>()
  let lastPlaybackKey: string | null = null
  let audioRef: HTMLAudioElement | undefined
  const onMobile = isMobileDevice()
  const equalizer = createEqualizerController(() => audioRef)
  const viewerId = getRadioViewerId()
  const [session] = createResource(fetchSession)
  // Re-read on socket events so an in-tab opt-out toggle takes effect on next
  // keepalive without forcing the user to reload.
  const listenerDid = (): string | null => {
    if (getListenerOptOut()) return null
    return session()?.accountDid ?? null
  }

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
        if (socket) {
          sendRadioViewerHello(socket, viewerId, listenerDid())
        }
      })

      socket.addEventListener('message', (message) => {
        const event = JSON.parse(message.data) as RadioEvent
        if (event.type === 'snapshotChanged') {
          mutate(event.snapshot)
          // Don't refetch songs/albums here — most snapshot events are queue
          // mutations that don't affect the library. Local actions that do
          // change the library refetch explicitly.
        } else if (event.type === 'viewerCountChanged') {
          const count = event.viewerCount ?? event.viewer_count
          if (typeof count === 'number' && Number.isFinite(count)) {
            setViewerCount(count)
          }
          const dids = event.listenerDids ?? event.listener_dids
          if (Array.isArray(dids)) {
            setListenerDids(dids.filter((value): value is string => typeof value === 'string'))
          }
        } else if (event.type === 'viewerKeepalive' && socket?.readyState === WebSocket.OPEN) {
          sendRadioViewerKeepalive(socket, viewerId, listenerDid())
        }
      })

      const scheduleReconnect = () => {
        if (cancelled || reconnectTimer !== null) return
        // Refetch on every drop so state stays fresh while we wait to reopen.
        void refetch()
        void refetchSongs()
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
    let reconnectTimer: number | null = null
    let reconnectAttempt = 0
    let cancelled = false

    const connect = () => {
      if (cancelled) return
      chatSocket = openChatSocket()

      chatSocket.addEventListener('open', () => {
        reconnectAttempt = 0
        setChatConnected(true)
      })

      chatSocket.addEventListener('message', (message) => {
        const event = JSON.parse(message.data) as ChatEvent
        if (event.type === 'history') {
          setChatMessages(event.messages)
        } else if (event.type === 'message') {
          setChatMessages((current) => [...current, event.message])
        } else if (event.type === 'messageDeleted') {
          setChatMessages((current) => current.filter((entry) => entry.id !== event.id))
        } else if (event.type === 'messagesPurged') {
          setChatMessages((current) => current.filter((entry) => entry.senderDid !== event.senderDid))
        }
      })

      const scheduleReconnect = () => {
        setChatConnected(false)
        if (cancelled || reconnectTimer !== null) return
        const delay = Math.min(30000, 500 * 2 ** Math.min(reconnectAttempt, 6))
        reconnectAttempt += 1
        reconnectTimer = window.setTimeout(() => {
          reconnectTimer = null
          connect()
        }, delay)
      }

      chatSocket.addEventListener('close', scheduleReconnect)
      chatSocket.addEventListener('error', () => chatSocket?.close())
    }

    connect()

    onCleanup(() => {
      cancelled = true
      if (reconnectTimer !== null) window.clearTimeout(reconnectTimer)
      chatSocket?.close()
      chatSocket = null
    })
  })

  // Auto-scroll the chat log to the newest message when it changes.
  createEffect(() => {
    chatMessages()
    if (chatLogRef) {
      queueMicrotask(() => {
        if (chatLogRef) chatLogRef.scrollTop = chatLogRef.scrollHeight
      })
    }
  })

  const canSendChat = () => Boolean(session()?.accountDid) && chatConnected()

  const submitChat = () => {
    const text = chatDraft().trim()
    if (!text) return
    if (!chatSocket || chatSocket.readyState !== WebSocket.OPEN) return
    const token = getSessionToken()
    if (!token) return
    sendChatMessage(chatSocket, text.slice(0, MAX_CHAT_BODY_LEN), token)
    setChatDraft('')
  }

  const formatChatTime = (createdAt: number): string => {
    const date = new Date(createdAt * 1000)
    return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
  }

  createEffect(() => {
    const senders = chatMessages()
      .filter((message) => message.kind === 'user')
      .map((message) => message.senderDid)
    const dids = senders.filter(
      (did, index, values) => values.indexOf(did) === index && !profiles()[did] && !inFlightDids.has(did),
    )
    for (const did of dids) {
      inFlightDids.add(did)
      void resolveAtprotoProfile(did)
        .then((profile) => setProfiles((current) => ({ ...current, [did]: profile })))
        .finally(() => inFlightDids.delete(did))
    }
  })

  createEffect(() => {
    const currentSongDid = snapshot()?.currentSong?.addedByDid
    const dids = [
      ...(songs() ?? []).map((song) => song.addedByDid),
      ...(currentSongDid ? [currentSongDid] : []),
      ...(snapshot()?.queue ?? []).flatMap((item) => [item.addedByDid, item.queuedByDid]),
      ...listenerDids(),
    ].filter((did, index, values) => values.indexOf(did) === index && !profiles()[did] && !inFlightDids.has(did))

    for (const did of dids) {
      inFlightDids.add(did)
      void resolveAtprotoProfile(did)
        .then((profile) => setProfiles((current) => ({ ...current, [did]: profile })))
        .finally(() => inFlightDids.delete(did))
    }
  })

  const currentSong = () => localCurrentSong()
  const currentSongTitle = () => currentSong()?.title ?? 'nothing playing yet'
  const shouldMarqueeTitle = () => currentSongTitle().length > 25
  const viewerCountValue = () => viewerCount() ?? 0
  const viewerCountLabel = () => viewerCountValue().toString()

  createEffect(() => {
    const song = currentSong()
    equalizer.setLoudness({
      lufs: song?.loudnessLufs ?? null,
      peak: song?.loudnessPeak ?? null,
    })
  })

  createEffect(() => {
    const song = currentSong()
    if (!song?.hasCover) {
      setAlbumAccent(DEFAULT_ALBUM_ACCENT)
      return
    }

    let cancelled = false
    const image = new Image()
    image.crossOrigin = 'anonymous'
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

    setMetaThemeColor(themeColorFromAccent(accent))
  })

  const ambientImageSrc = (accent: AlbumAccent) => ambientImageDataUrl(accent)
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
  const profileFor = (did: string) => profiles()[did] ?? fallbackProfile(did)

  const chatCard = () => (
    <section class="glass-card chat-card">
      <div class="section-heading">
        <p class="eyebrow">chat</p>
        <span>{chatConnected() ? 'live' : 'offline'}</span>
      </div>
      <div class="chat-log" ref={chatLogRef}>
        <Show
          when={chatMessages().length > 0}
          fallback={<p class="muted chat-empty">no messages yet</p>}
        >
          <ul class="chat-message-list">
            <For each={chatMessages()}>
              {(message) => {
                if (message.kind === 'now_playing') {
                  return (
                    <li class="chat-now-playing">
                      <span class="chat-now-playing-label">now playing</span>
                      <span class="chat-now-playing-body">{message.body}</span>
                      <span class="chat-message-time">{formatChatTime(message.createdAt)}</span>
                    </li>
                  )
                }
                const profile = () => profileFor(message.senderDid)
                return (
                  <li class="chat-message">
                    <a
                      class="chat-message-avatar"
                      href={`https://bsky.app/profile/${profile().handle}`}
                      target="_blank"
                      rel="noreferrer"
                    >
                      <ProfileAvatar profile={profile()} class="chat-avatar" title={`@${profile().handle}`} />
                    </a>
                    <div class="chat-message-body">
                      <div class="chat-message-meta">
                        <span class="chat-message-handle">
                          {profile().displayName || `@${profile().handle}`}
                        </span>
                        <span class="chat-message-time">{formatChatTime(message.createdAt)}</span>
                      </div>
                      <p class="chat-message-text">{message.body}</p>
                    </div>
                  </li>
                )
              }}
            </For>
          </ul>
        </Show>
      </div>
      <form
        class="chat-composer"
        onSubmit={(event) => {
          event.preventDefault()
          submitChat()
        }}
      >
        <Show
          when={session()?.accountDid}
          fallback={<p class="muted chat-empty">log in to chat</p>}
        >
          <input
            class="chat-input"
            type="text"
            placeholder="say something nice"
            maxlength={MAX_CHAT_BODY_LEN}
            value={chatDraft()}
            onInput={(event) => setChatDraft(event.currentTarget.value)}
            disabled={!chatConnected()}
          />
          <button
            class="chat-send"
            type="submit"
            aria-label="send"
            disabled={!canSendChat() || chatDraft().trim().length === 0}
          >
            <Send size={16} />
          </button>
        </Show>
      </form>
    </section>
  )

  const queuePageSize = 6
  const upNextPaging = createPagedList(localQueue, queuePageSize)

  const startListening = async () => {
    if (!audioRef) return
    setHasStarted(true)
    if (!setElementVolume(audioRef, volume())) {
      equalizer.setOutputVolume(volume())
      void equalizer.ensureGraph()
    }
    const snap = snapshot()
    if (snap?.state) {
      await seekAudioTo(Math.max(0, snap.state.positionSeconds))
    }
    // play() must happen synchronously inside the user-gesture call stack on
    // mobile, so do not await Web Audio setup before it.
    void audioRef.play().catch(() => undefined)
    // On desktop, attach the equalizer graph upfront so visualizer + EQ work
    // immediately. On mobile we defer until the user opens the EQ panel —
    // routing through MediaElementSource makes the OS suspend audio when the
    // tab backgrounds / screen locks.
    if (!isMobileDevice()) {
      void equalizer.ensureGraph()
    }
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
    const nextVolume = volume()
    writeVolumeCookie(nextVolume)
    equalizer.setOutputVolume(nextVolume)
    if (audioRef && !setElementVolume(audioRef, nextVolume) && hasStarted()) {
      void equalizer.ensureGraph()
    }
  })


  createEffect(() => {
    const song = currentSong()
    document.title = song ? `${song.title} - ${song.artist}` : 'radio'

    if (!('mediaSession' in navigator)) return
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

  createEffect(() => {
    if (!listenerOverflowOpen()) return
    const onDocClick = (event: MouseEvent) => {
      const target = event.target as HTMLElement | null
      if (target && target.closest('.listener-avatars')) return
      setListenerOverflowOpen(false)
    }
    document.addEventListener('click', onDocClick)
    onCleanup(() => document.removeEventListener('click', onDocClick))
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

  // Show the CRT volume meter overlay briefly whenever volume changes (skip
  // the initial mount so it doesn't flash on page load).
  createEffect(() => {
    volume()
    if (!volumeOverlayInitialized) {
      volumeOverlayInitialized = true
      return
    }
    setVolumeOverlayActive(true)
    if (volumeOverlayTimeout) clearTimeout(volumeOverlayTimeout)
    volumeOverlayTimeout = setTimeout(() => setVolumeOverlayActive(false), 1400)
  })
  onCleanup(() => {
    if (volumeOverlayTimeout) clearTimeout(volumeOverlayTimeout)
  })

  return (
    <>
      <div class="album-ambient" aria-hidden="true">
        <div class="album-ambient-layer" classList={{ active: activeAmbientLayer() === 0 }} style={ambientLayerStyle(ambientLayers()[0])} />
        <div class="album-ambient-layer" classList={{ active: activeAmbientLayer() === 1 }} style={ambientLayerStyle(ambientLayers()[1])} />
      </div>
      <img class="album-ambient-image" classList={{ active: activeAmbientLayer() === 0 }} src={ambientImageSrc(ambientLayers()[0])} alt="" aria-hidden="true" />
      <img class="album-ambient-image" classList={{ active: activeAmbientLayer() === 1 }} src={ambientImageSrc(ambientLayers()[1])} alt="" aria-hidden="true" />
      <section class="radio-page">
        <div class="now-playing-card">
        <div class="art-shell">
          <Show when={currentSong()?.id ?? ''} keyed>
            {(songId) => (
              <>
                <Show when={currentSong()?.hasCover} fallback={<div class="art-glow" />}>
                  <img class="album-cover" src={`${API_BASE}/api/songs/${songId}/cover`} alt="" />
                </Show>
                <div class="art-crt-scanload" aria-hidden="true" />
              </>
            )}
          </Show>
          <div class="art-crt-sweep" aria-hidden="true" />
          <div class="art-crt-scanlines" aria-hidden="true" />
          <div class="art-crt-vignette" aria-hidden="true" />
          <Show when={volumeOverlayActive()}>
            <div class="art-crt-volume" aria-hidden="true">
              <span class="art-crt-volume-label">vol</span>
              <span class="art-crt-volume-bar">{volumeMeterChars()}</span>
            </div>
          </Show>
          <div class="station-id-card" aria-hidden="true">
            <small>sigil id: {currentSong()?.id.slice(0, 8) ?? 'awaiting'}</small>
          </div>
        </div>
        <div class="nowplaying-waveform" aria-hidden="true">
          <Index each={equalizer.visualizerBars()}>
            {(bar) => <span style={`--wave: ${bar()}`} />}
          </Index>
        </div>
        <p class="eyebrow">now playing // live rite</p>
        <h1 classList={{ marquee: shouldMarqueeTitle() }} title={currentSongTitle()}>
          <Show
            when={shouldMarqueeTitle()}
            fallback={currentSongTitle()}
          >
            <span class="marquee-track">
              <span>{currentSongTitle()}</span>
              <span aria-hidden="true">{currentSongTitle()}</span>
            </span>
          </Show>
        </h1>
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
          <div class="live-viewer-counter" aria-live="polite">
            <Eye size={16} />
            <span>{viewerCountLabel()}</span>
            <span>{viewerCountValue() === 1 ? 'listener' : 'listeners'}</span>
            <Show when={listenerDids().length > 0}>
              <ul class="listener-avatars" aria-label="listeners">
                <For each={visibleListenerDids()}>
                  {(did) => {
                    const profile = () => profileFor(did)
                    return (
                      <li>
                        <a
                          href={`https://bsky.app/profile/${profile().handle}`}
                          target="_blank"
                          rel="noreferrer"
                          data-handle={`@${profile().handle}`}
                        >
                          <ProfileAvatar profile={profile()} class="listener-avatar" title={`@${profile().handle}`} />
                        </a>
                      </li>
                    )
                  }}
                </For>
                <Show when={overflowListenerDids().length > 0}>
                  <li>
                    <button
                      type="button"
                      class="listener-avatar-more"
                      aria-expanded={listenerOverflowOpen()}
                      aria-label={`show ${overflowListenerDids().length} more listeners`}
                      data-handle={`+${overflowListenerDids().length} more`}
                      onClick={() => setListenerOverflowOpen((open) => !open)}
                    >
                      +{overflowListenerDids().length}
                    </button>
                  </li>
                  <Show when={listenerOverflowOpen()}>
                    <ul class="listener-avatars-overflow" aria-label="more listeners">
                      <For each={overflowListenerDids()}>
                        {(did) => {
                          const profile = () => profileFor(did)
                          return (
                            <li>
                              <a
                                href={`https://bsky.app/profile/${profile().handle}`}
                                target="_blank"
                                rel="noreferrer"
                              >
                                <ProfileAvatar profile={profile()} class="listener-avatar" title={`@${profile().handle}`} />
                                <span>{profile().displayName || `@${profile().handle}`}</span>
                              </a>
                            </li>
                          )
                        }}
                      </For>
                    </ul>
                  </Show>
                </Show>
              </ul>
            </Show>
          </div>
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
                const nextVolume = event.currentTarget.valueAsNumber
                setVolume(nextVolume)
                equalizer.setOutputVolume(nextVolume)
                if (audioRef && !setElementVolume(audioRef, nextVolume) && hasStarted()) {
                  void equalizer.ensureGraph()
                }
              }}
            />
          </label>
        </div>
        {onMobile && chatCard()}
      </div>

      <Show when={!onMobile}>
        <aside class="radio-panel">
          {chatCard()}

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
        </aside>
      </Show>
      </section>
    </>
  )
}
