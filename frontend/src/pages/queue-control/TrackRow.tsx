import { Show, type JSX } from 'solid-js'
import { ActionMenu, type ActionMenuApi, type ActionMenuItem } from './ActionMenu'
import { formatTime } from './helpers'
import type { Song } from '../../shared/lib/radio'

interface TrackRowProps {
  track: Song
  index: number
  /** Shown instead of the artist — album name in an artist list, artist elsewhere. */
  secondary?: string
  selectMode?: boolean
  selected?: boolean
  onToggleSelect?: (checked: boolean) => void
  /** Optional override for tapping the row body; defaults to opening the menu. */
  onActivate?: () => void
  menuItems: () => ActionMenuItem[]
  /** Reorder arrows, only used by editable sets. */
  leading?: JSX.Element
  queued?: boolean
}

/**
 * The compact track line inside an expanded album, artist, or set. Same
 * economy as LibraryRow: number, title, one menu — no button cluster.
 */
export function TrackRow(props: TrackRowProps) {
  let menu: ActionMenuApi | undefined
  const activate = () => {
    if (props.selectMode) props.onToggleSelect?.(!props.selected)
    else if (props.onActivate) props.onActivate()
    else menu?.open()
  }

  return (
    <li class="qc-track-row" classList={{ 'is-queued': !!props.queued, 'is-selected': !!props.selected }}>
      <div
        class="qc-track-main"
        role="button"
        tabindex={0}
        onClick={activate}
        onKeyDown={(event) => {
          if (event.key === 'Enter' || event.key === ' ') {
            event.preventDefault()
            activate()
          }
        }}
      >
        <Show
          when={props.selectMode}
          fallback={<span class="qc-track-num">{props.index + 1}</span>}
        >
          <input
            type="checkbox"
            class="qc-song-select"
            aria-label={`select ${props.track.title}`}
            checked={!!props.selected}
            onClick={(event) => event.stopPropagation()}
            onChange={(event) => props.onToggleSelect?.(event.currentTarget.checked)}
          />
        </Show>

        <span class="qc-track-copy">
          <span class="qc-track-title">{props.track.title}</span>
          <span class="qc-track-secondary">{props.secondary ?? props.track.artist}</span>
        </span>

        <span class="qc-track-duration">{formatTime(props.track.durationSeconds)}</span>
      </div>

      {props.leading}

      <Show when={!props.selectMode}>
        <ActionMenu
          title={props.track.title}
          items={props.menuItems}
          compact
          onInit={(api) => (menu = api)}
        />
      </Show>
    </li>
  )
}
