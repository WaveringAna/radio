import { createMemo, createSignal, Show } from 'solid-js'
import { ListChecks, Plus, Search, X } from 'lucide-solid'
import { AdminUploadPanel } from '../../features/upload/AdminUploadPanel'
import { AlbumsTab } from './AlbumsTab'
import { ArtistsTab } from './ArtistsTab'
import { buildArtistIndex } from './artists'
import { SelectionTray } from './SelectionTray'
import { SetsTab } from './SetsTab'
import { SongsTab } from './SongsTab'
import type { QueueControl, SearchMode } from './useQueueControl'

const PAGE_SIZE = 8

/**
 * The library column: one search box over songs, albums, and sets, plus the
 * intake panel. Selection state lives here so it survives switching tabs.
 */
export function LibraryPanel(props: { control: QueueControl }) {
  const control = props.control
  const [libraryQuery, setLibraryQuery] = createSignal('')
  const [searchMode, setSearchMode] = createSignal<SearchMode>('songs')
  const [showIntake, setShowIntake] = createSignal(false)
  const [selectedSongIds, setSelectedSongIds] = createSignal<string[]>([])
  const [selectMode, setSelectMode] = createSignal(false)

  // One query filters whichever tab is open. A song/album/set matches when
  // every word of the query appears in some field — so "beatles help" finds
  // the song, but searching just an artist still lists their whole catalog.
  // Haystacks are lowercased once per data change, not per keystroke.
  const queryWords = createMemo(() => libraryQuery().trim().toLowerCase().split(/\s+/).filter(Boolean))
  const matchesQuery = (haystack: string) => queryWords().every((word) => haystack.includes(word))

  const songIndex = createMemo(() => (control.songs() ?? []).map((song) => ({
    song,
    haystack: `${song.title} ${song.artist} ${song.album ?? ''} ${song.genre ?? ''}`.toLowerCase(),
  })))
  const albumIndex = createMemo(() => (control.albums() ?? []).map((album) => ({
    album,
    haystack: `${album.title} ${album.tracks.map((track) => `${track.title} ${track.artist}`).join(' ')}`.toLowerCase(),
  })))
  const artistIndex = createMemo(() => buildArtistIndex(control.songs() ?? [], control.albums() ?? []))
  const playlistIndex = createMemo(() => (control.playlists() ?? []).map((playlist) => ({
    playlist,
    haystack: `${playlist.name} ${playlist.tracks.map((track) => `${track.title} ${track.artist}`).join(' ')}`.toLowerCase(),
  })))

  const filteredSongs = createMemo(() => songIndex().filter((entry) => matchesQuery(entry.haystack)).map((entry) => entry.song))
  const filteredAlbums = createMemo(() => albumIndex().filter((entry) => matchesQuery(entry.haystack)).map((entry) => entry.album))
  const filteredArtists = createMemo(() => artistIndex().filter((entry) => matchesQuery(entry.haystack)))
  const filteredPlaylists = createMemo(() => playlistIndex().filter((entry) => matchesQuery(entry.haystack)).map((entry) => entry.playlist))

  const toggleSongSelection = (songId: string, checked: boolean) => {
    if (checked) setSelectMode(true)
    setSelectedSongIds((current) => (
      checked ? [...new Set([...current, songId])] : current.filter((id) => id !== songId)
    ))
  }

  const exitSelectMode = () => {
    setSelectMode(false)
    setSelectedSongIds([])
  }

  const selectMany = (songIds: string[]) => {
    setSelectMode(true)
    setSelectedSongIds((current) => [...new Set([...current, ...songIds])])
  }

  /** Checking a grouping row (album, artist) selects or clears all its tracks. */
  const toggleMany = (songIds: string[], checked: boolean) => {
    if (checked) {
      selectMany(songIds)
      return
    }
    const dropping = new Set(songIds)
    setSelectedSongIds((current) => current.filter((id) => !dropping.has(id)))
  }

  const queueSelection = async (songIds: string[], atTop: boolean, sequence = false) => {
    if (songIds.length === 0) return
    if (await control.addAlbumToQueue(songIds, atTop, sequence)) setSelectedSongIds([])
  }

  const saveSelectionAsSet = async (name: string) => {
    const saved = await control.saveAsPlaylist(name, selectedSongIds())
    if (saved) setSelectedSongIds([])
    return saved
  }

  const addSelectionToSet = async (playlistId: string, songIds: string[]) => {
    const added = await control.appendToPlaylist(playlistId, songIds)
    if (added) setSelectedSongIds([])
    return added
  }

  return (
    <div class="qc-column-left">
      <div class="qc-column-header">
        <div class="qc-column-title-group">
          <p class="qc-column-eyebrow">Music library</p>
          <h2>Find the next track</h2>
          <span class="qc-column-stats">
            {filteredSongs().length} songs • {control.albums()?.length || 0} albums
          </span>
        </div>
        {/* Both of these switch the panel's mode; the tab strip below is
            purely filters, so they belong up here instead of among them. */}
        <div class="qc-column-header-actions">
          <button
            class="qc-header-btn"
            type="button"
            classList={{ active: selectMode() }}
            aria-pressed={selectMode()}
            title="pick several tracks at once"
            onClick={() => (selectMode() ? exitSelectMode() : setSelectMode(true))}
          >
            <ListChecks size={15} />
            {selectMode() ? 'Done' : 'Select'}
          </button>
          <button
            class="qc-header-btn"
            type="button"
            classList={{ active: showIntake() }}
            aria-pressed={showIntake()}
            title="upload files, add from a URL, or import from Subsonic"
            onClick={() => setShowIntake(!showIntake())}
          >
            <Plus size={15} />
            {showIntake() ? 'Close' : 'Add music'}
          </button>
        </div>
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
          classList={{ active: searchMode() === 'artists' }}
          onClick={() => setSearchMode('artists')}
        >
          Artists{queryWords().length > 0 ? ` (${filteredArtists().length})` : ''}
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
      </div>

      <SelectionTray
        selectedSongIds={selectedSongIds}
        playlists={() => control.playlists() ?? []}
        onQueue={(songIds, atTop, sequence) => void queueSelection(songIds, atTop, sequence)}
        onSaveAsSet={saveSelectionAsSet}
        onAddToSet={addSelectionToSet}
        onClear={exitSelectMode}
      />

      <Show when={showIntake()}>
        <div class="qc-intake-wrapper">
          <AdminUploadPanel
            target={control.selectedRadioTarget()}
            onSongAdded={() => void control.refreshLibrary()}
            onError={control.setPageError}
          />
        </div>
      </Show>

      <Show when={searchMode() === 'songs'}>
        <SongsTab
          control={control}
          songs={filteredSongs}
          pageSize={PAGE_SIZE}
          resetPageOn={libraryQuery}
          selectedSongIds={selectedSongIds}
          selectMode={selectMode}
          onToggleSelection={toggleSongSelection}
        />
      </Show>

      <Show when={searchMode() === 'artists'}>
        <ArtistsTab
          control={control}
          artists={filteredArtists}
          pageSize={PAGE_SIZE}
          resetPageOn={libraryQuery}
          selectedSongIds={selectedSongIds}
          selectMode={selectMode}
          onToggleSelection={toggleSongSelection}
          onSelectMany={selectMany}
          onToggleMany={toggleMany}
        />
      </Show>

      <Show when={searchMode() === 'albums'}>
        <AlbumsTab
          control={control}
          albums={filteredAlbums}
          pageSize={PAGE_SIZE}
          resetPageOn={libraryQuery}
          selectedSongIds={selectedSongIds}
          selectMode={selectMode}
          onToggleSelection={toggleSongSelection}
          onSelectMany={selectMany}
          onToggleMany={toggleMany}
        />
      </Show>

      <Show when={searchMode() === 'playlists'}>
        <SetsTab
          control={control}
          playlists={filteredPlaylists}
          pageSize={PAGE_SIZE}
          resetPageOn={libraryQuery}
          selectedSongIds={selectedSongIds}
          selectMode={selectMode}
          onToggleSelection={toggleSongSelection}
        />
      </Show>
    </div>
  )
}
