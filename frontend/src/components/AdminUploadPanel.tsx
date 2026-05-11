import { createSignal, Show } from 'solid-js'
import { FileUploadForm } from './FileUploadForm'
import { UrlUploadForm } from './UrlUploadForm'
import { SubsonicPane } from './SubsonicPane'

type UploadMode = 'file' | 'url' | 'subsonic'

interface AdminUploadPanelProps {
  onSongAdded: () => void
  error: string | null
  onError: (message: string | null) => void
}

/**
 * Renders consolidated admin upload tools for files, urls, and subsonic imports.
 * @param props Upload callbacks and current error text.
 * @returns The upload panel view.
 */
export function AdminUploadPanel(props: AdminUploadPanelProps) {
  const [mode, setMode] = createSignal<UploadMode>('file')

  return (
    <section class="glass-card admin-controls">
      <div class="section-heading">
        <p class="eyebrow">upload flow</p>
        <span>library intake</span>
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
