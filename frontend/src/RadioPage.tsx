import { createEffect, createMemo, createResource, createSignal, For, onCleanup, Show } from 'solid-js'
import { ListPlus, Pause, Play, SkipForward, Trash2, UploadCloud, Volume2 } from 'lucide-solid'
import { resolveAtprotoProfile, type AtprotoProfile } from './atproto'
import { extractAudioMetadata, type ExtractedAudioMetadata } from './audioMetadata'
import {
  API_BASE,
  controlRadio,
  enqueueAlbum,
  enqueueSong,
  fetchAlbums,
  fetchRadioSeek,
  fetchRadioSnapshot,
  fetchSongs,
  importFromSubsonic,
  loadSubsonicCreds,
  openRadioSocket,
  removeQueueItem,
  saveSubsonicCreds,
  searchSubsonic,
  uploadSong,
  uploadSongFromUrl,
  type RadioEvent,
  type SubsonicSongResult,
} from './radio'

interface RadioPageProps {
  isAdmin: boolean
}

function hasRequiredMetadata(metadata: ExtractedAudioMetadata | null): boolean {
  return Boolean(metadata?.title && metadata.artist)
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
  const [uploadError, setUploadError] = createSignal<string | null>(null)
  const [metadata, setMetadata] = createSignal<ExtractedAudioMetadata | null>(null)
  const [profiles, setProfiles] = createSignal<Record<string, AtprotoProfile>>({})
  const inFlightDids = new Set<string>()
  const [title, setTitle] = createSignal('')
  const [artist, setArtist] = createSignal('')
  const [file, setFile] = createSignal<File | null>(null)
  const [coverFile, setCoverFile] = createSignal<File | null>(null)
  const [addToQueue, setAddToQueue] = createSignal(true)
  const [uploadMode, setUploadMode] = createSignal<'file' | 'url' | 'subsonic'>('file')
  const [urlInput, setUrlInput] = createSignal('')
  const [urlTitle, setUrlTitle] = createSignal('')
  const [urlArtist, setUrlArtist] = createSignal('')
  const [urlAlbum, setUrlAlbum] = createSignal('')
  const [urlAddToQueue, setUrlAddToQueue] = createSignal(true)
  const savedCreds = loadSubsonicCreds()
  const [subsonicServerUrl, setSubsonicServerUrl] = createSignal(savedCreds.serverUrl ?? '')
  const [subsonicUsername, setSubsonicUsername] = createSignal(savedCreds.username ?? '')
  const [subsonicPassword, setSubsonicPassword] = createSignal(savedCreds.password ?? '')
  const [subsonicQuery, setSubsonicQuery] = createSignal('')
  const [subsonicResults, setSubsonicResults] = createSignal<SubsonicSongResult[]>([])
  const [subsonicSearching, setSubsonicSearching] = createSignal(false)
  const [subsonicAddToQueue, setSubsonicAddToQueue] = createSignal(true)
  const [importingId, setImportingId] = createSignal<string | null>(null)
  const [volume, setVolume] = createSignal(readVolumeCookie())
  const [isListening, setIsListening] = createSignal(false)
  const [clock, setClock] = createSignal(Date.now())
  const [serverSeekSeconds, setServerSeekSeconds] = createSignal(0)
  const [seekSyncedAt, setSeekSyncedAt] = createSignal(Date.now())
  const [songPage, setSongPage] = createSignal(0)
  const [upNextPage, setUpNextPage] = createSignal(0)
  const [queueControlPage, setQueueControlPage] = createSignal(0)
  const [songFilterArtist, setSongFilterArtist] = createSignal('')
  const [songFilterGenre, setSongFilterGenre] = createSignal('')
  const [songFilterDid, setSongFilterDid] = createSignal('')
  let audioRef: HTMLAudioElement | undefined
  let lastAudioSyncKey = ''

  const applyBackendSeek = (positionSeconds: number) => {
    setServerSeekSeconds(Math.max(0, positionSeconds))
    setSeekSyncedAt(Date.now())
  }

  const refreshSeekFromBackend = async () => {
    try {
      const seek = await fetchRadioSeek()
      applyBackendSeek(seek.positionSeconds)
    } catch {
      // Best-effort refresh: keep the last known seek until the next success.
    }
  }

  createEffect(() => {
    const interval = window.setInterval(() => setClock(Date.now()), 1000)
    onCleanup(() => window.clearInterval(interval))
  })

  createEffect(() => {
    const state = snapshot()?.state
    if (state) {
      applyBackendSeek(state.positionSeconds)
    }
  })

  createEffect(() => {
    void refreshSeekFromBackend()
  })

  createEffect(() => {
    const state = snapshot()?.state
    if (!state || state.status !== 'playing' || !state.currentSongId) {
      return
    }

    void refreshSeekFromBackend()
    const interval = window.setInterval(() => void refreshSeekFromBackend(), 1000)
    onCleanup(() => window.clearInterval(interval))
  })

  createEffect(() => {
    const socket = openRadioSocket()

    socket.addEventListener('message', (message) => {
      const event = JSON.parse(message.data) as RadioEvent
      if (event.type === 'snapshotChanged') {
        mutate(event.snapshot)
        void refetchSongs()
        void refetchAlbums()
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

  const currentSong = () => snapshot()?.currentSong
  const currentAudioUrl = () => (currentSong() ? `${API_BASE}/api/songs/${currentSong()?.id}/audio` : undefined)
  const nextAudioUrl = () => {
    const queue = snapshot()?.queue
    return queue && queue.length > 0 ? `${API_BASE}/api/songs/${queue[0].songId}/audio` : undefined
  }
  const livePositionSeconds = () => {
    // Re-evaluate every second for queue progress labels.
    void clock()

    // When we're actually listening, the audio element's currentTime is the
    // truth - this is what the listener hears.
    if (audioRef && !audioRef.paused && Number.isFinite(audioRef.currentTime)) {
      return audioRef.currentTime
    }

    const state = snapshot()?.state
    if (state?.status === 'playing') {
      const elapsed = (clock() - seekSyncedAt()) / 1000
      return serverSeekSeconds() + Math.max(0, elapsed)
    }

    return serverSeekSeconds()
  }
  const needsMetadataPrompt = () => file() && !hasRequiredMetadata(metadata())
  const profileFor = (did: string) => profiles()[did] ?? fallbackProfile(did)
  const clampSeekPosition = (positionSeconds: number, durationSeconds: number | null | undefined): number => {
    const maxSeek = durationSeconds ? Math.max(0, durationSeconds - 2) : Infinity
    return Math.min(Math.max(0, positionSeconds), maxSeek)
  }
  const songPageSize = 6
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
  const queuePageCount = createMemo(() => Math.max(1, Math.ceil((snapshot()?.queue.length ?? 0) / queuePageSize)))
  const songPageCount = createMemo(() => Math.max(1, Math.ceil(filteredSongs().length / songPageSize)))
  const pagedSongs = createMemo(() => {
    const start = songPage() * songPageSize
    return filteredSongs().slice(start, start + songPageSize)
  })
  const pagedUpNext = createMemo(() => {
    const start = upNextPage() * queuePageSize
    return (snapshot()?.queue ?? []).slice(start, start + queuePageSize)
  })
  const pagedQueueControl = createMemo(() => {
    const start = queueControlPage() * queuePageSize
    return (snapshot()?.queue ?? []).slice(start, start + queuePageSize)
  })

  const playCurrentAudio = () => {
    window.setTimeout(() => {
      if (audioRef) {
        audioRef.volume = volume()
        void audioRef.play().catch(() => undefined)
      }
    }, 0)
  }

  const startLocalPlayback = async () => {
    if (!audioRef || !snapshot()?.state) {
      return
    }

    audioRef.volume = volume()
    await refreshSeekFromBackend()
    const seekPosition = livePositionSeconds()

    audioRef.currentTime = clampSeekPosition(seekPosition, currentSong()?.durationSeconds)
    void audioRef.play().catch(() => undefined)
  }

  createEffect(() => {
    if (!audioRef) {
      return
    }

    const state = snapshot()?.state
    const song = currentSong()
    if (!state || !song) {
      audioRef.pause()
      return
    }

    // Only resync on real transitions (song change, play/pause/skip).
    const syncKey = `${song.id}:${state.status}:${state.startedAt ?? ''}`
    if (syncKey !== lastAudioSyncKey) {
      lastAudioSyncKey = syncKey
      audioRef.volume = volume()
      audioRef.load()
      // Cap at duration - 2s so seeking never triggers a spurious onEnded.
      audioRef.currentTime = clampSeekPosition(livePositionSeconds(), song.durationSeconds)
    }

    if (state.status === 'playing') {
      playCurrentAudio()
      return
    }

    audioRef.pause()
  })

  createEffect(() => {
    void songFilterArtist()
    void songFilterGenre()
    void songFilterDid()
    setSongPage(0)
  })

  createEffect(() => {
    if (songPage() >= songPageCount()) {
      setSongPage(songPageCount() - 1)
    }
    if (upNextPage() >= queuePageCount()) {
      setUpNextPage(queuePageCount() - 1)
    }
    if (queueControlPage() >= queuePageCount()) {
      setQueueControlPage(queuePageCount() - 1)
    }
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
    navigator.mediaSession.setActionHandler('play', () => startLocalPlayback())
    navigator.mediaSession.setActionHandler('pause', () => audioRef?.pause())
    navigator.mediaSession.setActionHandler('stop', () => audioRef?.pause())
  })

  createEffect(() => {
    if (!('mediaSession' in navigator)) return
    if (isListening()) {
      navigator.mediaSession.playbackState = 'playing'
    } else if (currentSong()) {
      navigator.mediaSession.playbackState = 'paused'
    } else {
      navigator.mediaSession.playbackState = 'none'
    }
  })

  const selectFile = async (selectedFile: File | null) => {
    setFile(selectedFile)
    setMetadata(null)
    setTitle('')
    setArtist('')
    setUploadError(null)

    if (!selectedFile) {
      return
    }

    try {
      const extracted = await extractAudioMetadata(selectedFile)
      setMetadata(extracted)
      setTitle(extracted.title ?? '')
      setArtist(extracted.artist ?? '')
    } catch {
      setMetadata({})
    }
  }

  const submitUpload = async (event: SubmitEvent) => {
    event.preventDefault()
    const selectedFile = file()

    if (!selectedFile) {
      setUploadError('pick an audio file first.')
      return
    }

    const resolvedTitle = metadata()?.title ?? title().trim()
    const resolvedArtist = metadata()?.artist ?? artist().trim()

    if (!resolvedTitle || !resolvedArtist) {
      setUploadError('this file is missing title or artist metadata.')
      return
    }

    try {
      setUploadError(null)
      await uploadSong({
        file: selectedFile,
        title: resolvedTitle,
        artist: resolvedArtist,
        album: metadata()?.album,
        genre: metadata()?.genre,
        durationSeconds: metadata()?.durationSeconds,
        cover: coverFile(),
        addToQueue: addToQueue(),
      })
      setTitle('')
      setArtist('')
      setMetadata(null)
      setFile(null)
      setCoverFile(null)
      await Promise.all([refetch(), refetchSongs()])
    } catch (error) {
      setUploadError(error instanceof Error ? error.message : 'upload exploded a little.')
    }
  }

  const isYtdlpUrl = (url: string) =>
    url.includes('youtube.com/') || url.includes('youtu.be/') ||
    url.includes('soundcloud.com/') || url.includes('bandcamp.com/') || url.includes('vimeo.com/')

  const submitUrlUpload = async (event: SubmitEvent) => {
    event.preventDefault()
    const url = urlInput().trim()
    const title = urlTitle().trim()
    const artist = urlArtist().trim()

    if (!url) { setUploadError('paste a url first.'); return }
    if (!isYtdlpUrl(url) && !title) { setUploadError('title is required.'); return }
    if (!isYtdlpUrl(url) && !artist) { setUploadError('artist is required.'); return }

    try {
      setUploadError(null)
      await uploadSongFromUrl({ url, title: title || undefined, artist: artist || undefined, album: urlAlbum().trim() || undefined, addToQueue: urlAddToQueue() })
      setUrlInput('')
      setUrlTitle('')
      setUrlArtist('')
      setUrlAlbum('')
      await Promise.all([refetch(), refetchSongs()])
    } catch (error) {
      setUploadError(error instanceof Error ? error.message : 'url import exploded a little.')
    }
  }

  createEffect(() => {
    const url = subsonicServerUrl()
    const user = subsonicUsername()
    const pass = subsonicPassword()
    saveSubsonicCreds({ serverUrl: url, username: user, password: pass })
  })

  createEffect(() => {
    const query = subsonicQuery()
    if (!query.trim() || uploadMode() !== 'subsonic') {
      setSubsonicResults([])
      return
    }
    const timer = setTimeout(() => {
      setSubsonicSearching(true)
      void searchSubsonic(
        { serverUrl: subsonicServerUrl(), username: subsonicUsername(), password: subsonicPassword() },
        query,
      )
        .then(setSubsonicResults)
        .catch(() => setSubsonicResults([]))
        .finally(() => setSubsonicSearching(false))
    }, 500)
    onCleanup(() => clearTimeout(timer))
  })

  const importSubsonicSong = async (result: SubsonicSongResult) => {
    setImportingId(result.id)
    try {
      setUploadError(null)
      await importFromSubsonic(
        { serverUrl: subsonicServerUrl(), username: subsonicUsername(), password: subsonicPassword() },
        result.id,
        result.coverArtId,
        subsonicAddToQueue(),
      )
      await Promise.all([refetch(), refetchSongs()])
    } catch (error) {
      setUploadError(error instanceof Error ? error.message : 'import failed.')
    } finally {
      setImportingId(null)
    }
  }

  const sendControl = async (action: 'play' | 'pause' | 'stop' | 'skip') => {
    try {
      setUploadError(null)
      mutate(await controlRadio(action, 'explicit_admin_action'))
      if (action === 'play' || action === 'skip') {
        playCurrentAudio()
      }
    } catch (error) {
      setUploadError(error instanceof Error ? error.message : 'radio control faceplanted.')
    }
  }

  const addSongToQueue = async (songId: string) => {
    try {
      setUploadError(null)
      await enqueueSong(songId)
      await refetch()
    } catch (error) {
      setUploadError(error instanceof Error ? error.message : 'queue add faceplanted.')
    }
  }

  const addAlbumToQueue = async (songIds: string[]) => {
    try {
      setUploadError(null)
      mutate(await enqueueAlbum(songIds))
    } catch (error) {
      setUploadError(error instanceof Error ? error.message : 'album queue add faceplanted.')
    }
  }

  const removeFromQueue = async (queueId: string) => {
    try {
      setUploadError(null)
      mutate(await removeQueueItem(queueId))
    } catch (error) {
      setUploadError(error instanceof Error ? error.message : 'queue remove faceplanted.')
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
          onPlay={() => setIsListening(true)}
          onPause={() => setIsListening(false)}
          onEnded={() => setIsListening(false)}
        />
        <Show when={nextAudioUrl()}>
          {(url) => <audio src={url()} preload="auto" aria-hidden="true" style="display:none" />}
        </Show>
        <div class="listener-controls">
          <Show when={currentSong() && snapshot()?.state.status === 'playing'}>
            <button class="listen-button" type="button" onClick={startLocalPlayback}>
              {isListening() ? 'listening live' : 'click to listen live'}
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
              <For each={pagedUpNext()} fallback={<li class="muted">queue is empty</li>}>
                {(item, index) => {
                  const profile = () => profileFor(item.addedByDid)
                  const hasCover = () => (songs() ?? []).some((song) => song.id === item.songId && song.hasCover)
                  return (
                    <li>
                      <span class="queue-number">{upNextPage() * queuePageSize + index() + 1}</span>
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
            <Show when={(snapshot()?.queue.length ?? 0) > queuePageSize}>
              <div class="pagination-row compact">
                <button class="pill-button subtle" type="button" disabled={upNextPage() === 0} onClick={() => setUpNextPage((page) => Math.max(0, page - 1))}>
                  prev
                </button>
                <span>{upNextPage() + 1} / {queuePageCount()}</span>
                <button class="pill-button subtle" type="button" disabled={upNextPage() >= queuePageCount() - 1} onClick={() => setUpNextPage((page) => Math.min(queuePageCount() - 1, page + 1))}>
                  next
                </button>
              </div>
            </Show>
          </Show>
        </section>

        <Show when={props.isAdmin}>
          <section class="glass-card admin-controls">
            <div class="section-heading">
              <p class="eyebrow">admin control</p>
              <div class="transport-controls">
                <button class="icon-button primary" type="button" aria-label="play" onClick={() => void sendControl('play')}>
                  <Play size={20} fill="currentColor" />
                </button>
                <button class="icon-button" type="button" aria-label="pause" onClick={() => void sendControl('pause')}>
                  <Pause size={18} />
                </button>
                <button class="icon-button" type="button" aria-label="skip" onClick={() => void sendControl('skip')}>
                  <SkipForward size={18} />
                </button>
              </div>
            </div>

            <div class="upload-mode-tabs">
              <button class="pill-button" classList={{ subtle: uploadMode() !== 'file' }} type="button" onClick={() => setUploadMode('file')}>file</button>
              <button class="pill-button" classList={{ subtle: uploadMode() !== 'url' }} type="button" onClick={() => setUploadMode('url')}>url</button>
              <button class="pill-button" classList={{ subtle: uploadMode() !== 'subsonic' }} type="button" onClick={() => setUploadMode('subsonic')}>subsonic</button>
            </div>

            <Show when={uploadMode() === 'file'}>
              <form class="upload-form" onSubmit={submitUpload}>
                <label class="drop-zone">
                  <UploadCloud size={24} />
                  <span>{file()?.name ?? 'choose an audio file'}</span>
                  <input type="file" accept="audio/*" onChange={(event) => void selectFile(event.currentTarget.files?.[0] ?? null)} />
                </label>

                <div class="upload-options-row">
                  <label class="inline-file cover-picker">
                    <span>cover image</span>
                    <span class="file-button">choose cover</span>
                    <input type="file" accept="image/*" onChange={(event) => setCoverFile(event.currentTarget.files?.[0] ?? null)} />
                    <small>{coverFile()?.name ?? 'no cover selected'}</small>
                  </label>

                  <label class="inline-check">
                    <input type="checkbox" checked={addToQueue()} onChange={(event) => setAddToQueue(event.currentTarget.checked)} />
                    add to queue
                  </label>
                </div>

                <Show when={needsMetadataPrompt()}>
                  <div class="metadata-prompt">
                    <p class="muted">no title/artist tags found. add the minimum so the queue is readable.</p>
                    <input placeholder="title" value={title()} onInput={(event) => setTitle(event.currentTarget.value)} />
                    <input placeholder="artist" value={artist()} onInput={(event) => setArtist(event.currentTarget.value)} />
                  </div>
                </Show>

                <button class="pill-button" type="submit">upload</button>
              </form>
            </Show>

            <Show when={uploadMode() === 'url'}>
              <form class="upload-form" onSubmit={submitUrlUpload}>
                <input
                  type="url"
                  placeholder="https://example.com/song.mp3 or youtube.com/watch?v=..."
                  value={urlInput()}
                  onInput={(e) => setUrlInput(e.currentTarget.value)}
                />
                <Show when={isYtdlpUrl(urlInput())}>
                  <p class="subsonic-searching">youtube · title and artist auto-detected, or fill in below to override</p>
                </Show>
                <input placeholder={isYtdlpUrl(urlInput()) ? 'title (optional, auto-detected)' : 'title'} value={urlTitle()} onInput={(e) => setUrlTitle(e.currentTarget.value)} />
                <input placeholder={isYtdlpUrl(urlInput()) ? 'artist (optional, auto-detected)' : 'artist'} value={urlArtist()} onInput={(e) => setUrlArtist(e.currentTarget.value)} />
                <input placeholder="album (optional)" value={urlAlbum()} onInput={(e) => setUrlAlbum(e.currentTarget.value)} />
                <label class="inline-check">
                  <input type="checkbox" checked={urlAddToQueue()} onChange={(e) => setUrlAddToQueue(e.currentTarget.checked)} />
                  add to queue
                </label>
                <button class="pill-button" type="submit">import</button>
              </form>
            </Show>

            <Show when={uploadMode() === 'subsonic'}>
              <div class="upload-form">
                <input
                  type="url"
                  placeholder="server url"
                  value={subsonicServerUrl()}
                  onInput={(e) => setSubsonicServerUrl(e.currentTarget.value)}
                />
                <input
                  placeholder="username"
                  value={subsonicUsername()}
                  onInput={(e) => setSubsonicUsername(e.currentTarget.value)}
                />
                <input
                  type="password"
                  placeholder="password"
                  value={subsonicPassword()}
                  onInput={(e) => setSubsonicPassword(e.currentTarget.value)}
                />
                <hr class="subsonic-divider" />
                <input
                  placeholder="search songs..."
                  value={subsonicQuery()}
                  onInput={(e) => setSubsonicQuery(e.currentTarget.value)}
                />
                <label class="inline-check">
                  <input type="checkbox" checked={subsonicAddToQueue()} onChange={(e) => setSubsonicAddToQueue(e.currentTarget.checked)} />
                  add to queue
                </label>
                <Show when={subsonicSearching()}>
                  <p class="subsonic-searching">searching...</p>
                </Show>
                <Show when={subsonicResults().length > 0}>
                  <div class="subsonic-results">
                    <ul class="song-list">
                      <For each={subsonicResults()}>
                        {(result) => (
                          <li>
                            <div class="song-copy">
                              <span>{result.title}</span>
                              <small>{result.artist}{result.album ? ` · ${result.album}` : ''}</small>
                            </div>
                            <button
                              class="pill-button subtle"
                              type="button"
                              disabled={importingId() === result.id}
                              onClick={() => void importSubsonicSong(result)}
                            >
                              {importingId() === result.id ? '...' : 'import'}
                            </button>
                          </li>
                        )}
                      </For>
                    </ul>
                  </div>
                </Show>
              </div>
            </Show>

            <Show when={uploadError()}>{(message) => <p class="error-copy">{message()}</p>}</Show>
          </section>

          <section class="glass-card">
            <div class="section-heading">
              <p class="eyebrow">queue control</p>
              <span>{snapshot()?.queue.length ?? 0}</span>
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
              <For each={pagedQueueControl()} fallback={<li class="list-empty">queue is empty</li>}>
                {(item) => {
                  const profile = () => profileFor(item.addedByDid)
                  return (
                    <li>
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
            <Show when={(snapshot()?.queue.length ?? 0) > queuePageSize}>
              <div class="pagination-row compact">
                <button class="pill-button subtle" type="button" disabled={queueControlPage() === 0} onClick={() => setQueueControlPage((page) => Math.max(0, page - 1))}>
                  prev
                </button>
                <span>{queueControlPage() + 1} / {queuePageCount()}</span>
                <button class="pill-button subtle" type="button" disabled={queueControlPage() >= queuePageCount() - 1} onClick={() => setQueueControlPage((page) => Math.min(queuePageCount() - 1, page + 1))}>
                  next
                </button>
              </div>
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
            <Show when={!songs.loading} fallback={<p class="list-empty">loading songs...</p>}>
              <ul class="song-list">
                <For each={pagedSongs()} fallback={<li class="list-empty">no songs added yet</li>}>
                  {(song) => {
                    const profile = () => profileFor(song.addedByDid)
                    return (
                      <li>
                        <ProfileAvatar profile={profile()} />
                        <div class="song-copy">
                          <span>{song.title}</span>
                          <small>{song.artist}</small>
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
              <Show when={(songs()?.length ?? 0) > songPageSize}>
                <div class="pagination-row">
                  <button class="pill-button subtle" type="button" disabled={songPage() === 0} onClick={() => setSongPage((page) => Math.max(0, page - 1))}>
                    prev
                  </button>
                  <span>{songPage() + 1} / {songPageCount()}</span>
                  <button class="pill-button subtle" type="button" disabled={songPage() >= songPageCount() - 1} onClick={() => setSongPage((page) => Math.min(songPageCount() - 1, page + 1))}>
                    next
                  </button>
                </div>
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
                <For each={albums() ?? []} fallback={<li class="list-empty">no album loops yet</li>}>
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
            </Show>
          </section>
        </section>
      </Show>
    </section>
  )
}

function SongCoverThumb(props: { songId: string; hasCover: boolean }) {
  return (
    <span class="song-cover-thumb" aria-hidden="true">
      <Show when={props.hasCover} fallback={<span class="song-cover-fallback">art</span>}>
        <img src={`${API_BASE}/api/songs/${props.songId}/cover`} alt="" loading="lazy" />
      </Show>
    </span>
  )
}

function ProfileAvatar(props: { profile: AtprotoProfile; class?: string; title?: string }) {
  return (
    <span class={`profile-avatar${props.class ? ` ${props.class}` : ''}`} title={props.title}>
      <Show when={props.profile.avatar} fallback={props.profile.handle.slice(0, 1).toUpperCase()}>
        {(avatar) => <img src={avatar()} alt="" />}
      </Show>
    </span>
  )
}
