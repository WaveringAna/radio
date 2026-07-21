import { CornerUpRight, ListPlus, Plus, Shuffle } from 'lucide-solid'
import type { ActionMenuItem } from './ActionMenu'
import type { QueueControl } from './useQueueControl'

/**
 * The queue verbs every library row offers, in one place so a song, an album,
 * an artist, and a set all read the same way in their menus.
 *
 * `queuedCount` is only meaningful for single tracks; pass it to surface the
 * "already queued" hint instead of letting a double-add pass silently.
 */
export function queueMenuItems(
  control: QueueControl,
  songIds: () => string[],
  options: { queuedCount?: () => number; onSelect?: () => void; selectLabel?: string } = {},
): ActionMenuItem[] {
  const ids = songIds()
  const count = options.queuedCount?.() ?? 0
  const items: ActionMenuItem[] = [
    {
      label: 'Play next',
      icon: <CornerUpRight size={16} />,
      disabled: ids.length === 0,
      onSelect: () => void control.addAlbumToQueue(ids, true),
    },
    {
      label: 'Add to queue',
      icon: <Plus size={16} />,
      hint: count > 0 ? `already queued ${count}×` : undefined,
      disabled: ids.length === 0,
      onSelect: () => void control.addAlbumToQueue(ids),
    },
  ]

  if (ids.length > 1) {
    items.push({
      label: 'Shuffle into queue',
      icon: <Shuffle size={16} />,
      hint: 'sequenced by artist, energy & tempo',
      onSelect: () => void control.addAlbumToQueue(ids, false, true),
    })
  }

  if (options.onSelect) {
    items.push({
      label: options.selectLabel ?? 'Select',
      icon: <ListPlus size={16} />,
      onSelect: options.onSelect,
    })
  }

  return items
}
