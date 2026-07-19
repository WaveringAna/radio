import { createEffect, createMemo, createResource, createSignal, For, onCleanup, Show } from 'solid-js'
import {
  CircleUserRound,
  GripVertical,
  ListPlus,
  LoaderCircle,
  LockKeyhole,
  Pause,
  Play,
  RadioTower,
  SkipForward,
  Shuffle,
  Trash2,
  UploadCloud,
  X,
  Search,
  Clock,
  ChevronUp,
  ChevronDown,
} from 'lucide-solid'
import { AdminUploadPanel } from '../../features/upload/AdminUploadPanel'
import { ChatModerationPanel } from './ChatModerationPanel'
import { PaginationRow } from '../../shared/components/PaginationRow'
import { resolveAtprotoProfile, type AtprotoProfile } from '../../shared/lib/atproto'
import { fetchAdminPermissions, type SessionResponse } from '../../shared/lib/auth'
import {
  canUseRadioXrpcTarget,
  clearQueue,
  controlRadio,
  createPlaylist,
  deleteSong,
  enqueueAlbum,
  enqueueSong,
  fetchSyndicatedStations,
  fetchAlbums,
  deleteAlbum,
  setAlbumEnabled,
  mergeAlbums,
  fetchRadioSnapshot,
  fetchSongs,
  openRadioSocket,
  removeQueueItem,
  reorderQueue,
  updateSongMetadata,
  uploadSongCover,
  fetchPlaylists,
  deletePlaylist,
  loadPlaylist,
  songCoverThumbnailUrl,
  songCoverUrl,
  SYNDICATION_WORKER_BASE,
  type QueueItem,
  type RadioEvent,
  type RadioTarget,
  type Song,
} from '../../shared/lib/radio'
import { SearchableDropdown } from '../../shared/components/SearchableDropdown'
import {
  isPlaceholderStation,
  labelFromStationUrl,
  readSelectedStationUrl,
  selectedTuneInStationFrom,
  stationRadioTarget,
  stationResourceKey,
  TUNE_IN_CHANGED_EVENT,
  tuneInStationsFrom,
  type TuneInStation,
} from '../../shared/lib/stationSelection'
import { createPagedList } from '../../shared/primitives/createPagedList'

interface QueueControlPageProps {
  session?: SessionResponse
  sessionLoading: boolean
}

type SearchMode = 'songs' | 'albums' | 'playlists'
type LibraryAction = 'queue' | 'edit'

function fallbackProfile(did: string): AtprotoProfile {
  return { did, handle: did }
}

function formatTime(seconds: number | null | undefined): string {
  if (!seconds || seconds < 0) return '0:00'
  const minutes = Math.floor(seconds / 60)
  const remainder = Math.floor(seconds % 60).toString().padStart(2, '0')
  return `${minutes}:${remainder}`
}

/**
 * Renders the admin queue cockpit with playback, queue, upload, and library search tools.
 * @param props Current viewer permissions.
 * @returns The queue-control admin page view.
 */
