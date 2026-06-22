import { createSignal, Show } from 'solid-js'
import { CloudUpload } from 'lucide-solid'
import { extractAudioMetadata, type ExtractedAudioMetadata } from '../../shared/lib/audioMetadata'
import { createAlbum, uploadSong } from '../../shared/lib/radio'

interface FileUploadFormProps {
  onSongAdded: () => void
  onError: (message: string | null) => void
}

type FileUploadKind = 'songs' | 'album'

interface PlaylistEntry {
  location: string
  title?: string
  artist?: string
}

interface UploadPlanItem {
  file: File
  playlistEntry?: PlaylistEntry
}

const PLAYLIST_EXTENSIONS = new Set(['m3u', 'm3u8', 'pls', 'xspf'])

function fileExtension(file: File): string | undefined {
  return file.name.split('.').pop()?.toLowerCase()
}

function isPlaylistFile(file: File): boolean {
  const extension = fileExtension(file)
  return Boolean(extension && PLAYLIST_EXTENSIONS.has(extension))
}

function isSelectableAudioFile(file: File): boolean {
  return file.type.startsWith('audio/') || isPlaylistFile(file)
}

function playlistBasename(location: string): string {
  const withoutQuery = location.split('?')[0]?.split('#')[0] ?? location
  return decodeURIComponent(withoutQuery.split('/').pop() ?? withoutQuery).trim()
}

function playlistLabelMetadata(label: string | undefined): Pick<PlaylistEntry, 'title' | 'artist'> {
  const trimmed = label?.trim()
  if (!trimmed) return {}
  const [maybeArtist, maybeTitle] = trimmed.split(' - ')
  if (maybeArtist && maybeTitle) {
    return { artist: maybeArtist.trim(), title: maybeTitle.trim() }
  }
  return { title: trimmed }
}

function parseM3u(text: string): PlaylistEntry[] {
  const entries: PlaylistEntry[] = []
  let label: string | undefined

  for (const rawLine of text.split(/\r?\n/)) {
    const line = rawLine.replace(/^\uFEFF/, '').trim()
    if (!line) continue
    if (line.startsWith('#EXTINF:')) {
      label = line.split(',', 2)[1]?.trim()
      continue
    }
    if (line.startsWith('#')) continue

    entries.push({ location: line, ...playlistLabelMetadata(label) })
    label = undefined
  }

  return entries
}

async function uploadPlanFromPlaylist(playlistFile: File, audioFiles: File[]): Promise<UploadPlanItem[]> {
  const entries = parseM3u(await playlistFile.text())
  if (entries.length === 0) {
    throw new Error('playlist has no tracks.')
  }

  const filesByName = new Map(audioFiles.map((file) => [file.name, file]))
  return entries.map((entry) => {
    const file = filesByName.get(playlistBasename(entry.location))
    if (!file) {
      throw new Error(`playlist references missing file: ${entry.location}`)
    }
    return { file, playlistEntry: entry }
  })
}

function hasRequiredMetadata(metadata: ExtractedAudioMetadata | null): boolean {
  return Boolean(metadata?.title && metadata.artist)
}

/**
 * Renders the file upload flow for loose songs or album-loop creation.
 * @param props Upload completion and error callbacks.
 * @returns The file upload form view.
 */
