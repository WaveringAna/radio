import { createEffect, createMemo, createResource, createSignal, onCleanup } from 'solid-js'
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
  renamePlaylist,
  addPlaylistTracks,
  removePlaylistTrack,
  reorderPlaylistTracks,
  duplicatePlaylist,
  setPlaylistShuffleOnLoad,
  setLoopMode,
  setLoopPlaylist,
  sequencePlaylistTracks,
  sequenceQueue,
  songCoverThumbnailUrl,
  SYNDICATION_WORKER_BASE,
  type LoopMode,
  type RadioEvent,
  type RadioTarget,
  type Song,
} from '../../shared/lib/radio'
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
import { fallbackProfile, formatClockTime } from './helpers'

export interface QueueControlProps {
  session?: SessionResponse
  sessionLoading: boolean
}

export type SearchMode = 'songs' | 'artists' | 'albums' | 'playlists'

export type QueueControl = ReturnType<typeof createQueueControl>

/**
 * Owns every piece of queue-control state: station targeting, the admin probe,
 * the live snapshot socket, the library resources, and all mutations. The page
 * and its panels are pure views over what this returns.
 */
export function createQueueControl(props: QueueControlProps) {
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
  const [coverVersions, setCoverVersions] = createSignal<Record<string, number>>({})
  const [clock, setClock] = createSignal(Date.now())
  const [snapshotSyncedAt, setSnapshotSyncedAt] = createSignal(Date.now())
  const inFlightDids = new Set<string>()

  const profileFor = (did: string) => profiles()[did] ?? fallbackProfile(did)
  const coverUrl = (song: Song) => `${songCoverThumbnailUrl(song.id, selectedApiBase())}?v=${coverVersions()[song.id] ?? song.createdAt}`

  const fail = (error: unknown, fallbackMessage: string) => {
    setPageError(error instanceof Error ? error.message : fallbackMessage)
  }

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
      mutate(await controlRadio(action, 'explicit_admin_action', selectedRadioTarget()))
    } catch (error) {
      fail(error, 'radio control faceplanted.')
    }
  }
  const shuffleOn = () => Boolean(snapshot()?.state.shuffle)

  const addSongToQueue = async (songId: string, atTop = false) => {
    try {
      setPageError(null)
      mutate(await enqueueSong(songId, selectedRadioTarget(), atTop))
      return true
    } catch (error) {
      fail(error, 'queue add faceplanted.')
      return false
    }
  }

  /**
   * `sequence` hands ordering to the backend's transition scorer — the same
   * artist/album separation, energy, genre, and BPM pairing the station uses
   * on itself — instead of preserving the given order.
   */
  const addAlbumToQueue = async (songIds: string[], atTop = false, sequence = false) => {
    try {
      setPageError(null)
      mutate(await enqueueAlbum(songIds, selectedRadioTarget(), atTop, sequence))
      return true
    } catch (error) {
      fail(error, 'album queue add faceplanted.')
      return false
    }
  }

  const removeFromQueue = async (queueId: string) => {
    try {
      setPageError(null)
      mutate(await removeQueueItem(queueId, selectedRadioTarget()))
    } catch (error) {
      fail(error, 'queue remove faceplanted.')
    }
  }

  // Clearing is the one destructive queue action with no natural undo, so the
  // song ids are stashed and can be re-queued until the next clear.
  const [clearedSongIds, setClearedSongIds] = createSignal<string[]>([])

  const clearTheQueue = async () => {
    const doomed = (snapshot()?.queue ?? []).filter((item) => !item.isShuffle).map((item) => item.songId)
    try {
      setPageError(null)
      mutate(await clearQueue(selectedRadioTarget()))
      setClearedSongIds(doomed)
      return true
    } catch (error) {
      fail(error, 'clear queue faceplanted.')
      return false
    }
  }

  const undoClearQueue = async () => {
    const ids = clearedSongIds()
    if (ids.length === 0) return
    if (await addAlbumToQueue(ids)) setClearedSongIds([])
  }

  const applyQueueOrder = async (queueIds: string[]) => {
    try {
      setPageError(null)
      mutate(await reorderQueue(queueIds, selectedRadioTarget()))
    } catch (error) {
      fail(error, 'reorder faceplanted.')
    }
  }

  /** Resequences the pending queue by transition score. */
  const shuffleQueueOrder = async () => {
    if ((snapshot()?.queue ?? []).length < 2) return
    try {
      setPageError(null)
      mutate(await sequenceQueue(selectedRadioTarget()))
    } catch (error) {
      fail(error, 'resequencing the queue faceplanted.')
    }
  }

  const moveQueueItem = async (queueId: string, to: 'top' | 'bottom') => {
    const ids = (snapshot()?.queue ?? []).map((item) => item.id)
    const from = ids.indexOf(queueId)
    if (from < 0) return
    const next = [...ids]
    next.splice(from, 1)
    if (to === 'top') next.unshift(queueId)
    else next.push(queueId)
    await applyQueueOrder(next)
  }

  const handleSetAlbumEnabled = async (albumId: string, enabled: boolean) => {
    try {
      setPageError(null)
      await setAlbumEnabled(albumId, enabled, selectedRadioTarget())
      void refetchAlbums()
    } catch (error) {
      fail(error, 'failed to update album rotation.')
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
      fail(error, 'failed to update album rotation.')
      void refetchAlbums()
    }
  }

  /** Bulk rotation toggle for an arbitrary group of albums (e.g. one artist's). */
  const setAlbumsEnabled = async (albumIds: string[], enabled: boolean) => {
    if (albumIds.length === 0) return false
    try {
      setPageError(null)
      for (const albumId of albumIds) {
        await setAlbumEnabled(albumId, enabled, selectedRadioTarget())
      }
      void refetchAlbums()
      return true
    } catch (error) {
      fail(error, 'failed to update album rotation.')
      void refetchAlbums()
      return false
    }
  }

  const handleSetAlbumWeight = async (albumId: string, weight: number) => {
    try {
      setPageError(null)
      await setAlbumWeight(albumId, weight, selectedRadioTarget())
      void refetchRotationInfo()
    } catch (error) {
      fail(error, 'failed to update rotation weight.')
    }
  }

  const refreshLibrary = async () => {
    await Promise.all([refetchSongs(), refetchAlbums()])
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
      fail(error, 'failed to delete album loop.')
    }
  }

  const handleMergeAlbums = async (sourceId: string, targetId: string) => {
    try {
      setPageError(null)
      await mergeAlbums(sourceId, targetId, selectedRadioTarget())
      void refetchAlbums()
      void refetchSongs()
    } catch (error) {
      fail(error, 'failed to merge albums.')
    }
  }

  const saveAsPlaylist = async (name: string, songIds: string[]) => {
    const trimmed = name.trim()
    if (!trimmed || songIds.length === 0) return false
    try {
      setPageError(null)
      await createPlaylist(trimmed, songIds, selectedRadioTarget())
      await refetchPlaylists()
      return true
    } catch (error) {
      fail(error, 'failed to save set')
      return false
    }
  }

  const removePlaylist = async (id: string) => {
    try {
      setPageError(null)
      await deletePlaylist(id, selectedRadioTarget())
      await refetchPlaylists()
    } catch (error) {
      fail(error, 'failed to delete set')
    }
  }

  const loadPlaylistToQueue = async (id: string, replace: boolean, shuffle?: boolean) => {
    try {
      setPageError(null)
      mutate(await loadPlaylist(id, replace, selectedRadioTarget(), shuffle))
    } catch (error) {
      fail(error, 'failed to load set')
    }
  }

  /** Runs one of the set-editing calls and refreshes the sets list. */
  const editPlaylist = async (run: () => Promise<unknown>, fallbackMessage: string) => {
    try {
      setPageError(null)
      await run()
      await refetchPlaylists()
      return true
    } catch (error) {
      fail(error, fallbackMessage)
      return false
    }
  }

  const target = () => selectedRadioTarget()

  const renamePlaylistTo = (id: string, name: string) =>
    editPlaylist(() => renamePlaylist(id, name, target()), 'failed to rename set')

  const appendToPlaylist = (id: string, songIds: string[]) =>
    editPlaylist(() => addPlaylistTracks(id, songIds, target()), 'failed to add tracks to set')

  const dropPlaylistTrack = (id: string, position: number) =>
    editPlaylist(() => removePlaylistTrack(id, position, target()), 'failed to remove track from set')

  const reorderPlaylist = (id: string, songIds: string[]) =>
    editPlaylist(() => reorderPlaylistTracks(id, songIds, target()), 'failed to reorder set')

  const copyPlaylist = (id: string, name: string) =>
    editPlaylist(() => duplicatePlaylist(id, name, target()), 'failed to duplicate set')

  const resequencePlaylist = (id: string) =>
    editPlaylist(() => sequencePlaylistTracks(id, target()), 'failed to resequence set')

  const togglePlaylistShuffleOnLoad = (id: string, shuffleOnLoad: boolean) =>
    editPlaylist(
      () => setPlaylistShuffleOnLoad(id, shuffleOnLoad, target()),
      'failed to update set shuffle',
    )

  const loopMode = (): LoopMode => snapshot()?.state.loopMode ?? 'off'
  const loopPlaylistId = () => snapshot()?.state.loopPlaylistId ?? null

  const applyLoopMode = async (mode: LoopMode) => {
    try {
      setPageError(null)
      mutate(await setLoopMode(mode, selectedRadioTarget()))
    } catch (error) {
      fail(error, 'failed to change loop mode')
    }
  }

  const applyLoopPlaylist = async (playlistId: string | null) => {
    try {
      setPageError(null)
      mutate(await setLoopPlaylist(playlistId, selectedRadioTarget()))
    } catch (error) {
      fail(error, 'failed to pin the looping set')
    }
  }

  const saveSongMetadata = async (
    songId: string,
    fields: { title: string; artist: string; album: string | null; genre: string | null },
  ) => {
    const currentSong = (songs() ?? []).find((song) => song.id === songId)
    try {
      setPageError(null)
      await updateSongMetadata(songId, {
        ...fields,
        durationSeconds: currentSong?.durationSeconds ?? null,
      }, selectedRadioTarget())
      await refreshLibrary()
      void refetch()
      return true
    } catch (error) {
      fail(error, 'song metadata update faceplanted.')
      return false
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
      fail(error, 'cover upload faceplanted.')
    }
  }

  const removeSong = async (songId: string) => {
    try {
      setPageError(null)
      await deleteSong(songId, selectedRadioTarget())
      await refreshLibrary()
      void refetch()
    } catch (error) {
      fail(error, 'song delete faceplanted.')
    }
  }

  const estimatedEndTime = createMemo(() => {
    let totalSeconds = 0

    const current = snapshot()?.currentSong
    if (current && current.durationSeconds) {
      totalSeconds += Math.max(0, current.durationSeconds - livePositionSeconds())
    }
    for (const item of snapshot()?.queue ?? []) {
      totalSeconds += item.durationSeconds || 0
    }
    if (totalSeconds === 0) return '--:--'
    return formatClockTime(totalSeconds * 1000)
  })

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

  // What the station will do once the queue drains. The priority chain is
  // queue → loop-queue → loop-set → shuffle → album loops + singles → silence.
  const afterQueueLabel = createMemo(() => {
    if (snapshot()?.state.loopMode === 'one') return 'repeat one (the current track, forever)'
    if (snapshot()?.state.loopMode === 'queue') return 'loop queue (it never drains)'
    const pinned = loopPlaylistId()
    if (pinned) {
      const set = (playlists() ?? []).find((playlist) => playlist.id === pinned)
      return `loop set${set ? ` — “${set.name}”` : ''}`
    }
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

  // How many copies of each song are pending, so rows can say "×2" rather than
  // silently accepting a duplicate the DJ didn't mean to add.
  const queuedCounts = createMemo(() => {
    const counts = new Map<string, number>()
    for (const item of snapshot()?.queue ?? []) {
      counts.set(item.songId, (counts.get(item.songId) ?? 0) + 1)
    }
    return counts
  })
  const queuedCountFor = (songId: string) => queuedCounts().get(songId) ?? 0

  const queueDurationMin = createMemo(() => {
    const totalSeconds = (snapshot()?.queue ?? []).reduce((acc, item) => acc + (item.durationSeconds || 0), 0)
    return Math.round(totalSeconds / 60)
  })

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

  return {
    // station + access
    selectedStation,
    selectedApiBase,
    selectedStationKey,
    selectedRadioTarget,
    isAdmin,
    queueControlGate,
    // resources
    snapshot,
    songs,
    albums,
    playlists,
    rotationInfo,
    refetchSongs,
    refetchAlbums,
    refetchPlaylists,
    refreshLibrary,
    // page-level status
    pageError,
    setPageError,
    profileFor,
    coverUrl,
    // playback + queue
    livePositionSeconds,
    liveProgressPercent,
    sendControl,
    shuffleOn,
    addSongToQueue,
    addAlbumToQueue,
    removeFromQueue,
    clearTheQueue,
    undoClearQueue,
    clearedSongIds,
    applyQueueOrder,
    shuffleQueueOrder,
    moveQueueItem,
    // library maintenance
    handleSetAlbumEnabled,
    setAllAlbumsEnabled,
    setAlbumsEnabled,
    handleSetAlbumWeight,
    handleDeleteAlbum,
    handleMergeAlbums,
    saveSongMetadata,
    replaceCover,
    removeSong,
    // sets
    saveAsPlaylist,
    removePlaylist,
    loadPlaylistToQueue,
    renamePlaylistTo,
    appendToPlaylist,
    dropPlaylistTrack,
    reorderPlaylist,
    copyPlaylist,
    resequencePlaylist,
    togglePlaylistShuffleOnLoad,
    // loop
    loopMode,
    loopPlaylistId,
    applyLoopMode,
    applyLoopPlaylist,
    // derived
    estimatedEndTime,
    queueAirTimes,
    afterQueueLabel,
    benchedSongIds,
    queueDurationMin,
    queuedCountFor,
  }
}
