import { Show, type JSX } from 'solid-js'
import { ChevronDown } from 'lucide-solid'
import { ActionMenu, type ActionMenuApi, type ActionMenuItem } from './ActionMenu'

export interface LibraryRowProps {
  coverSrc?: string
  /** Text the placeholder derives its initials from. */
  placeholderFrom: string
  title: string
  meta: string
  /** Badges rendered under the title, on the meta line. */
  badges?: JSX.Element
  /** Right-aligned detail (duration). Hidden on narrow screens. */
  detail?: JSX.Element
  /** Selection-mode checkbox. Only rendered when `selectMode` is on. */
  selectMode?: boolean
  selected?: boolean
  onToggleSelect?: (checked: boolean) => void
  /**
   * Optional override for tapping the row body. Left unset, a row that can't
   * expand opens its own menu instead — a tap should never silently change
   * what's on air.
   */
  onActivate?: () => void
  /** Renders the chevron and makes the row body a disclosure instead. */
  onToggleExpand?: () => void
  expanded?: boolean
  /** Everything else, collapsed behind the overflow menu. */
  menuItems?: () => ActionMenuItem[]
  children?: JSX.Element
  dimmed?: boolean
}

/**
 * One row shape for every library result — song, album track, album, artist,
 * set. Deliberately spare: art, two lines of text, and a single overflow menu.
 * Verbs live in the menu so the title always gets the width it needs.
 */
export function LibraryRow(props: LibraryRowProps) {
  let menu: ActionMenuApi | undefined
  const interactive = () => !!props.onToggleExpand || !!props.onActivate || !!props.menuItems
  const activate = () => {
    if (props.selectMode) {
      props.onToggleSelect?.(!props.selected)
      return
    }
    if (props.onToggleExpand) props.onToggleExpand()
    else if (props.onActivate) props.onActivate()
    else menu?.open()
  }

  return (
    <li class="qc-lib-item" classList={{ expanded: !!props.expanded, 'is-dimmed': !!props.dimmed }}>
      <div
        class="qc-lib-row"
        classList={{ 'is-interactive': interactive() || !!props.selectMode, 'is-selected': !!props.selected }}
        role={interactive() || props.selectMode ? 'button' : undefined}
        tabindex={interactive() || props.selectMode ? 0 : undefined}
        aria-expanded={props.onToggleExpand ? !!props.expanded : undefined}
        onClick={activate}
        onKeyDown={(event) => {
          if (event.key === 'Enter' || event.key === ' ') {
            event.preventDefault()
            activate()
          }
        }}
      >
        <Show when={props.selectMode}>
          <input
            type="checkbox"
            class="qc-song-select"
            aria-label={`select ${props.title}`}
            checked={!!props.selected}
            onClick={(event) => event.stopPropagation()}
            onChange={(event) => props.onToggleSelect?.(event.currentTarget.checked)}
          />
        </Show>

        <div class="qc-song-thumb">
          <Show
            when={props.coverSrc}
            fallback={<div class="qc-thumb-placeholder">{props.placeholderFrom.slice(0, 4).toUpperCase()}</div>}
          >
            {(src) => <img src={src()} alt="" loading="lazy" />}
          </Show>
        </div>

        <div class="qc-song-info">
          <span class="qc-song-title">{props.title}</span>
          <span class="qc-song-meta-line">
            {props.meta}
            {props.badges}
          </span>
        </div>

        <Show when={props.detail}>
          <span class="qc-lib-detail">{props.detail}</span>
        </Show>

        <Show when={props.menuItems && !props.selectMode}>
          <div class="qc-lib-actions" onClick={(event) => event.stopPropagation()}>
            <ActionMenu
              title={props.title}
              items={props.menuItems!}
              onInit={(api) => (menu = api)}
            />
          </div>
        </Show>

        {/* A real button, not decoration: in select mode the row body toggles
            selection, so the chevron is the only way left to drill in. */}
        <Show when={props.onToggleExpand}>
          <button
            class="qc-lib-expand"
            type="button"
            aria-label={`${props.expanded ? 'collapse' : 'expand'} ${props.title}`}
            aria-expanded={!!props.expanded}
            onClick={(event) => {
              event.stopPropagation()
              props.onToggleExpand?.()
            }}
          >
            <ChevronDown size={18} />
          </button>
        </Show>
      </div>

      <Show when={props.expanded}>{props.children}</Show>
    </li>
  )
}
