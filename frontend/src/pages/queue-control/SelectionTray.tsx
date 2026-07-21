import { createSignal, For, Show, type Accessor } from 'solid-js'
import { CornerUpRight, X } from 'lucide-solid'
import type { Playlist } from '../../shared/lib/radio'

interface SelectionTrayProps {
  selectedSongIds: Accessor<string[]>
  playlists: Accessor<Playlist[]>
  onQueue: (songIds: string[], atTop: boolean, sequence?: boolean) => void
  onSaveAsSet: (name: string) => Promise<boolean>
  onAddToSet: (playlistId: string, songIds: string[]) => Promise<boolean>
  onClear: () => void
}

/**
 * Docked summary of the current multi-selection. It lives above the tabs
 * rather than inside one of them, so a selection built across Songs, Artists,
 * and Albums survives switching between them.
 */
export function SelectionTray(props: SelectionTrayProps) {
  const [naming, setNaming] = createSignal(false)
  const [newSetName, setNewSetName] = createSignal('')

  const count = () => props.selectedSongIds().length

  const save = async () => {
    if (await props.onSaveAsSet(newSetName())) {
      setNewSetName('')
      setNaming(false)
    }
  }

  return (
    <Show when={count() > 0}>
      <div class="qc-selection-bar" role="region" aria-label="selected songs">
        <div class="qc-selection-bar-row">
          <span class="qc-selection-count">{count()} {count() === 1 ? 'song' : 'songs'} selected</span>
          <div class="qc-selection-actions">
            <button class="pill-button" type="button" onClick={() => props.onQueue(props.selectedSongIds(), false)}>
              add to queue
            </button>
            <button
              class="pill-button subtle"
              type="button"
              title="jump these ahead of the queue"
              onClick={() => props.onQueue(props.selectedSongIds(), true)}
            >
              <CornerUpRight size={14} /> play next
            </button>
            <button
              class="pill-button subtle"
              type="button"
              title="add these sequenced by artist, energy and tempo"
              onClick={() => props.onQueue(props.selectedSongIds(), false, true)}
            >
              shuffle in
            </button>
            <button class="pill-button subtle" type="button" onClick={() => { setNaming(!naming()); setNewSetName('') }}>
              {naming() ? 'cancel' : 'save as set'}
            </button>
            <Show when={props.playlists().length > 0}>
              <select
                class="qc-album-merge-select"
                aria-label="add selection to an existing set"
                onChange={(event) => {
                  const id = event.currentTarget.value
                  if (id) {
                    void props.onAddToSet(id, props.selectedSongIds())
                    event.currentTarget.value = ''
                  }
                }}
              >
                <option value="">add to set...</option>
                <For each={props.playlists()}>
                  {(playlist) => <option value={playlist.id}>{playlist.name}</option>}
                </For>
              </select>
            </Show>
            <button class="pill-button subtle" type="button" aria-label="clear selection" onClick={() => props.onClear()}>
              <X size={14} />
            </button>
          </div>
        </div>
        <Show when={naming()}>
          <div class="playlist-save-form">
            <input
              type="text"
              class="qc-save-set-input"
              placeholder="name your set"
              value={newSetName()}
              onInput={(event) => setNewSetName(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter') void save()
              }}
            />
            <button class="pill-button" type="button" disabled={!newSetName().trim()} onClick={() => void save()}>
              save
            </button>
          </div>
        </Show>
      </div>
    </Show>
  )
}
