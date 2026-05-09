import { createSignal, Show } from 'solid-js'
import { Pause, Play, SkipForward } from 'lucide-solid'
import { FileUploadForm } from './FileUploadForm'
import { UrlUploadForm } from './UrlUploadForm'
import { SubsonicPane } from './SubsonicPane'

type UploadMode = 'file' | 'url' | 'subsonic'

interface AdminUploadPanelProps {
  onTransport: (action: 'play' | 'pause' | 'stop' | 'skip') => void
  onSongAdded: () => void
  error: string | null
  onError: (message: string | null) => void
}

export function AdminUploadPanel(props: AdminUploadPanelProps) {
  const [mode, setMode] = createSignal<UploadMode>('file')

  return (
    <section class="glass-card admin-controls">
      <div class="section-heading">
        <p class="eyebrow">admin control</p>
        <div class="transport-controls">
          <button class="icon-button primary" type="button" aria-label="play" onClick={() => props.onTransport('play')}>
            <Play size={20} fill="currentColor" />
          </button>
          <button class="icon-button" type="button" aria-label="pause" onClick={() => props.onTransport('pause')}>
            <Pause size={18} />
          </button>
          <button class="icon-button" type="button" aria-label="skip" onClick={() => props.onTransport('skip')}>
            <SkipForward size={18} />
          </button>
        </div>
      </div>

      <div class="upload-mode-tabs">
        <button class="pill-button" classList={{ subtle: mode() !== 'file' }} type="button" onClick={() => setMode('file')}>file</button>
        <button class="pill-button" classList={{ subtle: mode() !== 'url' }} type="button" onClick={() => setMode('url')}>url</button>
        <button class="pill-button" classList={{ subtle: mode() !== 'subsonic' }} type="button" onClick={() => setMode('subsonic')}>subsonic</button>
      </div>

      <Show when={mode() === 'file'}>
        <FileUploadForm onSongAdded={props.onSongAdded} onError={props.onError} />
      </Show>
      <Show when={mode() === 'url'}>
        <UrlUploadForm onSongAdded={props.onSongAdded} onError={props.onError} />
      </Show>
      <Show when={mode() === 'subsonic'}>
        <SubsonicPane onSongAdded={props.onSongAdded} onError={props.onError} />
      </Show>

      <Show when={props.error}>{(message) => <p class="error-copy">{message()}</p>}</Show>
    </section>
  )
}