export default function QueueControlPage(props: QueueControlPageProps) {
  const [selectedStationUrl, setSelectedStationUrl] = createSignal(readSelectedStationUrl())
  const [syndicatedStations] = createResource(
    () => SYNDICATION_WORKER_BASE || 'disabled',
    (workerBase) => workerBase === 'disabled' ? Promise.resolve([]) : fetchSyndicatedStations(workerBase),
  )
  const tuneInStations = (): TuneInStation[] => tuneInStationsFrom(syndicatedStations() ?? [])
  const selectedStation = createMemo<TuneInStation>(() => selectedTuneInStationFrom(tuneInStations(), selectedStationUrl()))
  const selectedApiBase = () => selectedStation().apiBase
  const selectedStationKey = () => stationResourceKey(selectedStation())
  const selectedRadioTarget = (): RadioTarget => stationRadioTarget(selectedStation())
  const selectedRadioCanUseXrpc = () => canUseRadioXrpcTarget(selectedRadioTarget())
  const adminProbeSource = () => props.session?.authenticated && selectedRadioCanUseXrpc() && !isPlaceholderStation(selectedStation()) ? selectedStationKey() : null
  const [adminStatus] = createResource(adminProbeSource, async () => {
    try {
      await fetchAdminPermissions(selectedRadioTarget())
      return { isAdmin: true, message: null as string | null }
    } catch (error) {
      console.warn('radio admin probe failed', error)
      return {
        isAdmin: false,
        message: 'you are not an admin of the radio you are currently tuned into.',
      }
    }
  })
  const isAdmin = () => adminStatus()?.isAdmin === true
  const adminResourceSource = () => isAdmin() ? selectedStationKey() : null
  const [snapshot, { mutate, refetch }] = createResource(adminResourceSource, () => fetchRadioSnapshot(selectedRadioTarget(), true))
  const [songs, { refetch: refetchSongs }] = createResource(adminResourceSource, () => fetchSongs(selectedRadioTarget(), true))
  const [albums, { refetch: refetchAlbums }] = createResource(adminResourceSource, () => fetchAlbums(selectedRadioTarget()))
  const [playlists, { refetch: refetchPlaylists }] = createResource(adminResourceSource, () => fetchPlaylists(selectedRadioTarget()))
  const [profiles, setProfiles] = createSignal<Record<string, AtprotoProfile>>({})
  const [pageError, setPageError] = createSignal<string | null>(null)
  const [songFilterTitle, setSongFilterTitle] = createSignal('')
  const [songFilterArtist, setSongFilterArtist] = createSignal('')
  const [songFilterGenre, setSongFilterGenre] = createSignal('')
  const [albumFilter, setAlbumFilter] = createSignal('')
  const [expandedAlbumId, setExpandedAlbumId] = createSignal<string | null>(null)
  const normalizeTitleForUi = (title: string): string => {
    return title
      .normalize('NFD')
      .replace(/[\u0300-\u036f]/g, '')
      .toLowerCase()
      .replace(/[^a-z0-9\s]/g, '')
      .trim()
      .replace(/\s+/g, ' ');
  }
  const [searchMode, setSearchMode] = createSignal<SearchMode>('songs')
  const [libraryAction, setLibraryAction] = createSignal<LibraryAction>('queue')
  const [showIntake, setShowIntake] = createSignal(false)
  const [songFilterDid] = createSignal('')
  const [editingSongId, setEditingSongId] = createSignal<string | null>(null)
  const [editTitle, setEditTitle] = createSignal('')
  const [editArtist, setEditArtist] = createSignal('')
  const [editAlbum, setEditAlbum] = createSignal('')
  const [editGenre, setEditGenre] = createSignal('')
  const [coverVersions, setCoverVersions] = createSignal<Record<string, number>>({})
  const [draggingQueueId, setDraggingQueueId] = createSignal<string | null>(null)
  const [clock, setClock] = createSignal(Date.now())
  const [snapshotSyncedAt, setSnapshotSyncedAt] = createSignal(Date.now())
  const inFlightDids = new Set<string>()
  const pageSize = 8

  const [selectedSongIds, setSelectedSongIds] = createSignal<string[]>([])
  const [savingSelection, setSavingSelection] = createSignal(false)
  const [newPlaylistName, setNewPlaylistName] = createSignal('')
  const [savingQueue, setSavingQueue] = createSignal(false)
  const [selectedGenre, setSelectedGenre] = createSignal('')

  const toggleSongSelection = (songId: string, checked: boolean) => {
    setSelectedSongIds((current) => (checked ? [...current, songId] : current.filter((id) => id !== songId)))
  }

  const addSelectedToQueue = async () => {
    const ids = selectedSongIds()
    if (ids.length === 0) return
    try {
      setPageError(null)
      mutate(await enqueueAlbum(ids, selectedRadioTarget()))
      setSelectedSongIds([])
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'multi add faceplanted.')
    }
  }

  const saveQueueAsPlaylist = async () => {
    const name = newPlaylistName().trim()
    if (!name) return
    const songIds = (snapshot()?.queue ?? []).map((item) => item.songId)
    if (songIds.length === 0) return

    try {
      setPageError(null)
      await createPlaylist(name, songIds, selectedRadioTarget())
      setNewPlaylistName('')
      setSavingQueue(false)
      await refetchPlaylists()
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'failed to save queue as set')
    }
  }

  const saveSelectionAsPlaylist = async () => {
    const name = newPlaylistName().trim()
    if (!name) return
    const songIds = selectedSongIds()
    if (songIds.length === 0) return

    try {
      setPageError(null)
      await createPlaylist(name, songIds, selectedRadioTarget())
      setNewPlaylistName('')
      setSavingSelection(false)
      setSelectedSongIds([])
      await refetchPlaylists()
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'failed to save selection as set')
    }
  }


  const genres = createMemo(() => {
    const list = (songs() ?? [])
      .map((song) => song.genre?.trim())
      .filter((genre): genre is string => !!genre)
    return [...new Set(list)].sort()
  })

  const shuffleLibraryByGenre = async (genre: string, replace: boolean) => {
    if (!genre) return
    const matchingSongIds = (songs() ?? [])
      .filter((song) => song.genre?.toLowerCase() === genre.toLowerCase())
      .map((song) => song.id)
    if (matchingSongIds.length === 0) return

    const shuffled = [...matchingSongIds].sort(() => Math.random() - 0.5)
    try {
      if (replace) {
        setPageError(null)
        mutate(await clearQueue(selectedRadioTarget()))
      }
      setPageError(null)
      mutate(await enqueueAlbum(shuffled, selectedRadioTarget()))
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'shuffle/queue genre failed')
    }
  }

  const profileFor = (did: string) => profiles()[did] ?? fallbackProfile(did)

  createEffect(() => {
    const syncSelectedStation = () => {
      setSelectedStationUrl(readSelectedStationUrl())
      setPageError(null)
    }
    window.addEventListener('storage', syncSelectedStation)
    window.addEventListener(TUNE_IN_CHANGED_EVENT, syncSelectedStation)
    onCleanup(() => {
      window.removeEventListener('storage', syncSelectedStation)
      window.removeEventListener(TUNE_IN_CHANGED_EVENT, syncSelectedStation)
    })
  })

  createEffect(() => {
    const interval = window.setInterval(() => setClock(Date.now()), 1000)
    onCleanup(() => window.clearInterval(interval))
  })

  createEffect(() => {
    if (snapshot()) setSnapshotSyncedAt(Date.now())
  })

  createEffect(() => {
    if (!isAdmin()) return
    const socketBase = selectedApiBase()
    let socket: WebSocket | null = null
    let reconnectTimer: number | null = null
    let reconnectAttempt = 0
    let cancelled = false

    const connect = () => {
      if (cancelled) return
      socket = openRadioSocket(socketBase)
      socket.addEventListener('open', () => {
        reconnectAttempt = 0
      })
      socket.addEventListener('message', (message) => {
        const event = JSON.parse(message.data) as RadioEvent
        if (event.type === 'snapshotChanged') mutate(event.snapshot)
      })
      const scheduleReconnect = () => {
        if (cancelled || reconnectTimer !== null) return
        void refetch()
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
    const dids = [
      ...(songs() ?? []).map((song) => song.addedByDid),
      ...(snapshot()?.queue ?? []).flatMap((item) => [item.addedByDid, item.queuedByDid]),
      ...(snapshot()?.currentSong?.addedByDid ? [snapshot()!.currentSong!.addedByDid] : []),
    ].filter((did, index, values) => values.indexOf(did) === index && !profiles()[did] && !inFlightDids.has(did))

    for (const did of dids) {
      inFlightDids.add(did)
      void resolveAtprotoProfile(did)
        .then((profile) => setProfiles((current) => ({ ...current, [did]: profile })))
        .finally(() => inFlightDids.delete(did))
    }
  })

  const filteredSongs = createMemo(() => {
    const title = songFilterTitle().trim().toLowerCase()
    const artist = songFilterArtist().trim().toLowerCase()
    const genre = songFilterGenre().trim().toLowerCase()
    const did = songFilterDid().trim().toLowerCase()
    return (songs() ?? []).filter((song) => {
      if (title && !song.title.toLowerCase().includes(title)) return false
      if (artist && !song.artist.toLowerCase().includes(artist)) return false
      if (genre && !song.genre?.toLowerCase().includes(genre)) return false
      if (did) {
        const profile = profileFor(song.addedByDid)
        if (!song.addedByDid.toLowerCase().includes(did) && !profile.handle.toLowerCase().includes(did)) return false
      }
      return true
    })
  })

  const filteredAlbums = createMemo(() => {
    const query = albumFilter().trim().toLowerCase()
    if (!query) return albums() ?? []
    return (albums() ?? []).filter((album) => {
      const trackText = album.tracks.map((track) => `${track.title} ${track.artist}`).join(' ').toLowerCase()
      return album.title.toLowerCase().includes(query) || trackText.includes(query)
    })
  })

  const queuePaging = createPagedList(() => snapshot()?.queue ?? [], pageSize)
  const songsPaging = createPagedList(filteredSongs, pageSize)
  const albumsPaging = createPagedList(filteredAlbums, pageSize)
  const playlistsPaging = createPagedList(() => playlists() ?? [], pageSize)

  createEffect(() => {
    void songFilterTitle()
    void songFilterArtist()
    void songFilterGenre()
    void songFilterDid()
    songsPaging.setPage(0)
  })

  createEffect(() => {
    void albumFilter()
    albumsPaging.setPage(0)
  })

  const livePositionSeconds = () => {
    const now = clock()
    const state = snapshot()?.state
    if (!state || state.status !== 'playing' || !state.startedAt) return state?.positionSeconds ?? 0
    return Math.max(0, state.positionSeconds + Math.floor((now - snapshotSyncedAt()) / 1000))
  }

  const liveProgressPercent = () => {
    const duration = snapshot()?.currentSong?.durationSeconds
    if (!duration || duration <= 0) return 0
    return Math.min(100, Math.max(0, (livePositionSeconds() / duration) * 100))
  }

  const sendControl = async (action: 'play' | 'pause' | 'stop' | 'skip' | 'shuffle') => {
    try {
      setPageError(null)
      const next = await controlRadio(action, 'explicit_admin_action', selectedRadioTarget())
      // The XRPC control response omits the shuffle flag (it isn't part of the
      // lexicon snapshot). Rely on the websocket snapshot broadcast for shuffle
      // state, and preserve the last-known shuffle value across other controls
      // so the toggle doesn't flicker off.
      if (action !== 'shuffle') {
        mutate((prev) => ({ ...next, state: { ...next.state, shuffle: prev?.state?.shuffle ?? next.state.shuffle } }))
      }
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'radio control faceplanted.')
    }
  }
  const shuffleOn = () => Boolean(snapshot()?.state.shuffle)

  const addSongToQueue = async (songId: string) => {
    try {
      setPageError(null)
      mutate(await enqueueSong(songId, selectedRadioTarget()))
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'queue add faceplanted.')
    }
  }

  const addAlbumToQueue = async (songIds: string[]) => {
    try {
      setPageError(null)
      mutate(await enqueueAlbum(songIds, selectedRadioTarget()))
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'album queue add faceplanted.')
    }
  }

  const moveQueueItemUp = async (queueId: string) => {
    const q = snapshot()?.queue || []
    const idx = q.findIndex(item => item.id === queueId)
    if (idx <= 0) return

    let newOrder = q.map(item => item.id)
    const temp = newOrder[idx]
    newOrder[idx] = newOrder[idx - 1]
    newOrder[idx - 1] = temp

    try {
      setPageError(null)
      mutate(await reorderQueue(newOrder, selectedRadioTarget()))
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'failed to move item up.')
    }
  }

  const moveQueueItemDown = async (queueId: string) => {
    const q = snapshot()?.queue || []
    const idx = q.findIndex(item => item.id === queueId)
    if (idx === -1 || idx >= q.length - 1) return

    let newOrder = q.map(item => item.id)
    const temp = newOrder[idx]
    newOrder[idx] = newOrder[idx + 1]
    newOrder[idx + 1] = temp

    try {
      setPageError(null)
      mutate(await reorderQueue(newOrder, selectedRadioTarget()))
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'failed to move item down.')
    }
  }

  const handleSetAlbumEnabled = async (albumId: string, enabled: boolean) => {
    try {
      setPageError(null)
      await setAlbumEnabled(albumId, enabled, selectedRadioTarget())
      void refetchAlbums()
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'failed to toggle album looping.')
    }
  }

  const handleDeleteAlbum = async (albumId: string) => {
    if (!confirm('Are you sure you want to delete this album loop? The associated songs will not be deleted, but they will no longer be grouped as an album loop.')) {
      return
    }
    try {
      setPageError(null)
      await deleteAlbum(albumId, selectedRadioTarget())
      void refetchAlbums()
      void refetchSongs()
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'failed to delete album loop.')
    }
  }

  const handleMergeAlbums = async (sourceId: string, targetId: string) => {
    try {
      setPageError(null)
      await mergeAlbums(sourceId, targetId, selectedRadioTarget())
      void refetchAlbums()
      void refetchSongs()
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'failed to merge albums.')
    }
  }


  const estimatedEndTime = createMemo(() => {
    let totalSeconds = 0

    // 1. Current song remaining duration
    const current = snapshot()?.currentSong
    if (current && current.durationSeconds) {
      const remaining = Math.max(0, current.durationSeconds - livePositionSeconds())
      totalSeconds += remaining
    }

    // 2. Queue total duration
    const q = snapshot()?.queue || []
    for (const item of q) {
      totalSeconds += item.durationSeconds || 0
    }

    if (totalSeconds === 0) {
      return '--:--'
    }

    const endTime = new Date(Date.now() + totalSeconds * 1000)
    let hours = endTime.getHours()
    const minutes = endTime.getMinutes()
    const ampm = hours >= 12 ? 'PM' : 'AM'
    hours = hours % 12
    hours = hours ? hours : 12 // the hour '0' should be '12'
    const minutesStr = minutes < 10 ? '0' + minutes : minutes

    return `${hours}:${minutesStr} ${ampm}`
  })

  const queueDurationMin = createMemo(() => {
    const q = snapshot()?.queue || []
    const totalSeconds = q.reduce((acc, item) => acc + (item.durationSeconds || 0), 0)
    return Math.round(totalSeconds / 60)
  })


  const removePlaylist = async (id: string) => {
    try {
      setPageError(null)
      await deletePlaylist(id, selectedRadioTarget())
      await refetchPlaylists()
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'failed to delete set')
    }
  }

  const loadPlaylistToQueue = async (id: string, replace: boolean) => {
    try {
      setPageError(null)
      mutate(await loadPlaylist(id, replace, selectedRadioTarget()))
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'failed to load set')
    }
  }

  const removeFromQueue = async (queueId: string) => {
    try {
      setPageError(null)
      mutate(await removeQueueItem(queueId, selectedRadioTarget()))
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'queue remove faceplanted.')
    }
  }

  const clearTheQueue = async () => {
    try {
      setPageError(null)
      mutate(await clearQueue(selectedRadioTarget()))
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'clear queue faceplanted.')
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
      mutate(await reorderQueue(reordered, selectedRadioTarget()))
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'reorder faceplanted.')
    }
  }

  const refreshLibrary = async () => {
    await Promise.all([refetchSongs(), refetchAlbums()])
  }

  const coverUrl = (song: Song) => `${songCoverThumbnailUrl(song.id, selectedApiBase())}?v=${coverVersions()[song.id] ?? song.createdAt}`

  const beginSongEdit = (song: Song) => {
    setEditingSongId(song.id)
    setEditTitle(song.title)
    setEditArtist(song.artist)
    setEditAlbum(song.album ?? '')
    setEditGenre(song.genre ?? '')
  }

  const cancelSongEdit = () => {
    setEditingSongId(null)
    setEditTitle('')
    setEditArtist('')
    setEditAlbum('')
    setEditGenre('')
  }

  const saveSongEdit = async (songId: string) => {
    const currentSong = (songs() ?? []).find((song) => song.id === songId)

    try {
      setPageError(null)
      await updateSongMetadata(songId, {
        title: editTitle(),
        artist: editArtist(),
        album: editAlbum() || null,
        genre: editGenre() || null,
        durationSeconds: currentSong?.durationSeconds ?? null,
      }, selectedRadioTarget())
      cancelSongEdit()
      await refreshLibrary()
      void refetch()
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'song metadata update faceplanted.')
    }
  }

  const replaceCover = async (songId: string, file: File | null) => {
    if (!file) return
    try {
      setPageError(null)
      await uploadSongCover(songId, file, selectedRadioTarget())
      setCoverVersions((current) => ({ ...current, [songId]: Date.now() }))
      await refetchSongs()
      void refetch()
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'cover upload faceplanted.')
    }
  }

  const removeSong = async (songId: string) => {
    try {
      setPageError(null)
      await deleteSong(songId, selectedRadioTarget())
      await refreshLibrary()
      void refetch()
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'song delete faceplanted.')
    }
  }

  const queueControlGate = () => {
    const station = labelFromStationUrl(selectedStation().url)
    if (props.sessionLoading) {
      return { kind: 'checking', station, title: 'checking access', message: 'reading your current session.' } as const
    }
    if (!props.session?.authenticated) {
      return { kind: 'signed-out', station, title: 'sign in to continue', message: `queue control for ${station} requires an authenticated account.` } as const
    }
    if (!selectedRadioCanUseXrpc()) {
      return { kind: 'read-only', station, title: 'this station is read only', message: `${station} does not advertise a public radio XRPC endpoint.` } as const
    }
    if (adminStatus.loading) {
      return { kind: 'checking', station, title: 'checking station access', message: `asking ${station} for your permissions.` } as const
    }
    return {
      kind: 'not-admin',
      station,
      title: 'no admin access',
      message: adminStatus()?.message ?? 'you are not an admin of the radio you are currently tuned into.',
    } as const
  }

  const renderQueueItem = (item: QueueItem, index: () => number) => {
    const profile = () => profileFor(item.queuedByDid)
    return (
      <li
        draggable={true}
        classList={{ 'queue-drag-source': draggingQueueId() === item.id }}
        onDragStart={(event) => {
          setDraggingQueueId(item.id)
          event.dataTransfer?.setData('text/plain', item.id)
        }}
        onDragOver={(event) => event.preventDefault()}
        onDrop={(event) => {
          event.preventDefault()
          void handleQueueDrop(item.id)
        }}
        onDragEnd={() => setDraggingQueueId(null)}
        class="qc-queue-item"
      >
        <div class="qc-queue-grip">
          <GripVertical size={16} />
        </div>
        <div class="qc-queue-index">{queuePaging.page() * pageSize + index() + 1}</div>
        <div class="qc-queue-copy">
          <span class="qc-queue-title">
            {item.title}
            <Show when={item.isShuffle}>
              <span
                title="auto-filled by shuffle"
                style="display:inline-flex;align-items:center;gap:2px;margin-left:6px;font-size:0.7em;color:#34d399;opacity:0.9;vertical-align:middle"
              >
                <Shuffle size={11} /> shuffle
              </span>
            </Show>
          </span>
          <span class="qc-queue-artist">{item.artist}{!item.isShuffle && profile().handle ? ` · @${profile().handle}` : ''}</span>
        </div>
        <div class="qc-queue-actions">
          <button class="qc-arrow-btn" type="button" aria-label="move up" onClick={() => void moveQueueItemUp(item.id)}>
            <ChevronUp size={16} />
          </button>
          <button class="qc-arrow-btn" type="button" aria-label="move down" onClick={() => void moveQueueItemDown(item.id)}>
            <ChevronDown size={16} />
          </button>
          <button class="qc-delete-btn" type="button" aria-label="remove from queue" onClick={() => void removeFromQueue(item.id)}>
            <X size={14} />
          </button>
        </div>
      </li>
    )
  }

  return (
    <section class="queue-control-page">
      <Show
        when={isAdmin()}
        fallback={(
          <section
            class="queue-control-gate"
            classList={{ 'is-checking': queueControlGate().kind === 'checking' }}
            role="status"
            aria-live="polite"
          >
            <span class="queue-control-gate-icon" aria-hidden="true">
              <Show when={queueControlGate().kind === 'checking'}>
                <LoaderCircle size={20} strokeWidth={1.8} />
              </Show>
              <Show when={queueControlGate().kind === 'signed-out'}>
                <CircleUserRound size={20} strokeWidth={1.8} />
              </Show>
              <Show when={queueControlGate().kind === 'read-only'}>
                <RadioTower size={20} strokeWidth={1.8} />
              </Show>
              <Show when={queueControlGate().kind === 'not-admin'}>
                <LockKeyhole size={20} strokeWidth={1.8} />
              </Show>
            </span>
            <div class="queue-control-gate-copy">
              <p class="eyebrow">{queueControlGate().station}</p>
              <h1>{queueControlGate().title}</h1>
              <p>{queueControlGate().message}</p>
              <Show when={queueControlGate().kind === 'signed-out'}>
                <a class="queue-control-gate-action" href="/auth">sign in</a>
              </Show>
            </div>
          </section>
        )}
      >
        <Show when={pageError()}>{(message) => <p class="error-copy queue-control-error">{message()}</p>}</Show>

        <header class="qc-station-bar-new">
          <div class="qc-station-identity-new">
            <strong>{selectedStation().name}</strong>
            <span title={selectedStation().url}>{labelFromStationUrl(selectedStation().url)}</span>
          </div>
          <span
            class="qc-station-state-new"
            classList={{ 'is-playing': snapshot()?.state.status === 'playing' }}
          >
            {snapshot()?.state.status ?? 'connecting'}
          </span>
        </header>

        <div class="qc-split-new">
          {/* Top Banner: Now Playing */}
          <section class="qc-now-new">
            <div class="qc-art-new">
              <Show
                when={snapshot()?.currentSong?.hasCover}
                fallback={<div class="qc-art-glow-new" aria-hidden="true">OPEN ROAD</div>}
              >
                <img class="qc-art-cover-new" src={songCoverUrl(snapshot()?.currentSong?.id ?? '', selectedApiBase())} alt="" />
              </Show>
            </div>

            <div class="qc-now-details">
              <div class="qc-now-eyebrow">
                <span>NOW PLAYING</span>
              </div>
              <Show
                when={snapshot()?.currentSong}
                fallback={
                  <>
                    <h1 class="qc-now-title qc-now-title-empty">Nothing playing yet</h1>
                    <p class="qc-now-meta">Silence • Dark Room</p>
                  </>
                }
              >
                {(song) => (
                  <>
                    <h1 class="qc-now-title" title={song().title}>{song().title}</h1>
                    <p class="qc-now-meta">{song().artist} • {song().album || 'Single'}</p>
                    
                    <div class="qc-now-progress-container">
                      <div
                        class="qc-now-progress-track"
                        role="progressbar"
                        aria-label="song progress"
                        aria-valuemin="0"
                        aria-valuemax="100"
                        aria-valuenow={Math.round(liveProgressPercent())}
                      >
                        <span style={`width: ${liveProgressPercent()}%`} />
                      </div>
                      <div class="qc-now-time-row">
                        <span>{formatTime(Math.min(livePositionSeconds(), song().durationSeconds ?? Infinity))}</span>
                        <span>{formatTime(song().durationSeconds)}</span>
                      </div>
                    </div>
                  </>
                )}
              </Show>
            </div>

            <div class="qc-now-controls">
              <div class="qc-transport-panel-new" aria-label="radio transport controls">
                <Show
                  when={snapshot()?.state.status === 'playing'}
                  fallback={
                    <button class="qc-control-btn play-circle-btn" type="button" aria-label="play" title="play" onClick={() => void sendControl('play')}>
                      <Play size={20} fill="black" stroke="black" />
                    </button>
                  }
                >
                  <button class="qc-control-btn play-circle-btn" type="button" aria-label="pause" title="pause" onClick={() => void sendControl('pause')}>
                    <Pause size={20} fill="black" stroke="black" />
                  </button>
                </Show>
                <button class="qc-control-btn" type="button" aria-label="skip" title="skip" onClick={() => void sendControl('skip')}>
                  <SkipForward size={20} />
                </button>
                <button
                  class="qc-control-btn"
                  type="button"
                  aria-label="shuffle all songs"
                  aria-pressed={shuffleOn()}
                  title={shuffleOn() ? 'shuffle: on (playing random songs)' : 'shuffle: off'}
                  onClick={() => void sendControl('shuffle')}
                >
                  <Shuffle size={20} />
                </button>
              </div>

              <button class="qc-end-broadcast-btn" type="button" onClick={() => void sendControl('stop')}>
                End broadcast
              </button>
            </div>
          </section>

          <div class="qc-columns-container">
            {/* Left Column: Music Library */}
            <div class="qc-column-left">
              <div class="qc-column-header">
                <div class="qc-column-title-group">
                  <p class="qc-column-eyebrow">Music library</p>
                  <h2>Find the next track</h2>
                </div>
                <span class="qc-column-stats">
                  {filteredSongs().length} songs • {albums()?.length || 0} albums
                </span>
              </div>

              <div class="qc-search-row">
                <div class="qc-search-input-wrapper">
                  <Search class="qc-search-icon" size={18} />
                  <input
                    type="text"
                    placeholder="Search title, artist, or album"
                    value={songFilterTitle()}
                    onInput={(event) => {
                      setSongFilterTitle(event.currentTarget.value)
                      setSongFilterArtist(event.currentTarget.value)
                      setSongFilterGenre(event.currentTarget.value)
                    }}
                  />
                </div>
              </div>

              <div class="qc-tabs-row">
                <button
                  class="qc-tab-btn"
                  classList={{ active: searchMode() === 'songs' }}
                  onClick={() => setSearchMode('songs')}
                >
                  All
                </button>
                <button
                  class="qc-tab-btn"
                  classList={{ active: searchMode() === 'albums' }}
                  onClick={() => setSearchMode('albums')}
                >
                  Albums
                </button>
                <button
                  class="qc-tab-btn"
                  classList={{ active: searchMode() === 'playlists' }}
                  onClick={() => setSearchMode('playlists')}
                >
                  Sets
                </button>
                <button
                  class="qc-tab-btn edit-tab-btn"
                  classList={{ active: libraryAction() === 'edit' }}
                  onClick={() => {
                    const next: LibraryAction = libraryAction() === 'edit' ? 'queue' : 'edit'
                    setLibraryAction(next)
                    if (next === 'edit') setSearchMode('songs')
                  }}
                >
                  Edit
                </button>

                <button
                  class="qc-tab-btn add-tab-btn"
                  classList={{ active: showIntake() }}
                  onClick={() => setShowIntake(!showIntake())}
                >
                  {showIntake() ? 'Close' : 'Add Music'}
                </button>
              </div>

              <Show when={showIntake()}>
                <div class="qc-intake-wrapper">
                  <AdminUploadPanel
                    target={selectedRadioTarget()}
                    onSongAdded={() => void refreshLibrary()}
                    onError={setPageError}
                  />
                </div>
              </Show>

              {/* Songs List */}
              <Show when={searchMode() === 'songs'}>
                <Show when={libraryAction() === 'queue' && selectedSongIds().length > 0}>
                  <div class="multi-add-row queue-selection-row" style="display: flex; flex-direction: column; gap: 0.75rem; padding: 0.75rem; border: 1px solid var(--hairline); border-radius: 8px; margin-bottom: 1rem; background: color-mix(in srgb, var(--hairline) 20%, transparent);">
                    <div style="display: flex; align-items: center; justify-content: space-between; gap: 1rem; flex-wrap: wrap;">
                      <span style="font-weight: 500; font-size: 0.95rem; color: var(--text);">{selectedSongIds().length} songs selected</span>
                      <div style="display: flex; align-items: center; gap: 0.5rem;">
                        <button class="pill-button" type="button" onClick={() => void addSelectedToQueue()}>
                          add to queue
                        </button>
                        <button class="pill-button subtle" type="button" onClick={() => { setSavingSelection(!savingSelection()); setNewPlaylistName(''); }}>
                          {savingSelection() ? 'cancel' : 'save as set'}
                        </button>
                        <button class="pill-button subtle" type="button" onClick={() => setSelectedSongIds([])}>
                          clear
                        </button>
                      </div>
                    </div>
                    <Show when={savingSelection()}>
                      <div class="playlist-save-form">
                        <input
                          type="text"
                          placeholder="name your set"
                          style="flex: 1; padding: 0.35rem 0.65rem; border-radius: 6px; border: 1px solid var(--hairline); background: transparent; color: var(--text); font-size: 0.9rem;"
                          value={newPlaylistName()}
                          onInput={(event) => setNewPlaylistName(event.currentTarget.value)}
                          onKeyDown={(event) => {
                            if (event.key === 'Enter') void saveSelectionAsPlaylist()
                          }}
                        />
                        <button class="pill-button" type="button" disabled={!newPlaylistName().trim()} onClick={() => void saveSelectionAsPlaylist()}>
                          save
                        </button>
                      </div>
                    </Show>
                  </div>
                </Show>

                <Show when={libraryAction() === 'queue'}>
                  <div class="qc-genre-bar">
                    <span style="font-weight: 500; font-size: 0.9rem; color: var(--text); flex-shrink: 0;">queue genre:</span>
                    <div style="flex: 1; min-width: 140px; position: relative;">
                      <SearchableDropdown
                        options={genres()}
                        placeholder="select genre..."
                        onSelect={(val) => setSelectedGenre(val)}
                      />
                    </div>
                    <Show when={selectedGenre()}>
                      <div style="display: flex; align-items: center; gap: 0.5rem; flex-shrink: 0;">
                        <button class="pill-button subtle" type="button" onClick={() => void shuffleLibraryByGenre(selectedGenre(), false)}>
                          append
                        </button>
                        <button class="pill-button subtle" type="button" onClick={() => void shuffleLibraryByGenre(selectedGenre(), true)}>
                          play (shuffle)
                        </button>
                        <button class="pill-button subtle" type="button" style="padding-inline: 0.4rem;" onClick={() => setSelectedGenre('')}>
                          <X size={15} />
                        </button>
                      </div>
                    </Show>
                  </div>
                </Show>

                <Show when={!songs.loading} fallback={<p class="list-empty">loading songs...</p>}>
                  <ul class="qc-songs-list" classList={{ 'library-edit-list': libraryAction() === 'edit' }}>
                    <For each={songsPaging.paged()} fallback={<li class="list-empty">no songs match</li>}>
                      {(song) => {
                        const isQueued = () => snapshot()?.queue.some(item => item.songId === song.id)
                        return (
                          <li class="qc-song-item" classList={{ editing: editingSongId() === song.id }}>
                            <Show
                              when={libraryAction() === 'edit'}
                              fallback={
                                <>
                                  <input
                                    type="checkbox"
                                    style="margin: 0 0.5rem 0 0; cursor: pointer; flex-shrink: 0; width: 16px; height: 16px;"
                                    checked={selectedSongIds().includes(song.id)}
                                    onChange={(event) => toggleSongSelection(song.id, event.currentTarget.checked)}
                                  />
                                  <div class="qc-song-thumb">
                                    <Show when={song.hasCover} fallback={<div class="qc-thumb-placeholder">{song.title.slice(0, 4).toUpperCase()}</div>}>
                                      <img src={songCoverThumbnailUrl(song.id, selectedApiBase())} alt="" />
                                    </Show>
                                  </div>
                                  <div class="qc-song-info">
                                    <span class="qc-song-title">{song.title}</span>
                                    <span class="qc-song-meta-line">{song.artist} • {song.album || 'Single'}</span>
                                  </div>
                                  <div class="qc-song-genre-pill">{song.genre || 'General'}</div>
                                  <div class="qc-song-duration">{formatTime(song.durationSeconds)}</div>
                                  
                                  <button class="qc-add-btn" classList={{ 'already-queued': isQueued() }} onClick={() => void addSongToQueue(song.id)}>
                                    + Add
                                  </button>
                                  <button class="qc-more-btn" onClick={() => beginSongEdit(song)}>...</button>
                                </>
                              }
                            >
                              <Show when={song.hasCover} fallback={<span class="cover-thumb" />}>
                                <img class="cover-thumb" src={coverUrl(song)} alt="" loading="lazy" />
                              </Show>
                              <Show
                                when={editingSongId() === song.id}
                                fallback={
                                  <>
                                    <div class="song-copy">
                                      <span>{song.title}</span>
                                      <small>{song.artist}{song.album ? ` · ${song.album}` : ''}{song.genre ? ` · ${song.genre}` : ''}</small>
                                    </div>
                                    <button class="pill-button subtle" type="button" onClick={() => beginSongEdit(song)}>
                                      edit
                                    </button>
                                  </>
                                }
                              >
                                <form class="song-edit-form" onSubmit={(event) => { event.preventDefault(); void saveSongEdit(song.id) }}>
                                  <input aria-label="song title" placeholder="title" value={editTitle()} onInput={(event) => setEditTitle(event.currentTarget.value)} />
                                  <input aria-label="song artist" placeholder="artist" value={editArtist()} onInput={(event) => setEditArtist(event.currentTarget.value)} />
                                  <input aria-label="song album" placeholder="album" value={editAlbum()} onInput={(event) => setEditAlbum(event.currentTarget.value)} />
                                  <input aria-label="song genre" placeholder="genre" value={editGenre()} onInput={(event) => setEditGenre(event.currentTarget.value)} />
                                  <div class="song-edit-actions">
                                    <label class="pill-button subtle cover-upload inline-cover-upload">
                                      <UploadCloud size={16} />
                                      cover
                                      <input type="file" accept="image/*" onChange={(event) => void replaceCover(song.id, event.currentTarget.files?.[0] ?? null)} />
                                    </label>
                                    <button class="pill-button" type="submit">save</button>
                                    <button class="pill-button subtle" type="button" onClick={cancelSongEdit}>cancel</button>
                                    <button class="pill-button subtle danger-button" type="button" onClick={() => void removeSong(song.id)}>delete</button>
                                  </div>
                                </form>
                              </Show>
                            </Show>
                          </li>
                        )
                      }}
                    </For>
                  </ul>
                  <Show when={songsPaging.pageCount() > 1}>
                    <PaginationRow page={songsPaging.page()} pageCount={songsPaging.pageCount()} onPageChange={songsPaging.setPage} />
                  </Show>
                </Show>
              </Show>

              {/* Albums List */}
              <Show when={searchMode() === 'albums'}>
                <div class="song-filters album-search-row">
                  <input placeholder="album, track, or artist" value={albumFilter()} onInput={(event) => setAlbumFilter(event.currentTarget.value)} />
                </div>
                <Show when={!albums.loading} fallback={<p class="list-empty">loading albums...</p>}>
                  <ul class="song-list album-loop-list">
                    <For each={albumsPaging.paged()} fallback={<li class="list-empty">no albums match</li>}>
                      {(album) => {
                        const isExpanded = () => expandedAlbumId() === album.id
                        const duplicate = () => (albums() ?? []).find(a => a.id !== album.id && normalizeTitleForUi(a.title) === normalizeTitleForUi(album.title))

                        return (
                          <li class="album-item-container" style="display: flex; flex-direction: column; align-items: stretch; gap: 0.5rem; padding: 0.75rem; border-bottom: 1px solid var(--hairline);">
                            <div class="album-row-header" style="display: flex; align-items: center; justify-content: space-between; gap: 1rem; width: 100%;">
                              <div
                                class="song-copy"
                                style="cursor: pointer; flex-grow: 1; min-width: 0;"
                                onClick={() => setExpandedAlbumId(isExpanded() ? null : album.id)}
                              >
                                <span style="font-weight: bold; display: flex; align-items: center; gap: 0.5rem; color: var(--text);">
                                  {album.title}
                                  <Show when={album.isEnabled}>
                                    <span style="font-size: 0.7rem; padding: 0.1rem 0.3rem; border-radius: 4px; background: rgba(0,200,0,0.15); color: #00cc00; font-weight: normal;">looping</span>
                                  </Show>
                                  <Show when={duplicate()}>
                                    <span style="font-size: 0.7rem; padding: 0.1rem 0.3rem; border-radius: 4px; background: rgba(255,165,0,0.15); color: #ffa500; font-weight: normal;">duplicate</span>
                                  </Show>
                                </span>
                                <small style="display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;">
                                  {album.tracks.length} tracks · {album.tracks.map((track) => track.title).join(' → ')}
                                </small>
                              </div>
                              <div style="display: flex; align-items: center; gap: 0.5rem; flex-shrink: 0;">
                                <button
                                  class="icon-button"
                                  type="button"
                                  aria-label="queue album"
                                  disabled={album.tracks.length === 0}
                                  onClick={(e) => {
                                    e.stopPropagation()
                                    void addAlbumToQueue(album.tracks.map((track) => track.id))
                                  }}
                                >
                                  <ListPlus size={18} />
                                </button>
                              </div>
                            </div>

                            <Show when={isExpanded()}>
                              <div class="album-details" style="display: flex; flex-direction: column; gap: 0.75rem; border-top: 1px solid var(--hairline); padding-top: 0.75rem; margin-top: 0.25rem;">
                                <div class="album-actions-row" style="display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 0.75rem;">
                                  <div style="display: flex; align-items: center; gap: 0.75rem;">
                                    <label class="inline-check" style="margin: 0; display: flex; align-items: center; gap: 0.35rem; font-size: 0.9rem; cursor: pointer; color: var(--text);">
                                      <input
                                        type="checkbox"
                                        checked={album.isEnabled}
                                        onChange={(e) => void handleSetAlbumEnabled(album.id, e.currentTarget.checked)}
                                      />
                                      loop this album
                                    </label>
                                    <button
                                      class="pill-button subtle"
                                      style="color: var(--error); border-color: rgba(255, 0, 0, 0.2); padding: 0.25rem 0.5rem; font-size: 0.85rem;"
                                      type="button"
                                      onClick={() => void handleDeleteAlbum(album.id)}
                                    >
                                      clear album loop
                                    </button>
                                  </div>

                                  <div style="display: flex; align-items: center; gap: 0.5rem;">
                                    <Show when={duplicate()}>
                                      {(dup) => (
                                        <button
                                          class="pill-button"
                                          style="background: #ffa500; color: #000; font-size: 0.85rem; padding: 0.25rem 0.5rem; border: none; font-weight: bold;"
                                          type="button"
                                          onClick={() => {
                                            if (confirm(`Merge this album "${album.title}" into the duplicate "${dup().title}"? All tracks will be combined under "${dup().title}".`)) {
                                              void handleMergeAlbums(album.id, dup().id)
                                            }
                                          }}
                                        >
                                          merge duplicate
                                        </button>
                                      )}
                                    </Show>

                                    <div style="display: flex; align-items: center; gap: 0.25rem;">
                                      <select
                                        style="padding: 0.25rem 0.5rem; font-size: 0.85rem; border: 1px solid var(--line); border-radius: 4px; background: transparent; color: var(--text);"
                                        onChange={(e) => {
                                          const val = e.currentTarget.value
                                          if (val) {
                                            const target = (albums() ?? []).find(a => a.id === val)
                                            if (target && confirm(`Merge this album "${album.title}" into "${target.title}"? All tracks will be combined under "${target.title}".`)) {
                                              void handleMergeAlbums(album.id, val)
                                            }
                                            e.currentTarget.value = ""
                                          }
                                        }}
                                      >
                                        <option value="" style="color: #000;">merge into...</option>
                                        <For each={(albums() ?? []).filter(a => a.id !== album.id)}>
                                          {(other) => <option value={other.id} style="color: #000;">{other.title}</option>}
                                        </For>
                                      </select>
                                    </div>
                                  </div>
                                </div>

                                <div class="album-tracks-list" style="display: flex; flex-direction: column; gap: 0.25rem; background: rgba(0,0,0,0.1); padding: 0.5rem; border-radius: 4px; max-height: 12rem; overflow-y: auto;">
                                  <span style="font-size: 0.8rem; font-weight: bold; opacity: 0.7; margin-bottom: 0.25rem; color: var(--text);">tracks:</span>
                                  <For each={album.tracks}>
                                    {(track, idx) => (
                                      <div style="display: flex; align-items: center; justify-content: space-between; font-size: 0.85rem; padding: 0.15rem 0; color: var(--text);">
                                        <span style="text-overflow: ellipsis; overflow: hidden; white-space: nowrap; max-width: 75%;">
                                          <span style="opacity: 0.5; margin-right: 0.35rem;">{idx() + 1}.</span>
                                          {track.title}
                                        </span>
                                        <span style="opacity: 0.6; text-overflow: ellipsis; overflow: hidden; white-space: nowrap; max-width: 25%; font-size: 0.8rem;">
                                          {track.artist}
                                        </span>
                                      </div>
                                    )}
                                  </For>
                                </div>
                              </div>
                            </Show>
                          </li>
                        )
                      }}
                    </For>
                  </ul>
                  <Show when={albumsPaging.pageCount() > 1}>
                    <PaginationRow page={albumsPaging.page()} pageCount={albumsPaging.pageCount()} onPageChange={albumsPaging.setPage} />
                  </Show>
                </Show>
              </Show>

              {/* Sets/Playlists List */}
              <Show when={searchMode() === 'playlists'}>
                <Show when={!playlists.loading} fallback={<p class="list-empty">loading sets...</p>}>
                  <ul class="playlist-list">
                    <For each={playlistsPaging.paged()} fallback={<li class="list-empty">no sets saved yet</li>}>
                      {(playlist) => (
                        <li class="playlist-card-item">
                          <div class="playlist-item-header">
                            <span class="playlist-item-title">{playlist.name}</span>
                            <div class="playlist-item-actions">
                              <button class="pill-button subtle" type="button" onClick={() => void loadPlaylistToQueue(playlist.id, true)}>
                                play
                              </button>
                              <button class="pill-button subtle" type="button" onClick={() => void loadPlaylistToQueue(playlist.id, false)}>
                                append
                              </button>
                              <button class="pill-button subtle danger-button" type="button" aria-label="delete playlist" onClick={() => void removePlaylist(playlist.id)}>
                                delete
                              </button>
                            </div>
                          </div>
                          <div class="playlist-item-meta">
                            <small class="playlist-track-count">{playlist.tracks.length} tracks</small>
                          </div>
                          <div class="playlist-track-list">
                            <For each={playlist.tracks}>
                              {(track, idx) => (
                                <div class="playlist-track-item">
                                  <span class="track-num">{idx() + 1}</span>
                                  <span class="track-title" title={track.title}>{track.title}</span>
                                  <span class="track-artist" title={track.artist}>{track.artist}</span>
                                </div>
                              )}
                            </For>
                          </div>
                        </li>
                      )}
                    </For>
                  </ul>
                  <Show when={playlistsPaging.pageCount() > 1}>
                    <PaginationRow page={playlistsPaging.page()} pageCount={playlistsPaging.pageCount()} onPageChange={playlistsPaging.setPage} />
                  </Show>
                </Show>
              </Show>
            </div>

            {/* Right Column: Broadcast Order */}
            <div class="qc-column-right">
              <div class="qc-column-header">
                <div class="qc-column-title-group">
                  <p class="qc-column-eyebrow">Broadcast order</p>
                  <h2>Up next</h2>
                </div>
                <Show when={(snapshot()?.queue.length ?? 0) > 0}>
                  <div style="display: flex; align-items: center; gap: 0.5rem; flex-shrink: 0;">
                    <button class="qc-clear-btn" onClick={() => { setSavingQueue(!savingQueue()); setNewPlaylistName(''); }}>
                      Save Set
                    </button>
                    <button class="qc-clear-btn" onClick={() => void clearTheQueue()}>
                      <Trash2 size={14} style="margin-right: 4px;" />
                      Clear
                    </button>
                  </div>
                </Show>
              </div>

              <Show when={savingQueue()}>
                <div class="playlist-save-form">
                  <input
                    type="text"
                    placeholder="name your set"
                    style="flex: 1; padding: 0.35rem 0.65rem; border-radius: 6px; border: 1px solid var(--hairline); background: transparent; color: var(--text); font-size: 0.9rem;"
                    value={newPlaylistName()}
                    onInput={(event) => setNewPlaylistName(event.currentTarget.value)}
                    onKeyDown={(event) => {
                      if (event.key === 'Enter') void saveQueueAsPlaylist()
                    }}
                  />
                  <button class="pill-button" type="button" disabled={!newPlaylistName().trim()} onClick={() => void saveQueueAsPlaylist()}>
                    save
                  </button>
                </div>
              </Show>


              <span class="qc-column-stats" style="margin-top: 4px; display: block;">
                {snapshot()?.queue.length ?? 0} tracks • about {queueDurationMin()} min
              </span>

              <Show when={!snapshot.loading} fallback={<p class="list-empty">loading queue...</p>}>
                <ul class="qc-queue-list">
                  <For each={queuePaging.paged()} fallback={<li class="list-empty">queue is empty</li>}>
                    {renderQueueItem}
                  </For>
                </ul>
                <Show when={queuePaging.pageCount() > 1}>
                  <PaginationRow page={queuePaging.page()} pageCount={queuePaging.pageCount()} onPageChange={queuePaging.setPage} compact />
                </Show>
              </Show>

              {/* Estimated End Time */}
              <div class="qc-est-end-row">
                <span class="qc-est-label">
                  <Clock size={16} style="margin-right: 6px;" />
                  Estimated end
                </span>
                <span class="qc-est-time">{estimatedEndTime()}</span>
              </div>
            </div>
          </div>

          {/* Chat Panel at the bottom */}
          <ChatModerationPanel apiBase={selectedApiBase()} stationKey={selectedStationKey()} target={selectedRadioTarget()} />
        </div>
      </Show>
    </section>
  )
}
