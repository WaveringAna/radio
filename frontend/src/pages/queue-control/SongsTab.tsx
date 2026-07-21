import { createEffect, createSignal, For, Show, type Accessor } from 'solid-js'
import { Pencil, UploadCloud, X } from 'lucide-solid'
import { PaginationRow } from '../../shared/components/PaginationRow'
import { SearchableDropdown } from '../../shared/components/SearchableDropdown'
import { createPagedList } from '../../shared/primitives/createPagedList'
import type { Song } from '../../shared/lib/radio'
import { LibraryRow } from './LibraryRow'
import { queueMenuItems } from './queueActions'
import { formatTime } from './helpers'
import type { QueueControl } from './useQueueControl'

interface SongsTabProps {
  control: QueueControl
  songs: Accessor<Song[]>
  pageSize: number
  /** Reading this resets to page one — wired to the search query. */
  resetPageOn: Accessor<unknown>
  selectedSongIds: Accessor<string[]>
  selectMode: Accessor<boolean>
  onToggleSelection: (songId: string, checked: boolean) => void
}

/** The song list: multi-select, the genre-station shortcut, and inline metadata editing. */
export function SongsTab(props: SongsTabProps) {
  const control = props.control
  const paging = createPagedList(props.songs, props.pageSize)
  createEffect(() => {
    props.resetPageOn()
    paging.setPage(0)
  })

  const [editingSongId, setEditingSongId] = createSignal<string | null>(null)
  const [editTitle, setEditTitle] = createSignal('')
  const [editArtist, setEditArtist] = createSignal('')
  const [editAlbum, setEditAlbum] = createSignal('')
  const [editGenre, setEditGenre] = createSignal('')
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
    const ok = await control.saveSongMetadata(songId, {
      title: editTitle(),
      artist: editArtist(),
      album: editAlbum() || null,
      genre: editGenre() || null,
    })
    if (ok) cancelSongEdit()
  }

  return (
    <>
      <div class="qc-genre-bar">
        <span class="qc-genre-bar-label">genre station</span>
        <div class="qc-genre-picker">
          <SearchableDropdown
            options={control.genres()}
            counts={control.genreCounts()}
            placeholder="pick a genre..."
            value={control.selectedGenre()}
            onSelect={(val) => control.setSelectedGenre(val)}
          />
        </div>
        <Show when={control.selectedGenre()}>
          <div class="qc-genre-actions">
            <button
              class="pill-button subtle"
              type="button"
              disabled={control.selectedGenreCount() === 0}
              onClick={() => void control.shuffleLibraryByGenre(control.selectedGenre(), false)}
            >
              add {control.selectedGenreCount()} to queue
            </button>
            <button
              class="pill-button subtle"
              type="button"
              disabled={control.selectedGenreCount() === 0}
              title="clears the current queue first"
              onClick={() => {
                const queued = control.snapshot()?.queue.length ?? 0
                if (queued === 0 || confirm(`Replace the current queue (${queued} songs) with ${control.selectedGenreCount()} shuffled ${control.selectedGenre()} songs?`)) {
                  void control.shuffleLibraryByGenre(control.selectedGenre(), true)
                }
              }}
            >
              replace queue
            </button>
            <button class="pill-button subtle qc-genre-clear" type="button" aria-label="clear genre" onClick={() => control.setSelectedGenre('')}>
              <X size={15} />
            </button>
          </div>
        </Show>
        <Show when={control.genreNotice()}>
          <span class="qc-genre-notice" role="status">{control.genreNotice()}</span>
        </Show>
      </div>
      <Show when={!control.songs.loading} fallback={<p class="list-empty">loading songs...</p>}>
        <ul class="qc-songs-list">
          <For each={paging.paged()} fallback={<li class="list-empty">no songs match</li>}>
            {(song) => {
              const isEditing = () => editingSongId() === song.id
              return (
                <LibraryRow
                  coverSrc={song.hasCover ? control.coverUrl(song) : undefined}
                  placeholderFrom={song.title}
                  title={song.title}
                  meta={`${song.artist} \u2022 ${song.album || 'Single'}`}
                  badges={
                    <Show when={control.benchedSongIds().has(song.id)}>
                      <span class="qc-album-badge benched" title="every album this song belongs to is out of rotation, so it never autoplays">benched</span>
                    </Show>
                  }
                  detail={formatTime(song.durationSeconds)}
                  dimmed={control.queuedCountFor(song.id) > 0}
                  selectMode={props.selectMode()}
                  selected={props.selectedSongIds().includes(song.id)}
                  onToggleSelect={(checked) => props.onToggleSelection(song.id, checked)}
                  expanded={isEditing()}
                  menuItems={() => [
                    ...queueMenuItems(control, () => [song.id], {
                      queuedCount: () => control.queuedCountFor(song.id),
                      onSelect: () => props.onToggleSelection(song.id, true),
                    }),
                    {
                      label: isEditing() ? 'Close editor' : 'Edit details',
                      icon: <Pencil size={16} />,
                      onSelect: () => (isEditing() ? cancelSongEdit() : beginSongEdit(song)),
                    },
                  ]}
                >
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
                        <input type="file" accept="image/*" onChange={(event) => void control.replaceCover(song.id, event.currentTarget.files?.[0] ?? null)} />
                      </label>
                      <button class="pill-button" type="submit">save</button>
                      <button class="pill-button subtle" type="button" onClick={cancelSongEdit}>cancel</button>
                      <button
                        class="pill-button subtle danger-button"
                        type="button"
                        onClick={() => {
                          if (confirm(`Delete "${song.title}" from the library? This can't be undone.`)) {
                            void control.removeSong(song.id)
                          }
                        }}
                      >delete</button>
                    </div>
                  </form>
                </LibraryRow>
              )
            }}
          </For>
        </ul>
        <Show when={paging.pageCount() > 1}>
          <PaginationRow page={paging.page()} pageCount={paging.pageCount()} onPageChange={paging.setPage} />
        </Show>
      </Show>
    </>
  )
}
