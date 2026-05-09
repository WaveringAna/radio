import { parseBlob } from 'music-metadata'

export interface ExtractedAudioMetadata {
  title?: string
  artist?: string
  album?: string
  durationSeconds?: number
}

/**
 * Extracts common audio metadata from a browser File.
 * @param file Audio file selected by an admin.
 * @returns Metadata discovered in the file tags and format header.
 */
export async function extractAudioMetadata(file: File): Promise<ExtractedAudioMetadata> {
  const metadata = await parseBlob(file)
  const title = metadata.common.title?.trim()
  const artist = metadata.common.artist?.trim()
  const album = metadata.common.album?.trim()
  const durationSeconds = metadata.format.duration
    ? Math.round(metadata.format.duration)
    : undefined

  return {
    title: title || undefined,
    artist: artist || undefined,
    album: album || undefined,
    durationSeconds,
  }
}
