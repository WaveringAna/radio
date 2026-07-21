import { createEffect, createSignal, For, Show, type Accessor } from 'solid-js'
import { ChevronDown, Radio } from 'lucide-solid'
import { PaginationRow } from '../../shared/components/PaginationRow'
import { createPagedList } from '../../shared/primitives/createPagedList'
import { songCoverThumbnailUrl } from '../../shared/lib/radio'
import { ActionMenu } from './ActionMenu'
import { LibraryRow } from './LibraryRow'
import { queueMenuItems } from './queueActions'
import { TrackRow } from './TrackRow'
import type { ArtistEntry } from './artists'
import type { QueueControl } from './useQueueControl'

interface ArtistsTabProps {
  control: QueueControl
  artists: Accessor<ArtistEntry[]>
  pageSize: number
  resetPageOn: Accessor<unknown>
  selectedSongIds: Accessor<string[]>
  selectMode: Accessor<boolean>
  onToggleSelection: (songId: string, checked: boolean) => void
  onSelectMany: (songIds: string[]) => void
  onToggleMany: (songIds: string[], checked: boolean) => void
}

/** Artists derived from song metadata: queue a whole catalogue, or drill into it. */
export function ArtistsTab(props: ArtistsTabProps) {
  const control = props.control
  const paging = createPagedList(props.artists, props.pageSize)
  createEffect(() => {
    props.resetPageOn()
    paging.setPage(0)
  })

  const [expandedKey, setExpandedKey] = createSignal<string | null>(null)
  const [openAlbumId, setOpenAlbumId] = createSignal<string | null>(null)

  return (
    <Show when={!control.songs.loading} fallback={<p class="list-empty">loading artists...</p>}>
      <ul class="qc-artists-list">
        <For each={paging.paged()} fallback={<li class="list-empty">no artists match</li>}>
          {(artist) => {
            const isExpanded = () => expandedKey() === artist.key
            const songIds = () => artist.songs.map((song) => song.id)
            const coverSong = () => artist.songs.find((song) => song.hasCover)
            const minutes = () => Math.round(artist.totalSeconds / 60)
            const allSongsSelected = () =>
              artist.songs.length > 0 && songIds().every((id) => props.selectedSongIds().includes(id))
            const albumIds = () => artist.albums.map((album) => album.id)
            const enabledAlbums = () => artist.albums.filter((album) => album.isEnabled)
            // Rotation is an album-level flag, so an artist is "in rotation"
            // only insofar as their albums are.
            const rotationState = (): 'none' | 'some' | 'all' => {
              if (artist.albums.length === 0 || enabledAlbums().length === 0) return 'none'
              return enabledAlbums().length === artist.albums.length ? 'all' : 'some'
            }

            // Songs that belong to none of this artist's albums.
            const singles = () => {
              const inAlbums = new Set(artist.albums.flatMap((album) => album.tracks.map((t) => t.id)))
              return artist.songs.filter((song) => !inAlbums.has(song.id))
            }
            const meta = () => {
              const parts = [`${artist.songs.length} ${artist.songs.length === 1 ? 'track' : 'tracks'}`]
              if (artist.albums.length > 0) parts.push(`${artist.albums.length} ${artist.albums.length === 1 ? 'album' : 'albums'}`)
              if (minutes() > 0) parts.push(`${minutes()} min`)
              return parts.join(' • ')
            }
            return (
              <LibraryRow
                coverSrc={coverSong() ? songCoverThumbnailUrl(coverSong()!.id, control.selectedApiBase()) : undefined}
                placeholderFrom={artist.name}
                title={artist.name}
                meta={meta()}
                badges={
                  <>
                    <Show when={rotationState() === 'all'}>
                      <span class="qc-album-badge looping" title="every album by this artist plays automatically when the queue is empty">in rotation</span>
                    </Show>
                    <Show when={rotationState() === 'some'}>
                      <span class="qc-album-badge partial" title={`${enabledAlbums().length} of ${artist.albums.length} albums are in rotation`}>
                        {enabledAlbums().length}/{artist.albums.length} in rotation
                      </span>
                    </Show>
                  </>
                }
                selectMode={props.selectMode()}
                selected={allSongsSelected()}
                onToggleSelect={(checked) => props.onToggleMany(songIds(), checked)}
                expanded={isExpanded()}
                onToggleExpand={() => setExpandedKey(isExpanded() ? null : artist.key)}
                menuItems={() => [
                  ...queueMenuItems(control, songIds, {
                    onSelect: () => props.onSelectMany(songIds()),
                    selectLabel: `Select all ${artist.songs.length}`,
                  }),
                  ...(artist.albums.length > 0
                    ? [{
                        label: rotationState() === 'all'
                          ? `Take ${artist.albums.length === 1 ? 'album' : 'all albums'} out of rotation`
                          : `Put ${artist.albums.length === 1 ? 'album' : 'all albums'} in rotation`,
                        icon: <Radio size={16} />,
                        hint: rotationState() === 'some'
                          ? `${enabledAlbums().length} of ${artist.albums.length} in rotation`
                          : undefined,
                        onSelect: () => void control.setAlbumsEnabled(albumIds(), rotationState() !== 'all'),
                      }]
                    : []),
                ]}
              >
                <div class="qc-lib-details">
                  {/* Albums expand in place rather than queueing on tap — an
                      album chip is navigation, not an action. */}
                  <For each={artist.albums}>
                    {(album) => {
                      const albumOpen = () => openAlbumId() === album.id
                      const albumCover = () => album.tracks.find((track) => track.hasCover)
                      const albumTrackIds = () => album.tracks.map((track) => track.id)
                      const albumSelected = () =>
                        album.tracks.length > 0 && albumTrackIds().every((id) => props.selectedSongIds().includes(id))
                      return (
                        <div class="qc-subsection">
                          {/* Disclosure and menu are siblings: a button can't
                              legally nest inside another button. */}
                          <div class="qc-subsection-head">
                          <button
                            class="qc-subsection-header"
                            type="button"
                            aria-expanded={props.selectMode() ? undefined : albumOpen()}
                            onClick={() => (props.selectMode()
                              ? props.onToggleMany(albumTrackIds(), !albumSelected())
                              : setOpenAlbumId(albumOpen() ? null : album.id))}
                          >
                            <Show
                              when={props.selectMode()}
                              fallback={<ChevronDown size={15} class="qc-subsection-chevron" />}
                            >
                              <input
                                type="checkbox"
                                class="qc-song-select"
                                aria-label={`select ${album.title}`}
                                checked={albumSelected()}
                                onClick={(event) => event.stopPropagation()}
                                onChange={(event) => props.onToggleMany(albumTrackIds(), event.currentTarget.checked)}
                              />
                            </Show>
                            <span class="qc-subsection-thumb">
                              <Show
                                when={albumCover()}
                                fallback={<span class="qc-thumb-placeholder">{album.title.slice(0, 2).toUpperCase()}</span>}
                              >
                                {(track) => (
                                  <img
                                    src={songCoverThumbnailUrl(track().id, control.selectedApiBase())}
                                    alt=""
                                    loading="lazy"
                                  />
                                )}
                              </Show>
                            </span>
                            <span class="qc-subsection-title">{album.title}</span>
                            <Show when={album.isEnabled}>
                              <span class="qc-album-badge looping" title="plays automatically when the queue is empty">in rotation</span>
                            </Show>
                            <span class="qc-subsection-count">{album.tracks.length}</span>
                          </button>
                          {/* Selecting takes over the header, so the chevron
                              moves out here to keep drilling in possible. */}
                          <Show when={props.selectMode()}>
                            <button
                              class="qc-lib-expand"
                              type="button"
                              aria-label={`${albumOpen() ? 'collapse' : 'expand'} ${album.title}`}
                              aria-expanded={albumOpen()}
                              onClick={() => setOpenAlbumId(albumOpen() ? null : album.id)}
                            >
                              <ChevronDown size={16} />
                            </button>
                          </Show>
                          <Show when={!props.selectMode()}>
                          <ActionMenu
                            title={album.title}
                            label={`actions for the album ${album.title}`}
                            compact
                            items={() => [
                              ...queueMenuItems(control, albumTrackIds, {
                                onSelect: () => props.onSelectMany(albumTrackIds()),
                                selectLabel: `Select all ${album.tracks.length}`,
                              }),
                              {
                                label: album.isEnabled ? 'Take out of rotation' : 'Put in rotation',
                                icon: <Radio size={16} />,
                                onSelect: () => void control.handleSetAlbumEnabled(album.id, !album.isEnabled),
                              },
                            ]}
                          />
                          </Show>
                          </div>
                          <Show when={albumOpen()}>
                            <ol class="qc-track-list">
                              <For each={album.tracks}>
                                {(song, idx) => (
                                  <TrackRow
                                    track={song}
                                    index={idx()}
                                    selectMode={props.selectMode()}
                                    selected={props.selectedSongIds().includes(song.id)}
                                    onToggleSelect={(checked) => props.onToggleSelection(song.id, checked)}
                                    queued={control.queuedCountFor(song.id) > 0}
                                    menuItems={() => queueMenuItems(control, () => [song.id], {
                                      queuedCount: () => control.queuedCountFor(song.id),
                                      onSelect: () => props.onToggleSelection(song.id, true),
                                    })}
                                  />
                                )}
                              </For>
                            </ol>
                          </Show>
                        </div>
                      )
                    }}
                  </For>

                  <Show when={singles().length > 0}>
                    <div class="qc-subsection">
                      <Show when={artist.albums.length > 0}>
                        <span class="qc-subsection-head">
                        <span class="qc-subsection-header is-static">
                          <span class="qc-subsection-thumb is-empty" aria-hidden="true" />
                          <span class="qc-subsection-title">Singles</span>
                          <span class="qc-subsection-count">{singles().length}</span>
                        </span>
                        </span>
                      </Show>
                      <ol class="qc-track-list">
                        <For each={singles()}>
                          {(song, idx) => (
                            <TrackRow
                              track={song}
                              index={idx()}
                              secondary={song.album || 'Single'}
                              selectMode={props.selectMode()}
                              selected={props.selectedSongIds().includes(song.id)}
                              onToggleSelect={(checked) => props.onToggleSelection(song.id, checked)}
                              queued={control.queuedCountFor(song.id) > 0}
                              menuItems={() => queueMenuItems(control, () => [song.id], {
                                queuedCount: () => control.queuedCountFor(song.id),
                                onSelect: () => props.onToggleSelection(song.id, true),
                              })}
                            />
                          )}
                        </For>
                      </ol>
                    </div>
                  </Show>
                </div>
              </LibraryRow>
            )
          }}
        </For>
      </ul>
      <Show when={paging.pageCount() > 1}>
        <PaginationRow page={paging.page()} pageCount={paging.pageCount()} onPageChange={paging.setPage} />
      </Show>
    </Show>
  )
}
