import { createSignal, For, onCleanup, Show, type JSX } from 'solid-js'
import { Portal } from 'solid-js/web'
import { MoreHorizontal } from 'lucide-solid'

export interface ActionMenuItem {
  label: string
  icon?: JSX.Element
  onSelect: () => void
  danger?: boolean
  disabled?: boolean
  /** Shown greyed under the label — e.g. "already queued ×2". */
  hint?: string
}

export interface ActionMenuApi {
  /** Opens the menu as though its trigger had been tapped. */
  open: () => void
}

interface ActionMenuProps {
  /** What the menu acts on — used for the trigger's accessible name. */
  title: string
  items: () => ActionMenuItem[]
  /** Overrides the trigger's accessible name. */
  label?: string
  compact?: boolean
  /**
   * Hands back an imperative opener so a parent can raise this menu from a
   * bigger tap target — the whole row rather than the small trigger.
   */
  onInit?: (api: ActionMenuApi) => void
}

/**
 * The single overflow control every row carries: a popover anchored to its
 * trigger, at every viewport size.
 *
 * Rows used to wear every verb as its own button, which on a phone left almost
 * no room for the title. Collapsing them here means new actions cost no
 * horizontal space.
 */
export function ActionMenu(props: ActionMenuProps) {
  const [open, setOpen] = createSignal(false)
  const [anchor, setAnchor] = createSignal<DOMRect | null>(null)
  let triggerEl: HTMLButtonElement | undefined

  const close = () => {
    setOpen(false)
    setAnchor(null)
  }

  // Always anchored to the trigger, even when raised from the row body, so the
  // popover lands in the same place however you opened it.
  const openMenu = () => {
    setAnchor(triggerEl?.getBoundingClientRect() ?? null)
    setOpen(true)
  }

  const toggle = (event: MouseEvent) => {
    event.stopPropagation()
    if (open()) close()
    else openMenu()
  }

  props.onInit?.({ open: openMenu })

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === 'Escape') close()
  }
  window.addEventListener('keydown', onKeyDown)
  // Any scroll or resize invalidates the anchor rect, so dismiss rather than
  // let the popover drift away from its row.
  window.addEventListener('resize', close)
  window.addEventListener('scroll', close, true)
  onCleanup(() => {
    window.removeEventListener('keydown', onKeyDown)
    window.removeEventListener('resize', close)
    window.removeEventListener('scroll', close, true)
  })

  // Flip above the trigger when there isn't room below.
  const panelStyle = () => {
    const rect = anchor()
    if (!rect) return ''
    const width = Math.min(240, window.innerWidth - 16)
    // Right-align to the trigger, then keep it inside the viewport.
    const left = Math.min(Math.max(8, rect.right - width), window.innerWidth - width - 8)
    const below = window.innerHeight - rect.bottom
    if (below < 240 && rect.top > below) {
      return `left:${left}px; bottom:${window.innerHeight - rect.top + 6}px; width:${width}px;`
    }
    return `left:${left}px; top:${rect.bottom + 6}px; width:${width}px;`
  }

  const run = (item: ActionMenuItem) => {
    if (item.disabled) return
    close()
    item.onSelect()
  }

  return (
    <>
      <button
        ref={triggerEl}
        class="qc-menu-trigger"
        classList={{ compact: props.compact, 'is-open': open() }}
        type="button"
        aria-haspopup="menu"
        aria-expanded={open()}
        aria-label={props.label ?? `more actions for ${props.title}`}
        onClick={toggle}
      >
        <MoreHorizontal size={18} />
      </button>

      <Show when={open()}>
        <Portal>
          <div class="qc-menu-backdrop" onClick={close} />
          <div class="qc-menu-panel" role="menu" style={panelStyle()}>
            <For each={props.items()}>
              {(item) => (
                <button
                  class="qc-menu-item"
                  classList={{ danger: item.danger }}
                  type="button"
                  role="menuitem"
                  disabled={item.disabled}
                  onClick={(event) => { event.stopPropagation(); run(item) }}
                >
                  <span class="qc-menu-item-icon">{item.icon}</span>
                  <span class="qc-menu-item-label">
                    {item.label}
                    <Show when={item.hint}>
                      <small class="qc-menu-item-hint">{item.hint}</small>
                    </Show>
                  </span>
                </button>
              )}
            </For>
          </div>
        </Portal>
      </Show>
    </>
  )
}
