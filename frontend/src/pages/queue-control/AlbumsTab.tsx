import { createEffect, createSignal, For, Show, type Accessor } from 'solid-js'
import { Radio } from 'lucide-solid'
import { PaginationRow } from '../../shared/components/PaginationRow'
import { createPagedList } from '../../shared/primitives/createPagedList'
import { songCoverThumbnailUrl, type RadioAlbum } from '../../shared/lib/radio'
import { LibraryRow } from './LibraryRow'
import { queueMenuItems } from './queueActions'
import { TrackRow } from './TrackRow'
import { normalizeTitleForUi } from './helpers'
import type { QueueControl } from './useQueueControl'

interface AlbumsTabProps {
  control: QueueControl
  albums: Accessor<RadioAlbum[]>
  pageSize: number
  resetPageOn: Accessor<unknown>
  selectedSongIds: Accessor<string[]>
  selectMode: Accessor<boolean>
  onToggleSelection: (songId: string, checked: boolean) => void
  onSelectMany: (songIds: string[]) => void
  onToggleMany: (songIds: string[], checked: boolean) => void
}

/** The album list: rotation membership, weighting, duplicate merging, and track listings. */
export function AlbumsTab(props: AlbumsTabProps) {
  const control = props.control
  const paging = createPagedList(props.albums, props.pageSize)
  createEffect(() => {
    props.resetPageOn()
    paging.setPage(0)
  })

  const [expandedAlbumId, setExpandedAlbumId] = createSignal<string | null>(null)
  const allAlbums = () => control.albums() ?? []

  return (
    <>
      <Show when={allAlbums().length > 0}>
        <div class="qc-rotation-bar">
          <span class="qc-rotation-count">
            {allAlbums().filter((album) => album.isEnabled).length}/{allAlbums().length} in rotation
          </span>
          <div class="qc-rotation-actions">
            <button class="pill-button subtle" type="button" onClick={() => void control.setAllAlbumsEnabled(true)}>
              add all to rotation
            </button>
            <button class="pill-button subtle danger-button" type="button" onClick={() => void control.setAllAlbumsEnabled(false)}>
              clear rotation
            </button>
          </div>
        </div>
      </Show>

      <Show when={!control.albums.loading} fallback={<p class="list-empty">loading albums...</p>}>
        <ul class="qc-albums-list">
          <For each={paging.paged()} fallback={<li class="list-empty">no albums match</li>}>
            {(album) => {
              const isExpanded = () => expandedAlbumId() === album.id
              const duplicate = () => allAlbums().find((a) => a.id !== album.id && normalizeTitleForUi(a.title) === normalizeTitleForUi(album.title))
              const coverTrack = () => album.tracks.find((track) => track.hasCover)
              const totalMinutes = () => {
                const seconds = album.tracks.reduce((sum, track) => sum + (track.durationSeconds ?? 0), 0)
                return seconds >= 60 ? Math.round(seconds / 60) : 0
              }
              const trackIds = () => album.tracks.map((track) => track.id)
              // A grouping row counts as selected only when all of it is.
              const allTracksSelected = () =>
                album.tracks.length > 0 && trackIds().every((id) => props.selectedSongIds().includes(id))
              return (
                <LibraryRow
                  coverSrc={coverTrack() ? songCoverThumbnailUrl(coverTrack()!.id, control.selectedApiBase()) : undefined}
                  placeholderFrom={album.title}
                  title={album.title}
                  meta={`${album.tracks.length} ${album.tracks.length === 1 ? 'track' : 'tracks'}${totalMinutes() ? ` \u2022 ${totalMinutes()} min` : ''}`}
                  badges={
                    <>
                      <Show when={album.isEnabled}>
                        <span class="qc-album-badge looping" title="plays automatically when the queue is empty">in rotation</span>
                      </Show>
                      <Show when={duplicate()}>
                        <span class="qc-album-badge duplicate">duplicate</span>
                      </Show>
                    </>
                  }
                  selectMode={props.selectMode()}
                  selected={allTracksSelected()}
                  onToggleSelect={(checked) => props.onToggleMany(trackIds(), checked)}
                  expanded={isExpanded()}
                  onToggleExpand={() => setExpandedAlbumId(isExpanded() ? null : album.id)}
                  menuItems={() => [
                    ...queueMenuItems(control, trackIds, {
                      onSelect: () => props.onSelectMany(trackIds()),
                      selectLabel: `Select all ${album.tracks.length}`,
                    }),
                    {
                      label: album.isEnabled ? 'Take out of rotation' : 'Put in rotation',
                      icon: <Radio size={16} />,
                      onSelect: () => void control.handleSetAlbumEnabled(album.id, !album.isEnabled),
                    },
                  ]}
                >
                    <div class="qc-album-details">
                      <div class="qc-album-actions">
                        <div class="qc-album-actions-group">
                          <label class="inline-check qc-album-loop-check" title="plays automatically when the queue is empty">
                            <input
                              type="checkbox"
                              checked={album.isEnabled}
                              onChange={(e) => void control.handleSetAlbumEnabled(album.id, e.currentTarget.checked)}
                            />
                            in station rotation
                          </label>
                          <label class="qc-weight-label" title="how often shuffle picks from this album">
                            rotation
                            <select
                              class="qc-weight-select"
                              value={String(control.rotationInfo()?.weights[album.id] ?? 2)}
                              onChange={(e) => void control.handleSetAlbumWeight(album.id, Number(e.currentTarget.value))}
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
                            onClick={() => void control.handleDeleteAlbum(album.id)}
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
                                    void control.handleMergeAlbums(album.id, dup().id)
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
                                const target = allAlbums().find((a) => a.id === val)
                                if (target && confirm(`Merge this album "${album.title}" into "${target.title}"? All tracks will be combined under "${target.title}".`)) {
                                  void control.handleMergeAlbums(album.id, val)
                                }
                                e.currentTarget.value = ''
                              }
                            }}
                          >
                            <option value="">merge into...</option>
                            <For each={allAlbums().filter((a) => a.id !== album.id)}>
                              {(other) => <option value={other.id}>{other.title}</option>}
                            </For>
                          </select>
                        </div>
                      </div>

                      <ol class="qc-track-list">
                        <For each={album.tracks}>
                          {(track, idx) => (
                            <TrackRow
                              track={track}
                              index={idx()}
                              selectMode={props.selectMode()}
                              selected={props.selectedSongIds().includes(track.id)}
                              onToggleSelect={(checked) => props.onToggleSelection(track.id, checked)}
                              queued={control.queuedCountFor(track.id) > 0}
                              menuItems={() => queueMenuItems(control, () => [track.id], {
                                queuedCount: () => control.queuedCountFor(track.id),
                                onSelect: () => props.onToggleSelection(track.id, true),
                              })}
                            />
                          )}
                        </For>
                      </ol>
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
    </>
  )
}
