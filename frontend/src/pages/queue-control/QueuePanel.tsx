import { createSignal, For, Show } from 'solid-js'
import { ChevronsDown, ChevronsUp, Clock, GripVertical, Repeat, Repeat1, Shuffle, Trash2, Undo2, X } from 'lucide-solid'
import { songCoverThumbnailUrl, type LoopMode, type QueueItem } from '../../shared/lib/radio'
import { ActionMenu } from './ActionMenu'
import type { QueueControl } from './useQueueControl'

// One button cycles the loop mode; these keep the labelling in one place.
const NEXT_LOOP_MODE: Record<LoopMode, LoopMode> = { off: 'queue', queue: 'one', one: 'off' }
const LOOP_LABELS: Record<LoopMode, string> = { off: 'loop off', queue: 'loop queue', one: 'repeat one' }
const LOOP_TITLES: Record<LoopMode, string> = {
  off: 'loop off — played tracks leave the queue. Click to loop the queue.',
  queue: 'loop queue — finished tracks go to the back. Click to repeat one.',
  one: 'repeat one — the current track restarts forever. Click to turn looping off.',
}

/** The broadcast-order column: the pending queue, drag reordering, and what plays after it. */
export function QueuePanel(props: { control: QueueControl }) {
  const control = props.control
  const [draggingQueueId, setDraggingQueueId] = createSignal<string | null>(null)
  const [dragOverQueueId, setDragOverQueueId] = createSignal<string | null>(null)
  const [savingQueue, setSavingQueue] = createSignal(false)
  const [newPlaylistName, setNewPlaylistName] = createSignal('')
  let queueListEl: HTMLUListElement | undefined

  const queue = () => control.snapshot()?.queue ?? []
  const queueLength = () => queue().length
  const loopingSet = () => (control.playlists() ?? []).find((set) => set.id === control.loopPlaylistId())

  const saveQueueAsPlaylist = async () => {
    const songIds = queue().map((item) => item.songId)
    if (await control.saveAsPlaylist(newPlaylistName(), songIds)) {
      setNewPlaylistName('')
      setSavingQueue(false)
    }
  }

  const handleQueueDrop = async (targetQueueId: string) => {
    const sourceId = draggingQueueId()
    setDraggingQueueId(null)
    if (!sourceId || sourceId === targetQueueId) return
    const ids = queue().map((item) => item.id)
    const sourceIndex = ids.indexOf(sourceId)
    const targetIndex = ids.indexOf(targetQueueId)
    if (sourceIndex < 0 || targetIndex < 0) return
    const reordered = [...ids]
    reordered.splice(sourceIndex, 1)
    reordered.splice(targetIndex, 0, sourceId)
    await control.applyQueueOrder(reordered)
  }

  // Pointer-based reordering: works for touch and mouse alike, unlike HTML5
  // drag-and-drop which never fires on touch screens. The grip is the only
  // initiator so the rest of the row still scrolls the page on a phone.
  // Shared drag session used by both initiators: the grip (immediate, any
  // pointer) and long-press on the row body (touch). Listeners live on the
  // element holding pointer capture; onEnd releases initiator-side hooks.
  const runQueueDragSession = (
    item: QueueItem,
    row: HTMLElement,
    captureEl: HTMLElement,
    originY: number,
    onEnd?: () => void,
  ) => {
    setDraggingQueueId(item.id)
    let overId: string | null = null

    const move = (ev: PointerEvent) => {
      row.style.transform = `translateY(${ev.clientY - originY}px)`
      overId = null
      for (const li of queueListEl?.querySelectorAll<HTMLElement>('li[data-queue-id]') ?? []) {
        if (li === row) continue
        const rect = li.getBoundingClientRect()
        if (ev.clientY >= rect.top && ev.clientY < rect.bottom) {
          overId = li.dataset.queueId ?? null
          break
        }
      }
      setDragOverQueueId(overId)
    }
    const finish = (commit: boolean) => {
      captureEl.removeEventListener('pointermove', move)
      captureEl.removeEventListener('pointerup', up)
      captureEl.removeEventListener('pointercancel', cancel)
      row.style.transform = ''
      setDragOverQueueId(null)
      onEnd?.()
      if (commit && overId) {
        void handleQueueDrop(overId)
      } else {
        setDraggingQueueId(null)
      }
    }
    const up = () => finish(true)
    const cancel = () => finish(false)
    captureEl.addEventListener('pointermove', move)
    captureEl.addEventListener('pointerup', up)
    captureEl.addEventListener('pointercancel', cancel)
  }

  const startQueueDrag = (item: QueueItem, event: PointerEvent) => {
    if (event.pointerType === 'mouse' && event.button !== 0) return
    const grip = event.currentTarget as HTMLElement
    const row = grip.closest('li')
    if (!row || !queueListEl) return
    event.preventDefault()
    grip.setPointerCapture(event.pointerId)
    runQueueDragSession(item, row, grip, event.clientY)
  }

  // Long-press on the row body starts a touch drag: hold ~a third of a second
  // without moving. A scroll gesture (movement or the browser's pointercancel)
  // aborts the timer, so normal scrolling through the queue is unaffected.
  const LONG_PRESS_MS = 350
  const LONG_PRESS_SLOP_PX = 10
  const startQueueRowLongPress = (item: QueueItem, event: PointerEvent) => {
    if (event.pointerType !== 'touch') return
    const row = event.currentTarget as HTMLElement
    if ((event.target as HTMLElement | null)?.closest('button')) return
    const startX = event.clientX
    const startY = event.clientY
    let lastY = startY
    let engaged = false

    // Once the drag engages, swallowing touchmove is the only way to keep the
    // browser from turning further finger movement into a scroll.
    const blockScroll = (ev: TouchEvent) => {
      if (engaged) ev.preventDefault()
    }
    row.addEventListener('touchmove', blockScroll, { passive: false })

    const abort = () => {
      window.clearTimeout(timer)
      row.removeEventListener('pointermove', premove)
      row.removeEventListener('pointerup', abort)
      row.removeEventListener('pointercancel', abort)
      if (!engaged) row.removeEventListener('touchmove', blockScroll)
    }
    const premove = (ev: PointerEvent) => {
      lastY = ev.clientY
      if (Math.hypot(ev.clientX - startX, ev.clientY - startY) > LONG_PRESS_SLOP_PX) abort()
    }
    row.addEventListener('pointermove', premove)
    row.addEventListener('pointerup', abort)
    row.addEventListener('pointercancel', abort)

    const timer = window.setTimeout(() => {
      engaged = true
      abort()
      navigator.vibrate?.(12)
      row.setPointerCapture(event.pointerId)
      runQueueDragSession(item, row, row, lastY, () => {
        row.removeEventListener('touchmove', blockScroll)
      })
    }, LONG_PRESS_MS)
  }

  const renderQueueItem = (item: QueueItem, index: () => number) => {
    const profile = () => control.profileFor(item.queuedByDid)
    return (
      <li
        data-queue-id={item.id}
        onPointerDown={(event) => startQueueRowLongPress(item, event)}
        classList={{
          'queue-drag-source': draggingQueueId() === item.id,
          'drag-over': dragOverQueueId() === item.id,
        }}
        class="qc-queue-item"
      >
        <div class="qc-queue-grip" onPointerDown={(event) => startQueueDrag(item, event)}>
          <GripVertical size={16} />
        </div>
        <div class="qc-queue-index">{index() + 1}</div>
        <div class="qc-queue-thumb">
          <Show
            when={(control.songs() ?? []).some((song) => song.id === item.songId && song.hasCover)}
            fallback={<div class="qc-thumb-placeholder">{item.title.slice(0, 4).toUpperCase()}</div>}
          >
            <img src={songCoverThumbnailUrl(item.songId, control.selectedApiBase())} alt="" loading="lazy" />
          </Show>
        </div>
        <div class="qc-queue-copy">
          <span class="qc-queue-title">{item.title}</span>
          <span class="qc-queue-artist">
            {item.artist}
            <Show when={!item.isShuffle && profile().handle}>
              <span class="qc-queue-by"> · @{profile().handle}</span>
            </Show>
          </span>
        </div>
        <Show when={item.isShuffle}>
          <span class="qc-queue-shuffle-badge" title="auto-filled by shuffle" aria-label="auto-filled by shuffle">
            <Shuffle size={11} />
            <span class="qc-queue-shuffle-label">shuffle</span>
          </span>
        </Show>
        <span class="qc-queue-airtime" title="estimated air time">
          {control.queueAirTimes()[index()] ?? ''}
        </span>
        <ActionMenu
          title={item.title}
          items={() => [
            {
              label: 'Move to top',
              icon: <ChevronsUp size={16} />,
              disabled: index() === 0,
              onSelect: () => void control.moveQueueItem(item.id, 'top'),
            },
            {
              label: 'Move to bottom',
              icon: <ChevronsDown size={16} />,
              disabled: index() === queueLength() - 1,
              onSelect: () => void control.moveQueueItem(item.id, 'bottom'),
            },
            {
              label: 'Remove from queue',
              icon: <X size={16} />,
              danger: true,
              onSelect: () => void control.removeFromQueue(item.id),
            },
          ]}
        />
      </li>
    )
  }

  return (
    <div class="qc-column-right">
      <div class="qc-column-header">
        <div class="qc-column-title-group">
          <p class="qc-column-eyebrow">Broadcast order</p>
          <h2>Up next</h2>
        </div>
        <div class="qc-column-header-actions">
          <Show when={queueLength() > 0}>
            <button class="qc-clear-btn" onClick={() => { setSavingQueue(!savingQueue()); setNewPlaylistName('') }}>
              Save Set
            </button>
            <button
              class="qc-clear-btn"
              title="clear the queue"
              onClick={() => {
                if (queueLength() < 3 || confirm(`Clear all ${queueLength()} queued tracks?`)) {
                  void control.clearTheQueue()
                }
              }}
            >
              <Trash2 size={14} />
              Clear
            </button>
          </Show>
          <Show when={control.clearedSongIds().length > 0}>
            <button class="qc-clear-btn" title="put the cleared tracks back" onClick={() => void control.undoClearQueue()}>
              <Undo2 size={14} />
              Undo clear ({control.clearedSongIds().length})
            </button>
          </Show>
        </div>
      </div>

      <div class="qc-queue-toolbar">
        <button
          class="pill-button subtle"
          type="button"
          disabled={queueLength() < 2}
          title="randomize the pending order"
          onClick={() => void control.shuffleQueueOrder()}
        >
          <Shuffle size={14} /> shuffle order
        </button>
        <button
          class="pill-button subtle"
          type="button"
          aria-pressed={control.loopMode() !== 'off'}
          classList={{ 'is-active': control.loopMode() !== 'off' }}
          title={LOOP_TITLES[control.loopMode()]}
          onClick={() => void control.applyLoopMode(NEXT_LOOP_MODE[control.loopMode()])}
        >
          <Show when={control.loopMode() === 'one'} fallback={<Repeat size={14} />}>
            <Repeat1 size={14} />
          </Show>
          {LOOP_LABELS[control.loopMode()]}
        </button>
        <Show when={loopingSet()}>
          {(set) => (
            <button
              class="pill-button subtle is-active"
              type="button"
              title="stop reloading this set when the queue drains"
              onClick={() => void control.applyLoopPlaylist(null)}
            >
              <Repeat size={14} /> looping “{set().name}”
            </button>
          )}
        </Show>
      </div>

      <Show when={savingQueue()}>
        <div class="playlist-save-form">
          <input
            type="text"
            class="qc-save-set-input"
            placeholder="name your set"
            value={newPlaylistName()}
            onInput={(event) => setNewPlaylistName(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter') void saveQueueAsPlaylist()
            }}
          />
          <button class="pill-button" type="button" disabled={!newPlaylistName().trim()} onClick={() => void saveQueueAsPlaylist()}>
            save
          </button>
        </div>
      </Show>

      <span class="qc-column-stats qc-queue-stats">
        {queueLength()} tracks • about {control.queueDurationMin()} min
      </span>

      {/* Scrolls rather than paginates: an eight-row page made it impossible to
          drag a track past the page boundary, which is most of what reordering is. */}
      <Show when={!control.snapshot.loading} fallback={<p class="list-empty">loading queue...</p>}>
        <ul class="qc-queue-list" ref={queueListEl}>
          <For each={queue()} fallback={<li class="list-empty">queue is empty — the station plays from rotation</li>}>
            {renderQueueItem}
          </For>
        </ul>
      </Show>

      <div class="qc-est-end-row">
        <span class="qc-est-label">
          <Clock size={16} style="margin-right: 6px;" />
          Estimated end
        </span>
        <span class="qc-est-time">{control.estimatedEndTime()}</span>
      </div>

      <div class="qc-after-queue-row" classList={{ 'is-silent': !control.afterQueueLabel() }}>
        <span class="qc-est-label">After queue</span>
        <span class="qc-after-queue-value">
          {control.afterQueueLabel() ?? '⚠ silence — nothing in rotation'}
        </span>
      </div>

      <Show when={control.rotationInfo()?.upNext}>
        {(next) => (
          <div class="qc-after-queue-row">
            <span class="qc-est-label">Next from rotation</span>
            <span class="qc-after-queue-value" title={`from ${next().source}`}>
              {next().title} — {next().artist}
            </span>
          </div>
        )}
      </Show>

      <Show when={(control.rotationInfo()?.recentlyPlayed?.length ?? 0) > 0}>
        <details class="qc-recently-played">
          <summary>recently played</summary>
          <ul>
            <For each={control.rotationInfo()?.recentlyPlayed ?? []}>
              {(entry) => (
                <li>
                  <span class="qc-recent-time">{new Date(entry.startedAt * 1000).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })}</span>
                  <span class="qc-recent-title">{entry.title}</span>
                  <span class="qc-recent-artist">{entry.artist}</span>
                </li>
              )}
            </For>
          </ul>
        </details>
      </Show>
    </div>
  )
}
