import { createSignal, Show } from 'solid-js'
import { createStore } from 'solid-js/store'
import { uploadSongFromUrl } from '../lib/radio'

interface UrlUploadFormProps {
  onSongAdded: () => void
  onError: (message: string | null) => void
}

const isYtdlpUrl = (url: string) =>
  url.includes('youtube.com/') || url.includes('youtu.be/') ||
  url.includes('soundcloud.com/') || url.includes('bandcamp.com/') || url.includes('vimeo.com/')

export function UrlUploadForm(props: UrlUploadFormProps) {
  const [urlInput, setUrlInput] = createSignal('')
  const [fields, setFields] = createStore({ title: '', artist: '', album: '' })
  const [addToQueue, setAddToQueue] = createSignal(true)

  const submit = async (event: SubmitEvent) => {
    event.preventDefault()
    const url = urlInput().trim()
    const title = fields.title.trim()
    const artist = fields.artist.trim()

    if (!url) { props.onError('paste a url first.'); return }
    if (!isYtdlpUrl(url) && !title) { props.onError('title is required.'); return }
    if (!isYtdlpUrl(url) && !artist) { props.onError('artist is required.'); return }

    try {
      props.onError(null)
      await uploadSongFromUrl({
        url,
        title: title || undefined,
        artist: artist || undefined,
        album: fields.album.trim() || undefined,
        addToQueue: addToQueue(),
      })
      setUrlInput('')
      setFields({ title: '', artist: '', album: '' })
      props.onSongAdded()
    } catch (error) {
      props.onError(error instanceof Error ? error.message : 'url import exploded a little.')
    }
  }

  return (
    <form class="upload-form" onSubmit={submit}>
      <input
        type="url"
        placeholder="https://example.com/song.mp3 or youtube.com/watch?v=..."
        value={urlInput()}
        onInput={(e) => setUrlInput(e.currentTarget.value)}
      />
      <Show when={isYtdlpUrl(urlInput())}>
        <p class="subsonic-searching">youtube · title and artist auto-detected, or fill in below to override</p>
      </Show>
      <input
        placeholder={isYtdlpUrl(urlInput()) ? 'title (optional, auto-detected)' : 'title'}
        value={fields.title}
        onInput={(e) => setFields('title', e.currentTarget.value)}
      />
      <input
        placeholder={isYtdlpUrl(urlInput()) ? 'artist (optional, auto-detected)' : 'artist'}
        value={fields.artist}
        onInput={(e) => setFields('artist', e.currentTarget.value)}
      />
      <input
        placeholder="album (optional)"
        value={fields.album}
        onInput={(e) => setFields('album', e.currentTarget.value)}
      />
      <label class="inline-check">
        <input type="checkbox" checked={addToQueue()} onChange={(e) => setAddToQueue(e.currentTarget.checked)} />
        add to queue
      </label>
      <button class="pill-button" type="submit">import</button>
    </form>
  )
}
