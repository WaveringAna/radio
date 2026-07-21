import { Show } from 'solid-js'
import { AccessGate } from './AccessGate'
import { ChatModerationPanel } from './ChatModerationPanel'
import { LibraryPanel } from './LibraryPanel'
import { NowPlayingBar } from './NowPlayingBar'
import { QueuePanel } from './QueuePanel'
import { createQueueControl, type QueueControlProps } from './useQueueControl'
import { labelFromStationUrl } from '../../shared/lib/stationSelection'

/**
 * Renders the admin queue cockpit with playback, queue, upload, and library search tools.
 * @param props Current viewer permissions.
 * @returns The queue-control admin page view.
 */
export default function QueueControlPage(props: QueueControlProps) {
  const control = createQueueControl(props)

  return (
    <section class="queue-control-page">
      <Show when={control.isAdmin()} fallback={<AccessGate gate={control.queueControlGate} />}>
        <Show when={control.pageError()}>{(message) => <p class="error-copy queue-control-error">{message()}</p>}</Show>

        <header class="qc-station-bar-new">
          <div class="qc-station-identity-new">
            <strong>{control.selectedStation().name}</strong>
            <span title={control.selectedStation().url}>{labelFromStationUrl(control.selectedStation().url)}</span>
          </div>
          <span
            class="qc-station-state-new"
            classList={{ 'is-playing': control.snapshot()?.state.status === 'playing' }}
          >
            {control.snapshot()?.state.status ?? 'connecting'}
          </span>
        </header>

        <div class="qc-split-new">
          <NowPlayingBar control={control} />

          <div class="qc-columns-container">
            <LibraryPanel control={control} />
            <QueuePanel control={control} />
          </div>

          <ChatModerationPanel
            apiBase={control.selectedApiBase()}
            stationKey={control.selectedStationKey()}
            target={control.selectedRadioTarget()}
          />
        </div>
      </Show>
    </section>
  )
}
