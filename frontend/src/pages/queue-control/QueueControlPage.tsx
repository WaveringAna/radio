import { createEffect, createMemo, createResource, createSignal, For, onCleanup, Show } from 'solid-js'
import {
  CircleUserRound,
  GripVertical,
  ListPlus,
  LoaderCircle,
  LockKeyhole,
  Pause,
  Play,
  Plus,
  RadioTower,
  SkipForward,
  Trash2,
  UploadCloud,
  X,
} from 'lucide-solid'
import { AdminUploadPanel } from '../../features/upload/AdminUploadPanel'
import { ChatModerationPanel } from './ChatModerationPanel'
import { PaginationRow } from '../../shared/components/PaginationRow'
import { ProfileAvatar } from '../../shared/components/ProfileAvatar'
import { SearchableDropdown } from '../../shared/components/SearchableDropdown'
import { resolveAtprotoProfile, type AtprotoProfile } from '../../shared/lib/atproto'
import { fetchAdminPermissions, type SessionResponse } from '../../shared/lib/auth'
import {
  canUseRadioXrpcTarget,
  clearQueue,
  controlRadio,
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
  createPlaylist,
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
import {
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
  const adminProbeSource = () => props.session?.authenticated && selectedRadioCanUseXrpc() ? selectedStationKey() : null
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
  const [songFilterDid, setSongFilterDid] = createSignal('')
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
  const [selectedSongIds, setSelectedSongIds] = createSignal<string[]>([])
  const [editingSongId, setEditingSongId] = createSignal<string | null>(null)
  const [editTitle, setEditTitle] = createSignal('')
  const [editArtist, setEditArtist] = createSignal('')
  const [editAlbum, setEditAlbum] = createSignal('')
  const [editGenre, setEditGenre] = createSignal('')
  const [coverVersions, setCoverVersions] = createSignal<Record<string, number>>({})
  const [draggingQueueId, setDraggingQueueId] = createSignal<string | null>(null)
  const [clock, setClock] = createSignal(Date.now())
  const [snapshotSyncedAt, setSnapshotSyncedAt] = createSignal(Date.now())
  const [savingQueue, setSavingQueue] = createSignal(false)
  const [savingSelection, setSavingSelection] = createSignal(false)
  const [newPlaylistName, setNewPlaylistName] = createSignal('')
  const inFlightDids = new Set<string>()
  const pageSize = 8

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

  const sendControl = async (action: 'play' | 'pause' | 'stop' | 'skip') => {
    try {
      setPageError(null)
      mutate(await controlRadio(action, 'explicit_admin_action', selectedRadioTarget()))
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'radio control faceplanted.')
    }
  }

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

  const genres = createMemo(() => {
    const list = (songs() ?? [])
      .map((song) => song.genre?.trim())
      .filter((genre): genre is string => !!genre)
    return [...new Set(list)].sort()
  })

  const artists = createMemo(() => {
    const list = (songs() ?? [])
      .map((song) => song.artist?.trim())
      .filter((artist): artist is string => !!artist)
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
      setPageError(null)
      if (replace) {
        await clearQueue(selectedRadioTarget())
      }
      mutate(await enqueueAlbum(shuffled, selectedRadioTarget()))
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'shuffle genre failed')
    }
  }

  const shuffleLibraryByArtist = async (artist: string, replace: boolean) => {
    if (!artist) return
    const matchingSongIds = (songs() ?? [])
      .filter((song) => song.artist?.toLowerCase() === artist.toLowerCase())
      .map((song) => song.id)
    if (matchingSongIds.length === 0) return

    const shuffled = [...matchingSongIds].sort(() => Math.random() - 0.5)

    try {
      setPageError(null)
      if (replace) {
        await clearQueue(selectedRadioTarget())
      }
      mutate(await enqueueAlbum(shuffled, selectedRadioTarget()))
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'shuffle artist failed')
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

  const toggleSongSelection = (songId: string, checked: boolean) => {
    setSelectedSongIds((current) => (checked ? [...current, songId] : current.filter((id) => id !== songId)))
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
      >
        <span class="queue-drag-handle" aria-hidden="true">
          <GripVertical size={14} strokeWidth={1.7} />
        </span>
        <span class="queue-number">{queuePaging.page() * pageSize + index() + 1}</span>
        <ProfileAvatar profile={profile()} />
        <div class="song-copy">
          <span>{item.title}</span>
          <small>{item.artist}{item.album ? ` · ${item.album}` : ''}</small>
        </div>
        <small class="profile-handle">@{profile().handle}</small>
        <button class="icon-button" type="button" aria-label="remove from queue" onClick={() => void removeFromQueue(item.id)}>
          <Trash2 size={17} />
        </button>
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

        <div class="qc-split">
          <section class="qc-now">
              <header class="qc-station-bar">
                <div class="qc-station-identity">
                  <span class="qc-live-mark" aria-hidden="true" />
                  <strong>{selectedStation().name}</strong>
                  <span title={selectedStation().url}>{labelFromStationUrl(selectedStation().url)}</span>
                </div>
                <span
                  class="qc-station-state"
                  classList={{ 'is-playing': snapshot()?.state.status === 'playing' }}
                >
                  <span aria-hidden="true" />
                  {snapshot()?.state.status ?? 'connecting'}
                </span>
              </header>

              <div class="qc-on-air">
                <div class="qc-art">
                  <Show
                    when={snapshot()?.currentSong?.hasCover}
                    fallback={<div class="qc-art-glow" aria-hidden="true" />}
                  >
                    <img class="qc-art-cover" src={songCoverUrl(snapshot()?.currentSong?.id ?? '', selectedApiBase())} alt="" />
                  </Show>
                </div>

                <div class="qc-on-air-copy">
                  <p class="eyebrow qc-eyebrow">now playing</p>
                  <Show
                    when={snapshot()?.currentSong}
                    fallback={<h2 class="qc-title qc-title-empty">nothing playing yet</h2>}
                  >
                    {(song) => (
                      <>
                        <h2 class="qc-title" title={song().title}>{song().title}</h2>
                        <p class="qc-artist">{song().artist}</p>
                        <Show when={song().album}>{(album) => <p class="qc-album">{album()}</p>}</Show>
                        <div class="qc-progress-group">
                          <div
                            class="qc-progress-track"
                            role="progressbar"
                            aria-label="song progress"
                            aria-valuemin="0"
                            aria-valuemax="100"
                            aria-valuenow={Math.round(liveProgressPercent())}
                          >
                            <span style={`width: ${liveProgressPercent()}%`} />
                          </div>
                          <div class="qc-time-row">
                            <span>{formatTime(Math.min(livePositionSeconds(), song().durationSeconds ?? Infinity))}</span>
                            <span>{formatTime(song().durationSeconds)}</span>
                          </div>
                        </div>
                      </>
                    )}
                  </Show>
                </div>

                <div class="qc-on-air-actions">
                  <div class="queue-transport-panel" aria-label="radio transport controls">
                    <button class="icon-button primary" type="button" aria-label="play" title="play" onClick={() => void sendControl('play')}>
                      <Play size={18} fill="currentColor" />
                    </button>
                    <button class="icon-button" type="button" aria-label="pause" title="pause" onClick={() => void sendControl('pause')}>
                      <Pause size={16} />
                    </button>
                    <button class="icon-button" type="button" aria-label="skip" title="skip" onClick={() => void sendControl('skip')}>
                      <SkipForward size={16} />
                    </button>
                  </div>
                </div>
              </div>
          </section>

          <div class="qc-left">
            <section class="qc-queue">
              <div class="section-heading qc-queue-heading">
                <div class="qc-heading-copy">
                  <p class="eyebrow">up next</p>
                  <span>{snapshot()?.queue.length ?? 0} queued</span>
                </div>
                <Show
                  when={(snapshot()?.queue.length ?? 0) > 0}
                >
                  <div class="queue-action-buttons">
                    <button class="pill-button subtle" type="button" onClick={() => { setSavingQueue(!savingQueue()); setNewPlaylistName(''); }}>
                      {savingQueue() ? 'cancel' : 'save set'}
                    </button>
                    <button class="pill-button subtle qc-clear" type="button" onClick={() => void clearTheQueue()}>
                      clear ({snapshot()?.queue.length})
                    </button>
                  </div>
                </Show>
              </div>

              <Show when={savingQueue()}>
                <div class="playlist-save-inline">
                  <input
                    type="text"
                    placeholder="name your playlist/set"
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
              <Show when={!snapshot.loading} fallback={<p class="list-empty">loading queue...</p>}>
                <ul class="song-list queue-control-list">
                  <For each={queuePaging.paged()} fallback={<li class="list-empty">queue is empty</li>}>
                    {renderQueueItem}
                  </For>
                </ul>
                <Show when={queuePaging.pageCount() > 1}>
                  <PaginationRow page={queuePaging.page()} pageCount={queuePaging.pageCount()} onPageChange={queuePaging.setPage} compact />
                </Show>
              </Show>
            </section>

            <ChatModerationPanel apiBase={selectedApiBase()} stationKey={selectedStationKey()} target={selectedRadioTarget()} />
          </div>

          <div class="qc-right">
            <section class="library-control-card">
              <div class="section-heading qc-library-heading">
                <div class="qc-heading-copy">
                  <p class="eyebrow">library</p>
                  <span>
                    {searchMode() === 'songs'
                      ? `${filteredSongs().length} songs`
                      : searchMode() === 'albums'
                        ? `${filteredAlbums().length} albums`
                        : `${(playlists() ?? []).length} sets`}
                  </span>
                </div>
                <button
                  class="icon-button qc-intake-toggle"
                  classList={{ 'is-active': showIntake() }}
                  type="button"
                  aria-label={showIntake() ? 'close add music' : 'add music'}
                  aria-pressed={showIntake()}
                  title={showIntake() ? 'close add music' : 'add music'}
                  onClick={() => setShowIntake(!showIntake())}
                >
                  <Show when={showIntake()} fallback={<Plus size={18} strokeWidth={1.8} />}>
                    <X size={18} strokeWidth={1.8} />
                  </Show>
                </button>
              </div>
              <Show when={showIntake()}>
                <div class="qc-intake" aria-label="add music">
                  <div class="section-heading">
                    <p class="eyebrow">add music</p>
                  </div>
                  <AdminUploadPanel
                    target={selectedRadioTarget()}
                    onSongAdded={() => void refreshLibrary()}
                    onError={setPageError}
                  />
                </div>
              </Show>
              <div class="upload-mode-tabs library-tabs" role="tablist" aria-label="library search mode">
                  <button class="pill-button" classList={{ subtle: searchMode() !== 'songs' }} type="button" role="tab" aria-selected={searchMode() === 'songs'} onClick={() => setSearchMode('songs')}>
                    songs
                  </button>
                  <button
                    class="pill-button"
                    classList={{ subtle: searchMode() !== 'albums' }}
                    type="button"
                    role="tab"
                    aria-selected={searchMode() === 'albums'}
                    disabled={libraryAction() === 'edit'}
                    onClick={() => setSearchMode('albums')}
                  >
                    albums
                  </button>
                  <button
                    class="pill-button"
                    classList={{ subtle: searchMode() !== 'playlists' }}
                    type="button"
                    role="tab"
                    aria-selected={searchMode() === 'playlists'}
                    disabled={libraryAction() === 'edit'}
                    onClick={() => setSearchMode('playlists')}
                  >
                    sets
                  </button>
                  <button
                    class="pill-button"
                    classList={{ subtle: libraryAction() !== 'edit' }}
                    type="button"
                    role="tab"
                    aria-selected={libraryAction() === 'edit'}
                    onClick={() => {
                      const next: LibraryAction = libraryAction() === 'edit' ? 'queue' : 'edit'
                      setLibraryAction(next)
                      if (next === 'edit') setSearchMode('songs')
                    }}
                  >
                    edit
                  </button>
              </div>

              <Show when={searchMode() === 'songs'}>
                <div class="qc-shuffle-bar">
                  <div class="qc-shuffle-select-wrapper">
                    <label>shuffle genre</label>
                    <SearchableDropdown
                      options={genres()}
                      placeholder="select genre..."
                      onSelect={(val) => void shuffleLibraryByGenre(val, true)}
                    />
                  </div>
                  <div class="qc-shuffle-select-wrapper">
                    <label>shuffle artist</label>
                    <SearchableDropdown
                      options={artists()}
                      placeholder="select artist..."
                      onSelect={(val) => void shuffleLibraryByArtist(val, true)}
                    />
                  </div>
                </div>

                <div class="song-filters song-filters-wide">
                  <input placeholder="title" value={songFilterTitle()} onInput={(event) => setSongFilterTitle(event.currentTarget.value)} />
                  <input placeholder="artist" value={songFilterArtist()} onInput={(event) => setSongFilterArtist(event.currentTarget.value)} />
                  <input placeholder="genre" value={songFilterGenre()} onInput={(event) => setSongFilterGenre(event.currentTarget.value)} />
                  <input placeholder="@handle or did" value={songFilterDid()} onInput={(event) => setSongFilterDid(event.currentTarget.value)} />
                </div>
                <Show when={libraryAction() === 'queue' && selectedSongIds().length > 0}>
                  <div class="multi-add-row queue-selection-row">
                    <button class="pill-button" type="button" onClick={() => void addSelectedToQueue()}>
                      add {selectedSongIds().length} to queue
                    </button>
                    <button class="pill-button subtle" type="button" onClick={() => { setSavingSelection(!savingSelection()); setNewPlaylistName(''); }}>
                      {savingSelection() ? 'cancel' : 'save as set'}
                    </button>
                    <button class="pill-button subtle" type="button" onClick={() => setSelectedSongIds([])}>
                      clear
                    </button>
                  </div>
                  <Show when={savingSelection()}>
                    <div class="playlist-save-inline">
                      <input
                        type="text"
                        placeholder="name your set"
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
                </Show>
                <Show when={!songs.loading} fallback={<p class="list-empty">loading songs...</p>}>
                  <ul class="song-list" classList={{ 'library-edit-list': libraryAction() === 'edit' }}>
                    <For each={songsPaging.paged()} fallback={<li class="list-empty">no songs match</li>}>
                      {(song) => {
                        const profile = () => profileFor(song.addedByDid)
                        return (
                          <li classList={{ editing: editingSongId() === song.id }}>
                            <Show
                              when={libraryAction() === 'edit'}
                              fallback={
                                <>
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

              <Show when={searchMode() === 'albums'}>
                <div class="song-filters album-search-row">
                  <input placeholder="album, track, or artist" value={albumFilter()} onInput={(event) => setAlbumFilter(event.currentTarget.value)} />
                </div>
                <Show when={!albums.loading} fallback={<p class="list-empty">loading albums...</p>}>
                  <ul class="song-list album-loop-list">
                    <For each={albumsPaging.paged()} fallback={<li class="list-empty">no albums match</li>}>
                      {(album) => {
                        const isExpanded = () => expandedAlbumId() === album.id;
                        const duplicate = () => (albums() ?? []).find(a => a.id !== album.id && normalizeTitleForUi(a.title) === normalizeTitleForUi(album.title));

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
                                    e.stopPropagation();
                                    void addAlbumToQueue(album.tracks.map((track) => track.id));
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
                                              void handleMergeAlbums(album.id, dup().id);
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
                                          const val = e.currentTarget.value;
                                          if (val) {
                                            const target = (albums() ?? []).find(a => a.id === val);
                                            if (target && confirm(`Merge this album "${album.title}" into "${target.title}"? All tracks will be combined under "${target.title}".`)) {
                                              void handleMergeAlbums(album.id, val);
                                            }
                                            e.currentTarget.value = "";
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
                        );
                      }}
                    </For>
                  </ul>
                  <Show when={albumsPaging.pageCount() > 1}>
                    <PaginationRow page={albumsPaging.page()} pageCount={albumsPaging.pageCount()} onPageChange={albumsPaging.setPage} />
                  </Show>
                </Show>
              </Show>

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
            </section>
          </div>
        </div>
      </Show>
    </section>
  )
}
