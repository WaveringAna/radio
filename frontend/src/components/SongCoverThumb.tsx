import { Show } from 'solid-js'
import { API_BASE } from '../lib/radio'

export function SongCoverThumb(props: { songId: string; hasCover: boolean }) {
  return (
    <span class="song-cover-thumb" aria-hidden="true">
      <Show when={props.hasCover} fallback={<span class="song-cover-fallback">art</span>}>
        <img src={`${API_BASE}/api/songs/${props.songId}/cover/thumbnail`} alt="" loading="lazy" />
      </Show>
    </span>
  )
}
