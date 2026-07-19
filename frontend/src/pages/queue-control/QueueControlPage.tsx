import { createEffect, createMemo, createResource, createSignal, For, onCleanup, Show } from 'solid-js'
import {
  CircleUserRound,
  GripVertical,
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
  fetchRotationInfo,
  setAlbumWeight,
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
  const [rotationInfo, { refetch: refetchRotationInfo }] = createResource(adminResourceSource, () => fetchRotationInfo(selectedRadioTarget()))
  const [profiles, setProfiles] = createSignal<Record<string, AtprotoProfile>>({})
  const [pageError, setPageError] = createSignal<string | null>(null)
  const [libraryQuery, setLibraryQuery] = createSignal('')
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
  const [showIntake, setShowIntake] = createSignal(false)
  const [editingSongId, setEditingSongId] = createSignal<string | null>(null)
  const [editTitle, setEditTitle] = createSignal('')
  const [editArtist, setEditArtist] = createSignal('')
  const [editAlbum, setEditAlbum] = createSignal('')
  const [editGenre, setEditGenre] = createSignal('')
  const [coverVersions, setCoverVersions] = createSignal<Record<string, number>>({})
  const [draggingQueueId, setDraggingQueueId] = createSignal<string | null>(null)
  const [dragOverQueueId, setDragOverQueueId] = createSignal<string | null>(null)
  let queueListEl: HTMLUListElement | undefined
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

  const genreCounts = createMemo(() => {
    const counts: Record<string, number> = {}
    for (const genre of genres()) {
      counts[genre] = (songs() ?? []).filter((song) => song.genre?.toLowerCase() === genre.toLowerCase()).length
    }
    return counts
  })

  const selectedGenreCount = () => genreCounts()[selectedGenre()] ?? 0

  const [genreNotice, setGenreNotice] = createSignal<string | null>(null)
  let genreNoticeTimer: number | undefined
  const flashGenreNotice = (message: string) => {
    setGenreNotice(message)
    window.clearTimeout(genreNoticeTimer)
    genreNoticeTimer = window.setTimeout(() => setGenreNotice(null), 4000)
  }
  onCleanup(() => window.clearTimeout(genreNoticeTimer))

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
      flashGenreNotice(`${replace ? 'now playing' : 'queued'} ${shuffled.length} ${genre} song${shuffled.length === 1 ? '' : 's'}, shuffled`)
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
    void snapshot()?.currentSong?.id
    void refetchRotationInfo()
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

  // One query filters whichever tab is open. A song/album/set matches when
  // every word of the query appears in some field — so "beatles help" finds
  // the song, but searching just an artist still lists their whole catalog.
  // Haystacks are lowercased once per data change, not per keystroke.
  const queryWords = createMemo(() => libraryQuery().trim().toLowerCase().split(/\s+/).filter(Boolean))
  const matchesQuery = (haystack: string) => queryWords().every((word) => haystack.includes(word))

  const songIndex = createMemo(() => (songs() ?? []).map((song) => ({
    song,
    haystack: `${song.title} ${song.artist} ${song.album ?? ''} ${song.genre ?? ''}`.toLowerCase(),
  })))
  const albumIndex = createMemo(() => (albums() ?? []).map((album) => ({
    album,
    haystack: `${album.title} ${album.tracks.map((track) => `${track.title} ${track.artist}`).join(' ')}`.toLowerCase(),
  })))
  const playlistIndex = createMemo(() => (playlists() ?? []).map((playlist) => ({
    playlist,
    haystack: `${playlist.name} ${playlist.tracks.map((track) => `${track.title} ${track.artist}`).join(' ')}`.toLowerCase(),
  })))

  const filteredSongs = createMemo(() => songIndex().filter((entry) => matchesQuery(entry.haystack)).map((entry) => entry.song))
  const filteredAlbums = createMemo(() => albumIndex().filter((entry) => matchesQuery(entry.haystack)).map((entry) => entry.album))
  const filteredPlaylists = createMemo(() => playlistIndex().filter((entry) => matchesQuery(entry.haystack)).map((entry) => entry.playlist))

  const queuePaging = createPagedList(() => snapshot()?.queue ?? [], pageSize)
  const songsPaging = createPagedList(filteredSongs, pageSize)
  const albumsPaging = createPagedList(filteredAlbums, pageSize)
  const playlistsPaging = createPagedList(filteredPlaylists, pageSize)

  createEffect(() => {
    void libraryQuery()
    songsPaging.setPage(0)
    albumsPaging.setPage(0)
    playlistsPaging.setPage(0)
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

  const handleSetAlbumEnabled = async (albumId: string, enabled: boolean) => {
    try {
      setPageError(null)
      await setAlbumEnabled(albumId, enabled, selectedRadioTarget())
      void refetchAlbums()
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'failed to update album rotation.')
    }
  }

  const setAllAlbumsEnabled = async (enabled: boolean) => {
    const targets = (albums() ?? []).filter((album) => album.isEnabled !== enabled)
    if (targets.length === 0) return
    if (!enabled && !confirm(`Take all ${targets.length} albums out of rotation? The station will only autoplay from shuffle or loose singles.`)) return
    try {
      setPageError(null)
      for (const album of targets) {
        await setAlbumEnabled(album.id, enabled, selectedRadioTarget())
      }
      void refetchAlbums()
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'failed to update album rotation.')
      void refetchAlbums()
    }
  }

  const handleSetAlbumWeight = async (albumId: string, weight: number) => {
    try {
      setPageError(null)
      await setAlbumWeight(albumId, weight, selectedRadioTarget())
      void refetchRotationInfo()
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'failed to update rotation weight.')
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

  const formatClockTime = (msFromNow: number) => {
    const at = new Date(Date.now() + msFromNow)
    let hours = at.getHours()
    const minutes = at.getMinutes()
    const ampm = hours >= 12 ? 'PM' : 'AM'
    hours = hours % 12 || 12
    return `${hours}:${minutes < 10 ? '0' + minutes : minutes} ${ampm}`
  }

  // Estimated wall-clock start for each queue row: remaining current song
  // plus every earlier queued song. Unknown durations count as zero, so
  // times drift optimistic rather than crashing.
  const queueAirTimes = createMemo(() => {
    void clock()
    const current = snapshot()?.currentSong
    let offset = current?.durationSeconds
      ? Math.max(0, current.durationSeconds - livePositionSeconds())
      : 0
    return (snapshot()?.queue ?? []).map((item) => {
      const at = formatClockTime(offset * 1000)
      offset += item.durationSeconds || 0
      return at
    })
  })

  // What the station will do once the queue drains — the priority chain is
  // queue, then shuffle, then album loops + singles.
  const afterQueueLabel = createMemo(() => {
    if (snapshot()?.state.shuffle) return 'shuffle (weighted rotation)'
    const enabledAlbums = (albums() ?? []).filter((album) => album.isEnabled).length
    const inAlbums = new Set((albums() ?? []).flatMap((album) => album.tracks.map((track) => track.id)))
    const singles = (songs() ?? []).filter((song) => !inAlbums.has(song.id)).length
    if (enabledAlbums === 0 && singles === 0) return null
    const parts = []
    if (enabledAlbums > 0) parts.push(`${enabledAlbums} album${enabledAlbums === 1 ? '' : 's'}`)
    if (singles > 0) parts.push(`${singles} single${singles === 1 ? '' : 's'}`)
    return `album loops (${parts.join(' + ')})`
  })

  // Songs that can never autoplay: every album they belong to is benched.
  const benchedSongIds = createMemo(() => {
    const inAny = new Set<string>()
    const inEnabled = new Set<string>()
    for (const album of albums() ?? []) {
      for (const track of album.tracks) {
        inAny.add(track.id)
        if (album.isEnabled) inEnabled.add(track.id)
      }
    }
    const benched = new Set<string>()
    for (const id of inAny) {
      if (!inEnabled.has(id)) benched.add(id)
    }
    return benched
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

  // Pointer-based reordering: works for touch and mouse alike, unlike HTML5
  // drag-and-drop which never fires on touch screens. The grip is the only
  // initiator so the rest of the row still scrolls the page on a phone.
  // Shared drag session used by both initiators: the grip (immediate, any
  // pointer) and long-press on the row body (touch). Listeners live on the
  // element holding pointer capture; onEnd releases initiator-side hooks.
  const runQueueDragSession = (
    item: QueueItem,
    row: HTMLElement,
    captureEl: HTMLElement,
    originY: number,
    onEnd?: () => void,
  ) => {
    setDraggingQueueId(item.id)
    let overId: string | null = null

    const move = (ev: PointerEvent) => {
      row.style.transform = `translateY(${ev.clientY - originY}px)`
      overId = null
      for (const li of queueListEl?.querySelectorAll<HTMLElement>('li[data-queue-id]') ?? []) {
        if (li === row) continue
        const rect = li.getBoundingClientRect()
        if (ev.clientY >= rect.top && ev.clientY < rect.bottom) {
          overId = li.dataset.queueId ?? null
          break
        }
      }
      setDragOverQueueId(overId)
    }
    const finish = (commit: boolean) => {
      captureEl.removeEventListener('pointermove', move)
      captureEl.removeEventListener('pointerup', up)
      captureEl.removeEventListener('pointercancel', cancel)
      row.style.transform = ''
      setDragOverQueueId(null)
      onEnd?.()
      if (commit && overId) {
        void handleQueueDrop(overId)
      } else {
        setDraggingQueueId(null)
      }
    }
    const up = () => finish(true)
    const cancel = () => finish(false)
    captureEl.addEventListener('pointermove', move)
    captureEl.addEventListener('pointerup', up)
    captureEl.addEventListener('pointercancel', cancel)
  }

  const startQueueDrag = (item: QueueItem, event: PointerEvent) => {
    if (event.pointerType === 'mouse' && event.button !== 0) return
    const grip = event.currentTarget as HTMLElement
    const row = grip.closest('li')
    if (!row || !queueListEl) return
    event.preventDefault()
    grip.setPointerCapture(event.pointerId)
    runQueueDragSession(item, row, grip, event.clientY)
  }

  // Long-press on the row body starts a touch drag: hold ~a third of a second
  // without moving. A scroll gesture (movement or the browser's pointercancel)
  // aborts the timer, so normal scrolling through the queue is unaffected.
  const LONG_PRESS_MS = 350
  const LONG_PRESS_SLOP_PX = 10
  const startQueueRowLongPress = (item: QueueItem, event: PointerEvent) => {
    if (event.pointerType !== 'touch') return
    const row = event.currentTarget as HTMLElement
    if ((event.target as HTMLElement | null)?.closest('button')) return
    const startX = event.clientX
    const startY = event.clientY
    let lastY = startY
    let engaged = false

    // Once the drag engages, swallowing touchmove is the only way to keep the
    // browser from turning further finger movement into a scroll.
    const blockScroll = (ev: TouchEvent) => {
      if (engaged) ev.preventDefault()
    }
    row.addEventListener('touchmove', blockScroll, { passive: false })

    const abort = () => {
      window.clearTimeout(timer)
      row.removeEventListener('pointermove', premove)
      row.removeEventListener('pointerup', abort)
      row.removeEventListener('pointercancel', abort)
      if (!engaged) row.removeEventListener('touchmove', blockScroll)
    }
    const premove = (ev: PointerEvent) => {
      lastY = ev.clientY
      if (Math.hypot(ev.clientX - startX, ev.clientY - startY) > LONG_PRESS_SLOP_PX) abort()
    }
    row.addEventListener('pointermove', premove)
    row.addEventListener('pointerup', abort)
    row.addEventListener('pointercancel', abort)

    const timer = window.setTimeout(() => {
      engaged = true
      abort()
      navigator.vibrate?.(12)
      row.setPointerCapture(event.pointerId)
      runQueueDragSession(item, row, row, lastY, () => {
        row.removeEventListener('touchmove', blockScroll)
      })
    }, LONG_PRESS_MS)
  }

  const renderQueueItem = (item: QueueItem, index: () => number) => {
    const profile = () => profileFor(item.queuedByDid)
    return (
      <li
        data-queue-id={item.id}
        onPointerDown={(event) => startQueueRowLongPress(item, event)}
        classList={{
          'queue-drag-source': draggingQueueId() === item.id,
          'drag-over': dragOverQueueId() === item.id,
        }}
        class="qc-queue-item"
      >
        <div class="qc-queue-grip" onPointerDown={(event) => startQueueDrag(item, event)}>
          <GripVertical size={16} />
        </div>
        <div class="qc-queue-index">{queuePaging.page() * pageSize + index() + 1}</div>
        <div class="qc-queue-thumb">
          <Show
            when={(songs() ?? []).some((song) => song.id === item.songId && song.hasCover)}
            fallback={<div class="qc-thumb-placeholder">{item.title.slice(0, 4).toUpperCase()}</div>}
          >
            <img src={songCoverThumbnailUrl(item.songId, selectedApiBase())} alt="" loading="lazy" />
          </Show>
        </div>
        <div class="qc-queue-copy">
          <span class="qc-queue-title-row">
            <span class="qc-queue-title">{item.title}</span>
            <Show when={item.isShuffle}>
              <span class="qc-queue-shuffle-badge" title="auto-filled by shuffle">
                <Shuffle size={11} /> shuffle
              </span>
            </Show>
          </span>
          <span class="qc-queue-artist">{item.artist}{!item.isShuffle && profile().handle ? ` · @${profile().handle}` : ''}</span>
        </div>
        <span class="qc-queue-airtime" title="estimated air time">
          {queueAirTimes()[queuePaging.page() * pageSize + index()] ?? ''}
        </span>
        <div class="qc-queue-actions">
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
                    placeholder="Search songs, albums, and sets"
                    aria-label="search the library"
                    autocomplete="off"
                    enterkeyhint="search"
                    value={libraryQuery()}
                    onInput={(event) => setLibraryQuery(event.currentTarget.value)}
                    onKeyDown={(event) => {
                      if (event.key === 'Escape') setLibraryQuery('')
                    }}
                  />
                  <Show when={libraryQuery()}>
                    <button
                      class="qc-search-clear"
                      type="button"
                      aria-label="clear search"
                      onClick={() => setLibraryQuery('')}
                    >
                      <X size={16} />
                    </button>
                  </Show>
                </div>
              </div>

              <div class="qc-tabs-row">
                <button
                  class="qc-tab-btn"
                  classList={{ active: searchMode() === 'songs' }}
                  onClick={() => setSearchMode('songs')}
                >
                  All{queryWords().length > 0 ? ` (${filteredSongs().length})` : ''}
                </button>
                <button
                  class="qc-tab-btn"
                  classList={{ active: searchMode() === 'albums' }}
                  onClick={() => setSearchMode('albums')}
                >
                  Albums{queryWords().length > 0 ? ` (${filteredAlbums().length})` : ''}
                </button>
                <button
                  class="qc-tab-btn"
                  classList={{ active: searchMode() === 'playlists' }}
                  onClick={() => setSearchMode('playlists')}
                >
                  Sets{queryWords().length > 0 ? ` (${filteredPlaylists().length})` : ''}
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
                <Show when={selectedSongIds().length > 0}>
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

                <div class="qc-genre-bar">
                  <span class="qc-genre-bar-label">genre station</span>
                  <div class="qc-genre-picker">
                    <SearchableDropdown
                      options={genres()}
                      counts={genreCounts()}
                      placeholder="pick a genre..."
                      value={selectedGenre()}
                      onSelect={(val) => setSelectedGenre(val)}
                    />
                  </div>
                  <Show when={selectedGenre()}>
                    <div class="qc-genre-actions">
                      <button
                        class="pill-button subtle"
                        type="button"
                        disabled={selectedGenreCount() === 0}
                        onClick={() => void shuffleLibraryByGenre(selectedGenre(), false)}
                      >
                        add {selectedGenreCount()} to queue
                      </button>
                      <button
                        class="pill-button subtle"
                        type="button"
                        disabled={selectedGenreCount() === 0}
                        title="clears the current queue first"
                        onClick={() => {
                          const queued = snapshot()?.queue.length ?? 0
                          if (queued === 0 || confirm(`Replace the current queue (${queued} songs) with ${selectedGenreCount()} shuffled ${selectedGenre()} songs?`)) {
                            void shuffleLibraryByGenre(selectedGenre(), true)
                          }
                        }}
                      >
                        replace queue
                      </button>
                      <button class="pill-button subtle qc-genre-clear" type="button" aria-label="clear genre" onClick={() => setSelectedGenre('')}>
                        <X size={15} />
                      </button>
                    </div>
                  </Show>
                  <Show when={genreNotice()}>
                    <span class="qc-genre-notice" role="status">{genreNotice()}</span>
                  </Show>
                </div>

                <Show when={!songs.loading} fallback={<p class="list-empty">loading songs...</p>}>
                  <ul class="qc-songs-list">
                    <For each={songsPaging.paged()} fallback={<li class="list-empty">no songs match</li>}>
                      {(song) => {
                        const isQueued = () => snapshot()?.queue.some(item => item.songId === song.id)
                        const isEditing = () => editingSongId() === song.id
                        return (
                          <li class="qc-song-item" classList={{ editing: isEditing() }}>
                            <div class="qc-song-row">
                              <input
                                type="checkbox"
                                class="qc-song-select"
                                aria-label="select song"
                                checked={selectedSongIds().includes(song.id)}
                                onChange={(event) => toggleSongSelection(song.id, event.currentTarget.checked)}
                              />
                              <div class="qc-song-thumb">
                                <Show when={song.hasCover} fallback={<div class="qc-thumb-placeholder">{song.title.slice(0, 4).toUpperCase()}</div>}>
                                  <img src={coverUrl(song)} alt="" loading="lazy" />
                                </Show>
                              </div>
                              <div class="qc-song-info">
                                <span class="qc-song-title">{song.title}</span>
                                <span class="qc-song-meta-line">{song.artist} • {song.album || 'Single'}</span>
                              </div>
                              <Show when={benchedSongIds().has(song.id)}>
                                <span class="qc-album-badge benched" title="every album this song belongs to is out of rotation, so it never autoplays">benched</span>
                              </Show>
                              <div class="qc-song-genre-pill">{song.genre || 'General'}</div>
                              <div class="qc-song-duration">{formatTime(song.durationSeconds)}</div>
                              <button class="qc-add-btn" classList={{ 'already-queued': isQueued() }} onClick={() => void addSongToQueue(song.id)}>
                                + Add
                              </button>
                              <button
                                class="qc-more-btn"
                                aria-label={isEditing() ? 'close editor' : 'edit song details'}
                                aria-expanded={isEditing()}
                                title={isEditing() ? 'close editor' : 'edit song details'}
                                onClick={() => (isEditing() ? cancelSongEdit() : beginSongEdit(song))}
                              >...</button>
                            </div>
                            <Show when={isEditing()}>
                              <form
                                class="qc-song-editor"
                                onSubmit={(event) => { event.preventDefault(); void saveSongEdit(song.id) }}
                                onKeyDown={(event) => {
                                  if (event.key === 'Escape') cancelSongEdit()
                                }}
                              >
                                <div class="qc-song-editor-fields">
                                  <input aria-label="song title" placeholder="title" value={editTitle()} onInput={(event) => setEditTitle(event.currentTarget.value)} />
                                  <input aria-label="song artist" placeholder="artist" value={editArtist()} onInput={(event) => setEditArtist(event.currentTarget.value)} />
                                  <input aria-label="song album" placeholder="album" value={editAlbum()} onInput={(event) => setEditAlbum(event.currentTarget.value)} />
                                  <input aria-label="song genre" placeholder="genre" value={editGenre()} onInput={(event) => setEditGenre(event.currentTarget.value)} />
                                </div>
                                <div class="song-edit-actions">
                                  <label class="pill-button subtle cover-upload inline-cover-upload">
                                    <UploadCloud size={16} />
                                    cover
                                    <input type="file" accept="image/*" onChange={(event) => void replaceCover(song.id, event.currentTarget.files?.[0] ?? null)} />
                                  </label>
                                  <button class="pill-button" type="submit">save</button>
                                  <button class="pill-button subtle" type="button" onClick={cancelSongEdit}>cancel</button>
                                  <button
                                    class="pill-button subtle danger-button"
                                    type="button"
                                    onClick={() => {
                                      if (confirm(`Delete "${song.title}" from the library? This can't be undone.`)) {
                                        void removeSong(song.id)
                                      }
                                    }}
                                  >delete</button>
                                </div>
                              </form>
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
                <Show when={(albums() ?? []).length > 0}>
                  <div class="qc-rotation-bar">
                    <span class="qc-rotation-count">
                      {(albums() ?? []).filter((album) => album.isEnabled).length}/{(albums() ?? []).length} in rotation
                    </span>
                    <div class="qc-rotation-actions">
                      <button class="pill-button subtle" type="button" onClick={() => void setAllAlbumsEnabled(true)}>
                        add all to rotation
                      </button>
                      <button class="pill-button subtle danger-button" type="button" onClick={() => void setAllAlbumsEnabled(false)}>
                        clear rotation
                      </button>
                    </div>
                  </div>
                </Show>
                <Show when={!albums.loading} fallback={<p class="list-empty">loading albums...</p>}>
                  <ul class="qc-albums-list">
                    <For each={albumsPaging.paged()} fallback={<li class="list-empty">no albums match</li>}>
                      {(album) => {
                        const isExpanded = () => expandedAlbumId() === album.id
                        const duplicate = () => (albums() ?? []).find(a => a.id !== album.id && normalizeTitleForUi(a.title) === normalizeTitleForUi(album.title))

                        const coverTrack = () => album.tracks.find((track) => track.hasCover)
                        const totalMinutes = () => {
                          const seconds = album.tracks.reduce((sum, track) => sum + (track.durationSeconds ?? 0), 0)
                          return seconds >= 60 ? Math.round(seconds / 60) : 0
                        }
                        return (
                          <li class="qc-album-item" classList={{ expanded: isExpanded() }}>
                            <div
                              class="qc-album-row"
                              role="button"
                              aria-expanded={isExpanded()}
                              onClick={() => setExpandedAlbumId(isExpanded() ? null : album.id)}
                            >
                              <div class="qc-album-thumb">
                                <Show when={coverTrack()} fallback={<div class="qc-thumb-placeholder">{album.title.slice(0, 4).toUpperCase()}</div>}>
                                  {(track) => <img src={songCoverThumbnailUrl(track().id, selectedApiBase())} alt="" loading="lazy" />}
                                </Show>
                              </div>
                              <div class="qc-song-info">
                                <span class="qc-album-title-row">
                                  <span class="qc-song-title">{album.title}</span>
                                  <Show when={album.isEnabled}>
                                    <span class="qc-album-badge looping" title="plays automatically when the queue is empty">in rotation</span>
                                  </Show>
                                  <Show when={duplicate()}>
                                    <span class="qc-album-badge duplicate">duplicate</span>
                                  </Show>
                                </span>
                                <span class="qc-song-meta-line">
                                  {album.tracks.length} {album.tracks.length === 1 ? 'track' : 'tracks'}{totalMinutes() ? ` • ${totalMinutes()} min` : ''}
                                </span>
                              </div>
                              <button
                                class="qc-add-btn"
                                type="button"
                                aria-label="queue album"
                                disabled={album.tracks.length === 0}
                                onClick={(e) => {
                                  e.stopPropagation()
                                  void addAlbumToQueue(album.tracks.map((track) => track.id))
                                }}
                              >
                                + Add
                              </button>
                              <span class="qc-more-btn qc-album-expand" aria-hidden="true">
                                <ChevronDown size={18} />
                              </span>
                            </div>

                            <Show when={isExpanded()}>
                              <div class="qc-album-details">
                                <div class="qc-album-actions">
                                  <div class="qc-album-actions-group">
                                    <label class="inline-check qc-album-loop-check" title="plays automatically when the queue is empty">
                                      <input
                                        type="checkbox"
                                        checked={album.isEnabled}
                                        onChange={(e) => void handleSetAlbumEnabled(album.id, e.currentTarget.checked)}
                                      />
                                      in station rotation
                                    </label>
                                    <label class="qc-weight-label" title="how often shuffle picks from this album">
                                      rotation
                                      <select
                                        class="qc-weight-select"
                                        value={String(rotationInfo()?.weights[album.id] ?? 2)}
                                        onChange={(e) => void handleSetAlbumWeight(album.id, Number(e.currentTarget.value))}
                                      >
                                        <option value="1">light</option>
                                        <option value="2">normal</option>
                                        <option value="4">heavy</option>
                                      </select>
                                    </label>
                                    <button
                                      class="pill-button subtle danger-button"
                                      type="button"
                                      title="delete this album grouping; the songs stay in the library"
                                      onClick={() => void handleDeleteAlbum(album.id)}
                                    >
                                      ungroup album
                                    </button>
                                  </div>

                                  <div class="qc-album-actions-group">
                                    <Show when={duplicate()}>
                                      {(dup) => (
                                        <button
                                          class="pill-button subtle qc-merge-duplicate-btn"
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

                                    <select
                                      class="qc-album-merge-select"
                                      aria-label="merge this album into another"
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
                                      <option value="">merge into...</option>
                                      <For each={(albums() ?? []).filter(a => a.id !== album.id)}>
                                        {(other) => <option value={other.id}>{other.title}</option>}
                                      </For>
                                    </select>
                                  </div>
                                </div>

                                <ol class="qc-album-tracks">
                                  <For each={album.tracks}>
                                    {(track, idx) => (
                                      <li class="qc-album-track">
                                        <span class="qc-album-track-num">{idx() + 1}</span>
                                        <span class="qc-album-track-title">{track.title}</span>
                                        <span class="qc-album-track-artist">{track.artist}</span>
                                        <span class="qc-album-track-duration">{formatTime(track.durationSeconds)}</span>
                                      </li>
                                    )}
                                  </For>
                                </ol>
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
                  <div class="qc-column-header-actions">
                    <button class="qc-clear-btn" onClick={() => { setSavingQueue(!savingQueue()); setNewPlaylistName(''); }}>
                      Save Set
                    </button>
                    <button class="qc-clear-btn" onClick={() => void clearTheQueue()}>
                      <Trash2 size={14} />
                      Clear
                    </button>
                  </div>
                </Show>
              </div>

              <Show when={savingQueue()}>
                <div class="playlist-save-form">
                  <input
                    type="text"
                    class="qc-save-set-input"
                    placeholder="name your set"
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


              <span class="qc-column-stats qc-queue-stats">
                {snapshot()?.queue.length ?? 0} tracks • about {queueDurationMin()} min
              </span>

              <Show when={!snapshot.loading} fallback={<p class="list-empty">loading queue...</p>}>
                <ul class="qc-queue-list" ref={queueListEl}>
                  <For each={queuePaging.paged()} fallback={<li class="list-empty">queue is empty — the station plays from rotation</li>}>
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

              <div class="qc-after-queue-row" classList={{ 'is-silent': !afterQueueLabel() }}>
                <span class="qc-est-label">After queue</span>
                <span class="qc-after-queue-value">
                  {afterQueueLabel() ?? '⚠ silence — nothing in rotation'}
                </span>
              </div>

              <Show when={rotationInfo()?.upNext}>
                {(next) => (
                  <div class="qc-after-queue-row">
                    <span class="qc-est-label">Next from rotation</span>
                    <span class="qc-after-queue-value" title={`from ${next().source}`}>
                      {next().title} — {next().artist}
                    </span>
                  </div>
                )}
              </Show>

              <Show when={(rotationInfo()?.recentlyPlayed?.length ?? 0) > 0}>
                <details class="qc-recently-played">
                  <summary>recently played</summary>
                  <ul>
                    <For each={rotationInfo()?.recentlyPlayed ?? []}>
                      {(entry) => (
                        <li>
                          <span class="qc-recent-time">{new Date(entry.startedAt * 1000).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })}</span>
                          <span class="qc-recent-title">{entry.title}</span>
                          <span class="qc-recent-artist">{entry.artist}</span>
                        </li>
                      )}
                    </For>
                  </ul>
                </details>
              </Show>
            </div>
          </div>

          {/* Chat Panel at the bottom */}
          <ChatModerationPanel apiBase={selectedApiBase()} stationKey={selectedStationKey()} target={selectedRadioTarget()} />
        </div>
      </Show>
    </section>
  )
}
