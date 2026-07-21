import { Show } from 'solid-js'
import { Pause, Play, Shuffle, SkipForward } from 'lucide-solid'
import { songCoverUrl } from '../../shared/lib/radio'
import { formatTime } from './helpers'
import type { QueueControl } from './useQueueControl'

/** The now-playing banner: cover art, title, live progress bar, and transport controls. */
export function NowPlayingBar(props: { control: QueueControl }) {
  const control = props.control
  const currentSong = () => control.snapshot()?.currentSong

  return (
    <section class="qc-now-new">
      <div class="qc-art-new">
        <Show
          when={currentSong()?.hasCover}
          fallback={<div class="qc-art-glow-new" aria-hidden="true">OPEN ROAD</div>}
        >
          <img class="qc-art-cover-new" src={songCoverUrl(currentSong()?.id ?? '', control.selectedApiBase())} alt="" />
        </Show>
      </div>

      <div class="qc-now-details">
        <div class="qc-now-eyebrow">
          <span>NOW PLAYING</span>
        </div>
        <Show
          when={currentSong()}
          fallback={
            <>
              <h1 class="qc-now-title qc-now-title-empty">Nothing playing yet</h1>
              <p class="qc-now-meta">Silence • Dark Room</p>
            </>
          }
        >
          {(song) => (
            <>
              <h1 class="qc-now-title" title={song().title}>{song().title}</h1>
              <p class="qc-now-meta">{song().artist} • {song().album || 'Single'}</p>

              <div class="qc-now-progress-container">
                <div
                  class="qc-now-progress-track"
                  role="progressbar"
                  aria-label="song progress"
                  aria-valuemin="0"
                  aria-valuemax="100"
                  aria-valuenow={Math.round(control.liveProgressPercent())}
                >
                  <span style={`width: ${control.liveProgressPercent()}%`} />
                </div>
                <div class="qc-now-time-row">
                  <span>{formatTime(Math.min(control.livePositionSeconds(), song().durationSeconds ?? Infinity))}</span>
                  <span>{formatTime(song().durationSeconds)}</span>
                </div>
              </div>
            </>
          )}
        </Show>
      </div>

      <div class="qc-now-controls">
        <div class="qc-transport-panel-new" aria-label="radio transport controls">
          <Show
            when={control.snapshot()?.state.status === 'playing'}
            fallback={
              <button class="qc-control-btn play-circle-btn" type="button" aria-label="play" title="play" onClick={() => void control.sendControl('play')}>
                <Play size={20} fill="black" stroke="black" />
              </button>
            }
          >
            <button class="qc-control-btn play-circle-btn" type="button" aria-label="pause" title="pause" onClick={() => void control.sendControl('pause')}>
              <Pause size={20} fill="black" stroke="black" />
            </button>
          </Show>
          <button class="qc-control-btn" type="button" aria-label="skip" title="skip" onClick={() => void control.sendControl('skip')}>
            <SkipForward size={20} />
          </button>
          <button
            class="qc-control-btn"
            type="button"
            aria-label="shuffle all songs"
            aria-pressed={control.shuffleOn()}
            title={control.shuffleOn() ? 'shuffle: on (playing random songs)' : 'shuffle: off'}
            onClick={() => void control.sendControl('shuffle')}
          >
            <Shuffle size={20} />
          </button>
        </div>

        <button class="qc-end-broadcast-btn" type="button" onClick={() => void control.sendControl('stop')}>
          End broadcast
        </button>
      </div>
    </section>
  )
}
