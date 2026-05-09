import { createSignal, Show } from 'solid-js'
import { CloudUpload } from 'lucide-solid'
import { extractAudioMetadata, type ExtractedAudioMetadata } from '../lib/audioMetadata'
import { uploadSong } from '../lib/radio'

interface FileUploadFormProps {
  onSongAdded: () => void
  onError: (message: string | null) => void
}

function hasRequiredMetadata(metadata: ExtractedAudioMetadata | null): boolean {
  return Boolean(metadata?.title && metadata.artist)
}

export function FileUploadForm(props: FileUploadFormProps) {
  const [files, setFiles] = createSignal<File[]>([])
  const [metadata, setMetadata] = createSignal<ExtractedAudioMetadata | null>(null)
  const [title, setTitle] = createSignal('')
  const [artist, setArtist] = createSignal('')
  const [coverFile, setCoverFile] = createSignal<File | null>(null)
  const [addToQueue, setAddToQueue] = createSignal(true)
  const [uploadStatus, setUploadStatus] = createSignal<string | null>(null)
  const [isUploading, setIsUploading] = createSignal(false)
  const [isDropZoneActive, setIsDropZoneActive] = createSignal(false)

  const needsMetadataPrompt = () => files().length === 1 && !hasRequiredMetadata(metadata())

  const selectFiles = async (selectedFiles: File[]) => {
    setFiles(selectedFiles)
    setMetadata(null)
    setTitle('')
    setArtist('')
    props.onError(null)

    if (selectedFiles.length !== 1) {
      return
    }

    try {
      const extracted = await extractAudioMetadata(selectedFiles[0])
      setMetadata(extracted)
      setTitle(extracted.title ?? '')
      setArtist(extracted.artist ?? '')
    } catch {
      setMetadata({})
    }
  }

  const submitUpload = async (event: SubmitEvent) => {
    event.preventDefault()
    const selectedFiles = files()

    if (selectedFiles.length === 0) {
      props.onError('pick audio files first.')
      return
    }

    setIsUploading(true)
    try {
      props.onError(null)
      for (const [index, selectedFile] of selectedFiles.entries()) {
        setUploadStatus(`uploading ${index + 1}/${selectedFiles.length}: ${selectedFile.name}`)

        let resolvedTitle: string
        let resolvedArtist: string
        let extracted: ExtractedAudioMetadata | null = null

        if (selectedFiles.length === 1) {
          extracted = metadata()
          resolvedTitle = metadata()?.title ?? title().trim()
          resolvedArtist = metadata()?.artist ?? artist().trim()
        } else {
          extracted = await extractAudioMetadata(selectedFile).catch(() => ({} as ExtractedAudioMetadata))
          resolvedTitle = extracted.title ?? selectedFile.name.replace(/\.[^/.]+$/, '')
          resolvedArtist = extracted.artist ?? ''
        }

        if (!resolvedTitle || !resolvedArtist) {
          throw new Error(`${selectedFile.name} is missing title or artist metadata.`)
        }

        await uploadSong({
          file: selectedFile,
          title: resolvedTitle,
          artist: resolvedArtist,
          album: extracted?.album,
          genre: extracted?.genre,
          durationSeconds: extracted?.durationSeconds,
          cover: coverFile(),
          addToQueue: addToQueue(),
        })
      }

      setTitle('')
      setArtist('')
      setMetadata(null)
      setFiles([])
      setCoverFile(null)
      props.onSongAdded()
    } catch (error) {
      props.onError(error instanceof Error ? error.message : 'upload exploded a little.')
    } finally {
      setUploadStatus(null)
      setIsUploading(false)
    }
  }

  return (
    <form class="upload-form" onSubmit={submitUpload}>
      <label
        class="drop-zone"
        classList={{ 'drop-zone-active': isDropZoneActive() }}
        onDragOver={(e) => { e.preventDefault(); setIsDropZoneActive(true) }}
        onDragLeave={() => setIsDropZoneActive(false)}
        onDrop={(e) => {
          e.preventDefault()
          setIsDropZoneActive(false)
          const dropped = [...(e.dataTransfer?.files ?? [])].filter((f) => f.type.startsWith('audio/'))
          if (dropped.length > 0) {
            void selectFiles(dropped)
          }
        }}
      >
        <CloudUpload size={24} />
        <span>
          {files().length === 0
            ? 'choose audio files or drop them here'
            : files().length === 1
              ? files()[0].name
              : `${files().length} files selected`}
        </span>
        <small class="drop-zone-hint">multiple files supported</small>
        <input type="file" accept="audio/*" multiple onChange={(event) => void selectFiles([...(event.currentTarget.files ?? [])])} />
      </label>

      <div class="upload-options-row">
        <label class="inline-file cover-picker">
          <span>cover image</span>
          <span class="file-button">choose cover</span>
          <input type="file" accept="image/*" onChange={(event) => setCoverFile(event.currentTarget.files?.[0] ?? null)} />
          <small>{coverFile()?.name ?? 'no cover selected'}</small>
        </label>

        <label class="inline-check">
          <input type="checkbox" checked={addToQueue()} onChange={(event) => setAddToQueue(event.currentTarget.checked)} />
          add to queue
        </label>
      </div>

      <Show when={needsMetadataPrompt()}>
        <div class="metadata-prompt">
          <p class="muted">no title/artist tags found. add the minimum so the queue is readable.</p>
          <input placeholder="title" value={title()} onInput={(event) => setTitle(event.currentTarget.value)} />
          <input placeholder="artist" value={artist()} onInput={(event) => setArtist(event.currentTarget.value)} />
        </div>
      </Show>

      <Show when={uploadStatus()}>
        {(status) => <small class="muted upload-status">{status()}</small>}
      </Show>

      <button class="pill-button" type="submit" disabled={isUploading()}>
        {isUploading() ? 'uploading…' : files().length > 1 ? `upload ${files().length} files` : 'upload'}
      </button>
    </form>
  )
}
