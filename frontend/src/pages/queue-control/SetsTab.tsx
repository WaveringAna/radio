import { createEffect, createSignal, For, Show, type Accessor } from 'solid-js'
import { Copy, Pencil, Play, Repeat, Shuffle, Trash2, X } from 'lucide-solid'
import { PaginationRow } from '../../shared/components/PaginationRow'
import { createPagedList } from '../../shared/primitives/createPagedList'
import { songCoverThumbnailUrl, type Playlist } from '../../shared/lib/radio'
import { LibraryRow } from './LibraryRow'
import { queueMenuItems } from './queueActions'
import { TrackRow } from './TrackRow'
import type { QueueControl } from './useQueueControl'

interface SetsTabProps {
  control: QueueControl
  playlists: Accessor<Playlist[]>
  pageSize: number
  resetPageOn: Accessor<unknown>
  selectedSongIds: Accessor<string[]>
  selectMode: Accessor<boolean>
  onToggleSelection: (songId: string, checked: boolean) => void
}

/** Saved sets: play, edit in place, and pin one to loop when the queue drains. */
export function SetsTab(props: SetsTabProps) {
  const control = props.control
  const paging = createPagedList(props.playlists, props.pageSize)
  createEffect(() => {
    props.resetPageOn()
    paging.setPage(0)
  })

  const [expandedId, setExpandedId] = createSignal<string | null>(null)
  const [renamingId, setRenamingId] = createSignal<string | null>(null)
  const [draftName, setDraftName] = createSignal('')

  const beginRename = (playlist: Playlist) => {
    setRenamingId(playlist.id)
    setDraftName(playlist.name)
  }

  const commitRename = async (playlist: Playlist) => {
    const name = draftName().trim()
    if (!name || name === playlist.name) {
      setRenamingId(null)
      return
    }
    if (await control.renamePlaylistTo(playlist.id, name)) setRenamingId(null)
  }

  /** Moves a track one slot up or down by rewriting the whole order. */
  const moveTrack = (playlist: Playlist, index: number, delta: number) => {
    const target = index + delta
    if (target < 0 || target >= playlist.tracks.length) return
    const ids = playlist.tracks.map((track) => track.id)
    ;[ids[index], ids[target]] = [ids[target], ids[index]]
    void control.reorderPlaylist(playlist.id, ids)
  }

  return (
    <Show when={!control.playlists.loading} fallback={<p class="list-empty">loading sets...</p>}>
      <ul class="qc-sets-list">
        <For each={paging.paged()} fallback={<li class="list-empty">no sets saved yet</li>}>
          {(playlist) => {
            const isExpanded = () => expandedId() === playlist.id
            const isLooping = () => control.loopPlaylistId() === playlist.id
            const coverTrack = () => playlist.tracks.find((track) => track.hasCover)
            const minutes = () => Math.round(playlist.tracks.reduce((sum, t) => sum + (t.durationSeconds ?? 0), 0) / 60)
            const trackIds = () => playlist.tracks.map((track) => track.id)
            return (
              <LibraryRow
                coverSrc={coverTrack() ? songCoverThumbnailUrl(coverTrack()!.id, control.selectedApiBase()) : undefined}
                placeholderFrom={playlist.name}
                title={playlist.name}
                meta={`${playlist.tracks.length} ${playlist.tracks.length === 1 ? 'track' : 'tracks'}${minutes() > 0 ? ` \u2022 ${minutes()} min` : ''}`}
                badges={
                  <>
                    <Show when={isLooping()}>
                      <span class="qc-album-badge looping" title="reloads automatically when the queue drains">looping</span>
                    </Show>
                    <Show when={playlist.shuffleOnLoad}>
                      <span class="qc-album-badge shuffled" title="loads in random order">shuffled</span>
                    </Show>
                  </>
                }
                selectMode={props.selectMode()}
                expanded={isExpanded()}
                onToggleExpand={() => setExpandedId(isExpanded() ? null : playlist.id)}
                menuItems={() => [
                  ...queueMenuItems(control, trackIds),
                  {
                    label: 'Play now (replaces queue)',
                    icon: <Play size={16} />,
                    disabled: playlist.tracks.length === 0,
                    onSelect: () => {
                      const queued = control.snapshot()?.queue.length ?? 0
                      if (queued === 0 || confirm(`Replace the current queue (${queued} tracks) with "${playlist.name}"?`)) {
                        void control.loadPlaylistToQueue(playlist.id, true)
                      }
                    },
                  },
                  {
                    label: isLooping() ? 'Stop looping this set' : 'Loop this set',
                    icon: <Repeat size={16} />,
                    hint: isLooping() ? undefined : 'reloads when the queue drains',
                    onSelect: () => void control.applyLoopPlaylist(isLooping() ? null : playlist.id),
                  },
                  {
                    label: playlist.shuffleOnLoad ? 'Load in saved order' : 'Always load shuffled',
                    icon: <Shuffle size={16} />,
                    onSelect: () => void control.togglePlaylistShuffleOnLoad(playlist.id, !playlist.shuffleOnLoad),
                  },
                  { label: 'Rename', icon: <Pencil size={16} />, onSelect: () => beginRename(playlist) },
                  {
                    label: 'Duplicate',
                    icon: <Copy size={16} />,
                    onSelect: () => void control.copyPlaylist(playlist.id, `${playlist.name} copy`),
                  },
                  {
                    label: 'Resequence the saved order',
                    icon: <Shuffle size={16} />,
                    hint: 'reorders by artist, energy & tempo',
                    disabled: playlist.tracks.length < 2,
                    onSelect: () => void control.resequencePlaylist(playlist.id),
                  },
                  {
                    label: 'Delete set',
                    icon: <Trash2 size={16} />,
                    danger: true,
                    onSelect: () => {
                      if (confirm(`Delete the set "${playlist.name}"? The songs stay in the library.`)) {
                        void control.removePlaylist(playlist.id)
                      }
                    },
                  },
                ]}
              >
                <div class="qc-lib-details">
                  <Show when={renamingId() === playlist.id}>
                    <div class="playlist-save-form">
                      <input
                        type="text"
                        class="qc-save-set-input"
                        aria-label="set name"
                        value={draftName()}
                        onInput={(event) => setDraftName(event.currentTarget.value)}
                        onKeyDown={(event) => {
                          if (event.key === 'Enter') void commitRename(playlist)
                          if (event.key === 'Escape') setRenamingId(null)
                        }}
                      />
                      <button class="pill-button" type="button" disabled={!draftName().trim()} onClick={() => void commitRename(playlist)}>
                        save
                      </button>
                      <button class="pill-button subtle" type="button" onClick={() => setRenamingId(null)}>
                        cancel
                      </button>
                    </div>
                  </Show>

                  <ol class="qc-track-list">
                    <For each={playlist.tracks} fallback={<li class="list-empty">this set is empty</li>}>
                      {(track, idx) => (
                        <TrackRow
                          track={track}
                          index={idx()}
                          selectMode={props.selectMode()}
                          selected={props.selectedSongIds().includes(track.id)}
                          onToggleSelect={(checked) => props.onToggleSelection(track.id, checked)}
                          queued={control.queuedCountFor(track.id) > 0}
                          leading={
                            <div class="qc-track-move">
                              <button
                                class="qc-move-btn"
                                type="button"
                                aria-label={`move ${track.title} up`}
                                disabled={idx() === 0}
                                onClick={() => moveTrack(playlist, idx(), -1)}
                              >\u2191</button>
                              <button
                                class="qc-move-btn"
                                type="button"
                                aria-label={`move ${track.title} down`}
                                disabled={idx() === playlist.tracks.length - 1}
                                onClick={() => moveTrack(playlist, idx(), 1)}
                              >\u2193</button>
                            </div>
                          }
                          menuItems={() => [
                            ...queueMenuItems(control, () => [track.id], {
                              queuedCount: () => control.queuedCountFor(track.id),
                              onSelect: () => props.onToggleSelection(track.id, true),
                            }),
                            {
                              label: 'Remove from this set',
                              icon: <X size={16} />,
                              danger: true,
                              onSelect: () => void control.dropPlaylistTrack(playlist.id, idx() + 1),
                            },
                          ]}
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
  )
}
