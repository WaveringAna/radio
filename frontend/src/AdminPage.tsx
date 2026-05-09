import { createMemo, createResource, createSignal, For, Show } from 'solid-js'
import { Plus, Trash2, UploadCloud } from 'lucide-solid'
import { addAdminDid, fetchAdminPermissions, removeAdminDid } from './auth'
import { extractAudioMetadata, type ExtractedAudioMetadata } from './audioMetadata'
import { createAlbum, createAlbumFromMetadata, deleteAlbum, deleteSong, fetchAlbums, fetchSongs, setAlbumEnabled, uploadSong, uploadSongCover } from './radio'

interface AdminPageProps {
  accountDid: string
  isAdmin: boolean
}

function readableError(error: unknown): string {
  return error instanceof Error ? error.message : 'admin panel had a tiny meltdown.'
}

function titleFromFilename(file: File): string {
  return file.name.replace(/\.[^/.]+$/, '').replace(/^\d+\s*[-_. ]\s*/, '').trim() || file.name
}

/**
 * Renders admin whitelist and music management tools.
 * @param props Admin page auth context.
 * @returns The admin page view.
 */
export default function AdminPage(props: AdminPageProps) {
  const [newDid, setNewDid] = createSignal('')
  const [localError, setLocalError] = createSignal<string | null>(null)
  const [albumTitle, setAlbumTitle] = createSignal('')
  const [selectedSongIds, setSelectedSongIds] = createSignal<string[]>([])
  const [newAlbumTitle, setNewAlbumTitle] = createSignal('')
  const [newAlbumArtist, setNewAlbumArtist] = createSignal('')
  const [newAlbumFiles, setNewAlbumFiles] = createSignal<File[]>([])
  const [newAlbumCover, setNewAlbumCover] = createSignal<File | null>(null)
  const [albumUploadStatus, setAlbumUploadStatus] = createSignal<string | null>(null)
  const [adminPermissions, { mutate: mutateAdmins }] = createResource(
    () => props.isAdmin,
    (enabled) => (enabled ? fetchAdminPermissions() : undefined),
  )
  const [songs, { refetch: refetchSongs }] = createResource(() => (props.isAdmin ? fetchSongs() : undefined))
  const [albums, { mutate: mutateAlbums, refetch: refetchAlbums }] = createResource(() => (props.isAdmin ? fetchAlbums() : undefined))
  const metadataAlbums = createMemo(() => {
    const groups = new Map<string, number>()
    for (const song of songs() ?? []) {
      if (song.album) {
        groups.set(song.album, (groups.get(song.album) ?? 0) + 1)
      }
    }
    return [...groups.entries()].map(([title, count]) => ({ title, count }))
  })

  const addDid = async (event: SubmitEvent) => {
    event.preventDefault()
    const did = newDid().trim()
    if (!did) {
      return
    }

    try {
      setLocalError(null)
      mutateAdmins(await addAdminDid(did))
      setNewDid('')
    } catch (error) {
      setLocalError(readableError(error))
    }
  }

  const removeDid = async (did: string) => {
    try {
      setLocalError(null)
      mutateAdmins(await removeAdminDid(did))
    } catch (error) {
      setLocalError(readableError(error))
    }
  }

  const removeSong = async (songId: string) => {
    try {
      setLocalError(null)
      await deleteSong(songId)
      await refetchSongs()
    } catch (error) {
      setLocalError(readableError(error))
    }
  }

  const toggleSelectedSong = (songId: string, checked: boolean) => {
    setSelectedSongIds((current) => (checked ? [...current, songId] : current.filter((id) => id !== songId)))
  }

  const uploadAlbum = async (event: SubmitEvent) => {
    event.preventDefault()
    const files = newAlbumFiles()
    if (files.length === 0) {
      setLocalError('pick album audio files first.')
      return
    }

    try {
      setLocalError(null)
      const uploadedSongIds: string[] = []
      for (const [index, file] of files.entries()) {
        setAlbumUploadStatus(`uploading ${index + 1}/${files.length}: ${file.name}`)
        const metadata: ExtractedAudioMetadata = await extractAudioMetadata(file).catch(() => ({}))
        const album = metadata.album ?? newAlbumTitle().trim()
        const artist = metadata.artist ?? newAlbumArtist().trim()
        if (!album || !artist) {
          throw new Error('album upload needs album title and artist when files do not have tags')
        }
        const song = await uploadSong({
          file,
          title: metadata.title ?? titleFromFilename(file),
          artist,
          album,
          durationSeconds: metadata.durationSeconds,
          cover: newAlbumCover(),
          addToQueue: false,
        })
        uploadedSongIds.push(song.id)
      }

      const loopTitle = newAlbumTitle().trim() || 'album upload'
      await createAlbum({ title: loopTitle, songIds: uploadedSongIds })
      setNewAlbumTitle('')
      setNewAlbumArtist('')
      setNewAlbumFiles([])
      setNewAlbumCover(null)
      setAlbumUploadStatus(null)
      await Promise.all([refetchSongs(), refetchAlbums()])
    } catch (error) {
      setAlbumUploadStatus(null)
      setLocalError(readableError(error))
    }
  }

  const addAlbum = async (event: SubmitEvent) => {
    event.preventDefault()
    try {
      setLocalError(null)
      await createAlbum({ title: albumTitle(), songIds: selectedSongIds() })
      setAlbumTitle('')
      setSelectedSongIds([])
      await refetchAlbums()
    } catch (error) {
      setLocalError(readableError(error))
    }
  }

  const massAddAlbum = async (title: string) => {
    try {
      setLocalError(null)
      await createAlbumFromMetadata(title)
      await refetchAlbums()
    } catch (error) {
      setLocalError(readableError(error))
    }
  }

  const removeAlbum = async (albumId: string) => {
    try {
      setLocalError(null)
      mutateAlbums(await deleteAlbum(albumId))
    } catch (error) {
      setLocalError(readableError(error))
    }
  }

  const toggleAlbum = async (albumId: string, enabled: boolean) => {
    try {
      setLocalError(null)
      mutateAlbums(await setAlbumEnabled(albumId, enabled))
    } catch (error) {
      setLocalError(readableError(error))
    }
  }

  const replaceCover = async (songId: string, file: File | null) => {
    if (!file) {
      return
    }

    try {
      setLocalError(null)
      await uploadSongCover(songId, file)
      await refetchSongs()
    } catch (error) {
      setLocalError(readableError(error))
    }
  }

  return (
    <section class="admin-page">
      <p>signed in as: {props.accountDid}</p>
      <Show when={props.isAdmin} fallback={<p>this did is not on the admin whitelist.</p>}>
        <Show when={!adminPermissions.loading} fallback={<p>loading admin permissions...</p>}>
          <Show when={!adminPermissions.error} fallback={<p>{readableError(adminPermissions.error)}</p>}>
            <section class="glass-card admin-card">
              <div class="section-heading">
                <p class="eyebrow">whitelisted admin dids</p>
                <span>{adminPermissions()?.whitelistedDids.length ?? 0}</span>
              </div>
              <form class="admin-inline-form" onSubmit={addDid}>
                <input placeholder="did:plc:..." value={newDid()} onInput={(event) => setNewDid(event.currentTarget.value)} />
                <button class="icon-button" type="submit" aria-label="add admin did">
                  <Plus size={18} />
                </button>
              </form>
              <ul class="song-list">
                <For each={adminPermissions()?.whitelistedDids ?? []}>
                  {(did) => (
                    <li>
                      <div class="song-copy">
                        <span>{did}</span>
                      </div>
                      <button class="icon-button" type="button" aria-label="remove admin did" onClick={() => void removeDid(did)}>
                        <Trash2 size={17} />
                      </button>
                    </li>
                  )}
                </For>
              </ul>
            </section>

            <section class="glass-card admin-card">
              <div class="section-heading">
                <p class="eyebrow">add new album</p>
                <span>{newAlbumFiles().length}</span>
              </div>
              <form class="album-builder" onSubmit={uploadAlbum}>
                <input placeholder="album title fallback" value={newAlbumTitle()} onInput={(event) => setNewAlbumTitle(event.currentTarget.value)} />
                <input placeholder="artist fallback" value={newAlbumArtist()} onInput={(event) => setNewAlbumArtist(event.currentTarget.value)} />
                <label class="inline-file cover-picker album-file-picker">
                  <span>audio files</span>
                  <span class="file-button">choose files</span>
                  <input type="file" accept="audio/*" multiple onChange={(event) => setNewAlbumFiles([...event.currentTarget.files ?? []])} />
                  <small>{newAlbumFiles().length ? `${newAlbumFiles().length} files selected` : 'no files selected'}</small>
                </label>
                <label class="inline-file cover-picker album-file-picker">
                  <span>cover</span>
                  <span class="file-button">choose cover</span>
                  <input type="file" accept="image/*" onChange={(event) => setNewAlbumCover(event.currentTarget.files?.[0] ?? null)} />
                  <small>{newAlbumCover()?.name ?? 'no cover selected'}</small>
                </label>
                <button class="pill-button" type="submit">upload album + create loop</button>
                <Show when={albumUploadStatus()}>{(status) => <small class="muted">{status()}</small>}</Show>
              </form>
            </section>

            <section class="glass-card admin-card">
              <div class="section-heading">
                <p class="eyebrow">album loop</p>
                <span>{albums()?.length ?? 0}</span>
              </div>
              <form class="album-builder" onSubmit={addAlbum}>
                <input placeholder="album loop title" value={albumTitle()} onInput={(event) => setAlbumTitle(event.currentTarget.value)} />
                <div class="album-song-picker">
                  <For each={songs() ?? []} fallback={<span class="muted">upload songs first</span>}>
                    {(song) => (
                      <label>
                        <input
                          type="checkbox"
                          checked={selectedSongIds().includes(song.id)}
                          onChange={(event) => toggleSelectedSong(song.id, event.currentTarget.checked)}
                        />
                        <span>{song.title}</span>
                      </label>
                    )}
                  </For>
                </div>
                <button class="pill-button" type="submit">create loop album</button>
              </form>
              <div class="mass-add-row">
                <For each={metadataAlbums()}>
                  {(album) => (
                    <button class="pill-button subtle" type="button" onClick={() => void massAddAlbum(album.title)}>
                      mass add {album.title} ({album.count})
                    </button>
                  )}
                </For>
              </div>
              <Show when={!albums.loading} fallback={<p class="list-empty">loading albums...</p>}>
                <ul class="song-list album-loop-list">
                  <For each={albums() ?? []} fallback={<li class="list-empty">no album loops yet</li>}>
                    {(album) => (
                      <li>
                        <label class="tiny-check">
                          <input type="checkbox" checked={album.isEnabled} onChange={(event) => void toggleAlbum(album.id, event.currentTarget.checked)} />
                        </label>
                        <div class="song-copy">
                          <span>{album.title}</span>
                          <small>{album.tracks.length} tracks · {album.tracks.map((track) => track.title).join(' → ')}</small>
                        </div>
                        <button class="icon-button" type="button" aria-label="delete album loop" onClick={() => void removeAlbum(album.id)}>
                          <Trash2 size={17} />
                        </button>
                      </li>
                    )}
                  </For>
                </ul>
              </Show>
            </section>

            <section class="glass-card admin-card">
              <div class="section-heading">
                <p class="eyebrow">music library</p>
                <span>{songs()?.length ?? 0}</span>
              </div>
              <Show when={!songs.loading} fallback={<p class="list-empty">loading songs...</p>}>
                <ul class="song-list admin-song-list">
                  <For each={songs() ?? []} fallback={<li class="list-empty">no songs yet</li>}>
                    {(song) => (
                      <li>
                        <Show when={song.hasCover} fallback={<span class="cover-thumb" />}>
                          <img class="cover-thumb" src={`/api/songs/${song.id}/cover`} alt="" />
                        </Show>
                        <div class="song-copy">
                          <span>{song.title}</span>
                          <small>{song.artist}{song.album ? ` · ${song.album}` : ''}</small>
                        </div>
                        <label class="icon-button cover-upload" aria-label="replace cover">
                          <UploadCloud size={17} />
                          <input type="file" accept="image/*" onChange={(event) => void replaceCover(song.id, event.currentTarget.files?.[0] ?? null)} />
                        </label>
                        <button class="icon-button" type="button" aria-label="delete song" onClick={() => void removeSong(song.id)}>
                          <Trash2 size={17} />
                        </button>
                      </li>
                    )}
                  </For>
                </ul>
              </Show>
            </section>
          </Show>
        </Show>
      </Show>
      <Show when={localError()}>{(message) => <p class="error-copy">{message()}</p>}</Show>
    </section>
  )
}