export function FileUploadForm(props: FileUploadFormProps) {
  const [uploadKind, setUploadKind] = createSignal<FileUploadKind>('songs')
  const [files, setFiles] = createSignal<File[]>([])
  const [metadata, setMetadata] = createSignal<ExtractedAudioMetadata | null>(null)
  const [title, setTitle] = createSignal('')
  const [artist, setArtist] = createSignal('')
  const [coverFile, setCoverFile] = createSignal<File | null>(null)
  const [albumTitle, setAlbumTitle] = createSignal('')
  const [albumArtist, setAlbumArtist] = createSignal('')
  const [addToQueue, setAddToQueue] = createSignal(true)
  const [uploadStatus, setUploadStatus] = createSignal<string | null>(null)
  const [isUploading, setIsUploading] = createSignal(false)
  const [isDropZoneActive, setIsDropZoneActive] = createSignal(false)

  const needsMetadataPrompt = () => uploadKind() === 'songs' && files().length === 1 && !hasRequiredMetadata(metadata())
  const needsAlbumFallbacks = () => uploadKind() === 'album'

  const selectFiles = async (selectedFiles: File[]) => {
    const selectableFiles = selectedFiles.filter(isSelectableAudioFile)
    props.onError(
      selectableFiles.length === selectedFiles.length
        ? null
        : 'skipped non-audio files.',
    )
    setFiles(selectableFiles)
    setMetadata(null)
    setTitle('')
    setArtist('')

    if (selectableFiles.length !== 1 || isPlaylistFile(selectableFiles[0])) {
      return
    }

    try {
      const extracted = await extractAudioMetadata(selectableFiles[0])
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
      const uploadedSongIds: string[] = []
      const albumName = albumTitle().trim()
      const albumArtistFallback = albumArtist().trim()
      let albumLoopTitle = albumName
      const playlistFiles = selectedFiles.filter(isPlaylistFile)
      if (playlistFiles.length > 1) {
        throw new Error('upload one playlist at a time.')
      }
      const uploadPlan: UploadPlanItem[] = playlistFiles[0]
        ? await uploadPlanFromPlaylist(playlistFiles[0], selectedFiles.filter((file) => !isPlaylistFile(file)))
        : selectedFiles.map((file) => ({ file }))

      for (const [index, planItem] of uploadPlan.entries()) {
        const selectedFile = planItem.file
        const playlistEntry = planItem.playlistEntry
        setUploadStatus(`uploading ${index + 1}/${uploadPlan.length}: ${selectedFile.name}`)

        let resolvedTitle: string
        let resolvedArtist: string
        let resolvedAlbum: string | undefined
        let extracted: ExtractedAudioMetadata | null = null

        const extractSafe = async () => {
          try {
            return await extractAudioMetadata(selectedFile)
          } catch (error) {
            console.warn(`[upload] metadata extraction failed for ${selectedFile.name}`, error)
            return {} as ExtractedAudioMetadata
          }
        }

        if (uploadKind() === 'album') {
          extracted = await extractSafe()
          resolvedTitle = extracted.title ?? playlistEntry?.title ?? ''
          resolvedArtist = extracted.artist ?? playlistEntry?.artist ?? albumArtistFallback
          resolvedAlbum = extracted.album ?? albumName
        } else if (uploadPlan.length === 1 && !playlistEntry) {
          extracted = metadata()
          resolvedTitle = metadata()?.title ?? title().trim()
          resolvedArtist = metadata()?.artist ?? artist().trim()
          resolvedAlbum = extracted?.album
        } else {
          extracted = await extractSafe()
          resolvedTitle = extracted.title ?? playlistEntry?.title ?? ''
          resolvedArtist = extracted.artist ?? playlistEntry?.artist ?? ''
          resolvedAlbum = extracted.album
        }

        if (uploadKind() === 'album' && !resolvedAlbum) {
          throw new Error('album uploads need an album title when files do not have album tags.')
        }
        if (uploadKind() === 'album' && !albumLoopTitle) {
          albumLoopTitle = resolvedAlbum ?? ''
        }

        const song = await uploadSong({
          file: selectedFile,
          title: resolvedTitle,
          artist: resolvedArtist,
          album: resolvedAlbum,
          genre: extracted?.genre,
          durationSeconds: extracted?.durationSeconds,
          cover: coverFile(),
          addToQueue: uploadKind() === 'songs' && addToQueue(),
        })
        uploadedSongIds.push(song.id)
      }

      setTitle('')
      setArtist('')
      setAlbumTitle('')
      setAlbumArtist('')
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
      <div class="upload-kind-tabs" role="tablist" aria-label="file upload kind">
        <button class="pill-button" classList={{ subtle: uploadKind() !== 'songs' }} type="button" role="tab" aria-selected={uploadKind() === 'songs'} onClick={() => setUploadKind('songs')}>
          songs
        </button>
        <button class="pill-button" classList={{ subtle: uploadKind() !== 'album' }} type="button" role="tab" aria-selected={uploadKind() === 'album'} onClick={() => setUploadKind('album')}>
          album loop
        </button>
      </div>

      <label
        class="drop-zone"
        classList={{ 'drop-zone-active': isDropZoneActive() }}
        onDragOver={(e) => { e.preventDefault(); setIsDropZoneActive(true) }}
        onDragLeave={() => setIsDropZoneActive(false)}
        onDrop={(e) => {
          e.preventDefault()
          setIsDropZoneActive(false)
          const dropped = [...(e.dataTransfer?.files ?? [])].filter(isSelectableAudioFile)
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
        <small class="drop-zone-hint">{uploadKind() === 'album' ? 'select tracks in album order' : 'multiple files supported'}</small>
        <input type="file" accept="audio/*,.m3u,.m3u8,.pls,.xspf" multiple onChange={(event) => void selectFiles([...(event.currentTarget.files ?? [])])} />
      </label>

      <div class="upload-options-row">
        <label class="inline-file cover-picker">
          <span>cover image</span>
          <span class="file-button">choose cover</span>
          <input type="file" accept="image/*" onChange={(event) => setCoverFile(event.currentTarget.files?.[0] ?? null)} />
          <small>{coverFile()?.name ?? 'no cover selected'}</small>
        </label>

        <Show when={uploadKind() === 'songs'}>
          <label class="inline-check">
            <input type="checkbox" checked={addToQueue()} onChange={(event) => setAddToQueue(event.currentTarget.checked)} />
            add to queue
          </label>
        </Show>
      </div>

      <Show when={needsMetadataPrompt()}>
        <div class="metadata-prompt">
          <p class="muted">no title/artist tags found. add the minimum so the queue is readable.</p>
          <input placeholder="title" value={title()} onInput={(event) => setTitle(event.currentTarget.value)} />
          <input placeholder="artist" value={artist()} onInput={(event) => setArtist(event.currentTarget.value)} />
        </div>
      </Show>

      <Show when={needsAlbumFallbacks()}>
        <div class="metadata-prompt album-upload-flow">
          <p class="muted">album mode uploads tracks, applies fallbacks when tags are missing, then creates the album loop.</p>
          <input placeholder="album title fallback" value={albumTitle()} onInput={(event) => setAlbumTitle(event.currentTarget.value)} />
          <input placeholder="artist fallback" value={albumArtist()} onInput={(event) => setAlbumArtist(event.currentTarget.value)} />
        </div>
      </Show>

      <Show when={uploadStatus()}>
        {(status) => <small class="muted upload-status">{status()}</small>}
      </Show>

      <button class="pill-button" type="submit" disabled={isUploading()}>
        {isUploading()
          ? 'uploading…'
          : uploadKind() === 'album'
            ? files().length > 0 ? `upload ${files().length} album tracks` : 'upload album tracks'
            : files().length > 1
              ? `upload ${files().length} files`
              : 'upload'}
      </button>
    </form>
  )
}
