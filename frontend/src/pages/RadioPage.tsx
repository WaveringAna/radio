import { createEffect, createMemo, createResource, createSignal, For, onCleanup, Show, untrack } from 'solid-js'
import { ListPlus, Trash2, Volume2 } from 'lucide-solid'
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
import { createPagedList } from '../primitives/createPagedList'

interface RadioPageProps {
  isAdmin: boolean
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

/**
 * Renders the public radio view with admin-only upload and playback controls.
 * @param props Current viewer permissions.
 * @returns The radio page view.
 */
export default function RadioPage(props: RadioPageProps) {
  const [snapshot, { mutate, refetch }] = createResource(fetchRadioSnapshot)
  const [songs, { refetch: refetchSongs }] = createResource(fetchSongs)
  const [albums, { refetch: refetchAlbums }] = createResource(() => props.isAdmin, (enabled) => (enabled ? fetchAlbums() : []))
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

  // Local playback state. Frontend self-advances through localQueue; backend
  // resyncs only on admin actions (detected via playbackKey diff).
  const [localCurrentSong, setLocalCurrentSong] = createSignal<Song | null>(null)
  const [localQueue, setLocalQueue] = createSignal<QueueItem[]>([])
  let consumedQueueIds = new Set<string>()
  let lastPlaybackKey: string | null = null
  let audioRef: HTMLAudioElement | undefined

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
        setLocalQueue(snap.queue.filter((item) => !consumedQueueIds.has(item.id)))
      }
      if (!isFirst) {
        void applyBackendPlayback(snap.state, snap.currentSong ?? null, songChanged)
      }
    } else {
      setLocalQueue(snap.queue.filter((item) => !consumedQueueIds.has(item.id)))
    }
  })

  createEffect(() => {
    const socket = openRadioSocket()

    socket.addEventListener('message', (message) => {
      const event = JSON.parse(message.data) as RadioEvent
      if (event.type === 'snapshotChanged') {
        mutate(event.snapshot)
        // Don't refetch songs/albums here — most snapshot events are queue
        // mutations that don't affect the library. Local actions that do
        // change the library refetch explicitly.
      }
    })

    socket.addEventListener('close', () => {
      void refetch()
      void refetchSongs()
      void refetchAlbums()
    })

    onCleanup(() => socket.close())
  })

  createEffect(() => {
    const dids = [
      ...(songs() ?? []).map((song) => song.addedByDid),
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
      await enqueueSong(songId)
      // WS broadcast will deliver the updated snapshot.
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
    <section class="radio-page">
      <div class="now-playing-card">
        <Show
          when={currentSong()?.hasCover}
          fallback={
            <div class="art-shell">
              <div class="art-glow" />
            </div>
          }
        >
          <img class="art-shell album-cover" src={`${API_BASE}/api/songs/${currentSong()?.id}/cover`} alt="" />
        </Show>
        <p class="eyebrow">live radio</p>
        <h1>{currentSong()?.title ?? 'nothing playing yet'}</h1>
        <p class="subtitle">{currentSong()?.artist ?? 'queue something lovely'}</p>
        <Show when={currentSong()?.album}>{(album) => <p class="muted">{album()}</p>}</Show>
        <audio
          ref={audioRef}
          class="radio-audio"
          src={currentAudioUrl() ?? ''}
          preload="auto"
          onPlay={() => setIsAudioPlaying(true)}
          onPause={() => setIsAudioPlaying(false)}
          onEnded={handleAudioEnded}
        />
        <Show when={nextAudioUrl()}>
          {(url) => <audio src={url()} preload="auto" aria-hidden="true" style="display:none" />}
        </Show>
        <div class="listener-controls">
          <Show when={currentSong() && snapshot()?.state.status === 'playing'}>
            <button class="listen-button" type="button" onClick={() => void startListening()}>
              {isAudioPlaying() ? 'listening live' : 'click to listen live'}
            </button>
          </Show>
          <label class="volume-control local-volume">
            <Volume2 size={17} />
            <input
              type="range"
              min="0"
              max="1"
              step="0.01"
              value={volume()}
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

        <section class="glass-card">
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

        <Show when={props.isAdmin}>
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

      <Show when={props.isAdmin}>
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
  )
}
