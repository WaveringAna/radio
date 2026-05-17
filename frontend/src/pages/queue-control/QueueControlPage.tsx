import { createEffect, createMemo, createResource, createSignal, For, onCleanup, Show } from 'solid-js'
import { ListPlus, Pause, Play, SkipForward, Trash2, UploadCloud } from 'lucide-solid'
import { AdminUploadPanel } from '../../features/upload/AdminUploadPanel'
import { PaginationRow } from '../../shared/components/PaginationRow'
import { ProfileAvatar } from '../../shared/components/ProfileAvatar'
import { resolveAtprotoProfile, type AtprotoProfile } from '../../shared/lib/atproto'
import {
  API_BASE,
  clearQueue,
  controlRadio,
  deleteSong,
  enqueueAlbum,
  enqueueSong,
  fetchAlbums,
  fetchRadioSnapshot,
  fetchSongs,
  openRadioSocket,
  removeQueueItem,
  reorderQueue,
  updateSongMetadata,
  uploadSongCover,
  type QueueItem,
  type RadioEvent,
  type Song,
} from '../../shared/lib/radio'
import { createPagedList } from '../../shared/primitives/createPagedList'

interface QueueControlPageProps {
  isAdmin: boolean
}

type SearchMode = 'songs' | 'albums'
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
  const [snapshot, { mutate, refetch }] = createResource(() => props.isAdmin, (enabled) => (enabled ? fetchRadioSnapshot() : undefined))
  const [songs, { refetch: refetchSongs }] = createResource(() => props.isAdmin, (enabled) => (enabled ? fetchSongs() : []))
  const [albums, { refetch: refetchAlbums }] = createResource(() => props.isAdmin, (enabled) => (enabled ? fetchAlbums() : []))
  const [profiles, setProfiles] = createSignal<Record<string, AtprotoProfile>>({})
  const [pageError, setPageError] = createSignal<string | null>(null)
  const [songFilterTitle, setSongFilterTitle] = createSignal('')
  const [songFilterArtist, setSongFilterArtist] = createSignal('')
  const [songFilterGenre, setSongFilterGenre] = createSignal('')
  const [songFilterDid, setSongFilterDid] = createSignal('')
  const [albumFilter, setAlbumFilter] = createSignal('')
  const [searchMode, setSearchMode] = createSignal<SearchMode>('songs')
  const [libraryAction, setLibraryAction] = createSignal<LibraryAction>('queue')
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
  const inFlightDids = new Set<string>()
  const pageSize = 8

  const profileFor = (did: string) => profiles()[did] ?? fallbackProfile(did)

  createEffect(() => {
    const interval = window.setInterval(() => setClock(Date.now()), 1000)
    onCleanup(() => window.clearInterval(interval))
  })

  createEffect(() => {
    if (snapshot()) setSnapshotSyncedAt(Date.now())
  })

  createEffect(() => {
    if (!props.isAdmin) return
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

  const refreshLibrary = async () => {
    await Promise.all([refetchSongs(), refetchAlbums()])
  }

  const coverUrl = (song: Song) => `${API_BASE}/api/songs/${song.id}/cover/thumbnail?v=${coverVersions()[song.id] ?? song.createdAt}`

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
      })
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
      await uploadSongCover(songId, file)
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
      await deleteSong(songId)
      await refreshLibrary()
      void refetch()
    } catch (error) {
      setPageError(error instanceof Error ? error.message : 'song delete faceplanted.')
    }
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
      <Show when={props.isAdmin} fallback={<p class="glass-card queue-control-empty">queue control is admin-only.</p>}>
        <Show when={pageError()}>{(message) => <p class="error-copy queue-control-error">{message()}</p>}</Show>

        <div class="qc-split">
          <div class="qc-left">
            <section class="qc-now">
              <div class="qc-art">
                <Show
                  when={snapshot()?.currentSong?.hasCover}
                  fallback={<div class="qc-art-glow" aria-hidden="true" />}
                >
                  <img class="qc-art-cover" src={`${API_BASE}/api/songs/${snapshot()?.currentSong?.id}/cover`} alt="" />
                </Show>
              </div>

              <p class="eyebrow qc-eyebrow">now playing // live rite</p>
              <Show
                when={snapshot()?.currentSong}
                fallback={<h2 class="qc-title qc-title-empty">nothing playing yet</h2>}
              >
                {(song) => (
                  <>
                    <h2 class="qc-title" title={song().title}>{song().title}</h2>
                    <p class="qc-artist">{song().artist}</p>
                    <Show when={song().album}>{(album) => <p class="qc-album">{album()}</p>}</Show>
                  </>
                )}
              </Show>

              <div class="qc-transport-strip">
                <div class="queue-transport-panel" aria-label="radio transport controls">
                  <button class="icon-button primary" type="button" aria-label="play" onClick={() => void sendControl('play')}>
                    <Play size={18} fill="currentColor" />
                  </button>
                  <button class="icon-button" type="button" aria-label="pause" onClick={() => void sendControl('pause')}>
                    <Pause size={16} />
                  </button>
                  <button class="icon-button" type="button" aria-label="skip" onClick={() => void sendControl('skip')}>
                    <SkipForward size={16} />
                  </button>
                </div>
                <Show
                  when={snapshot()?.currentSong}
                  fallback={<small class="qc-time">—:— / —:—</small>}
                >
                  {(song) => (
                    <small class="qc-time">
                      {formatTime(Math.min(livePositionSeconds(), song().durationSeconds ?? Infinity))} / {formatTime(song().durationSeconds)}
                    </small>
                  )}
                </Show>
                <Show
                  when={(snapshot()?.queue.length ?? 0) > 0}
                  fallback={<span class="qc-clear-spacer" aria-hidden="true" />}
                >
                  <button class="pill-button subtle qc-clear" type="button" onClick={() => void clearTheQueue()}>
                    clear ({snapshot()?.queue.length})
                  </button>
                </Show>
              </div>
            </section>

            <section class="qc-queue">
              <div class="section-heading">
                <p class="eyebrow">up next</p>
                <span class="qc-hint">drag to reorder</span>
              </div>
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
          </div>

          <div class="qc-right">
            <AdminUploadPanel onSongAdded={() => void refreshLibrary()} error={pageError()} onError={setPageError} />

            <section class="library-control-card">
              <div class="section-heading">
                <p class="eyebrow">library · {searchMode() === 'songs' ? filteredSongs().length : filteredAlbums().length}</p>
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
              </div>

              <Show when={searchMode() === 'songs'}>
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
                    <button class="pill-button subtle" type="button" onClick={() => setSelectedSongIds([])}>
                      clear selection
                    </button>
                  </div>
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
              </Show>
            </section>
          </div>
        </div>
      </Show>
    </section>
  )
}
