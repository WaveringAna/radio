import { createEffect, createSignal, For, onCleanup, Show } from 'solid-js'
import { createStore } from 'solid-js/store'
import {
  importFromSubsonic,
  importFromSubsonicShare,
  loadSubsonicCreds,
  saveSubsonicCreds,
  searchSubsonic,
  type SubsonicSongResult,
} from '../../shared/lib/radio'

interface SubsonicPaneProps {
  onSongAdded: () => void
  onError: (message: string | null) => void
}

export function SubsonicPane(props: SubsonicPaneProps) {
  const savedCreds = loadSubsonicCreds()
  const [creds, setCreds] = createStore({
    serverUrl: savedCreds.serverUrl ?? '',
    username: savedCreds.username ?? '',
    password: savedCreds.password ?? '',
  })
  const [query, setQuery] = createSignal('')
  const [results, setResults] = createSignal<SubsonicSongResult[]>([])
  const [searching, setSearching] = createSignal(false)
  const [addToQueue, setAddToQueue] = createSignal(true)
  const [importingId, setImportingId] = createSignal<string | null>(null)
  const [shareUrl, setShareUrl] = createSignal('')
  const [importingShare, setImportingShare] = createSignal(false)

  const importShare = async () => {
    const url = shareUrl().trim()
    if (!url) return
    setImportingShare(true)
    try {
      props.onError(null)
      await importFromSubsonicShare(url, addToQueue())
      setShareUrl('')
      props.onSongAdded()
    } catch (error) {
      props.onError(error instanceof Error ? error.message : 'share import failed.')
    } finally {
      setImportingShare(false)
    }
  }

  createEffect(() => {
    saveSubsonicCreds({ serverUrl: creds.serverUrl, username: creds.username, password: creds.password })
  })

  createEffect(() => {
    const q = query()
    if (!q.trim()) {
      setResults([])
      return
    }
    const timer = setTimeout(() => {
      setSearching(true)
      void searchSubsonic({ serverUrl: creds.serverUrl, username: creds.username, password: creds.password }, q)
        .then(setResults)
        .catch(() => setResults([]))
        .finally(() => setSearching(false))
    }, 500)
    onCleanup(() => clearTimeout(timer))
  })

  const importSong = async (result: SubsonicSongResult) => {
    setImportingId(result.id)
    try {
      props.onError(null)
      await importFromSubsonic(
        { serverUrl: creds.serverUrl, username: creds.username, password: creds.password },
        result.id,
        result.coverArtId,
        addToQueue(),
      )
      props.onSongAdded()
    } catch (error) {
      props.onError(error instanceof Error ? error.message : 'import failed.')
    } finally {
      setImportingId(null)
    }
  }

  return (
    <div class="upload-form">
      <input
        type="url"
        placeholder="paste share link (e.g. https://server/share/abc123)"
        value={shareUrl()}
        onInput={(e) => setShareUrl(e.currentTarget.value)}
        onKeyDown={(e) => { if (e.key === 'Enter') { e.preventDefault(); void importShare() } }}
      />
      <button
        class="pill-button"
        type="button"
        disabled={importingShare() || !shareUrl().trim()}
        onClick={() => void importShare()}
      >
        {importingShare() ? 'importing...' : 'import from share link'}
      </button>
      <hr class="subsonic-divider" />
      <input
        type="url"
        placeholder="server url"
        value={creds.serverUrl}
        onInput={(e) => setCreds('serverUrl', e.currentTarget.value)}
      />
      <input
        placeholder="username"
        value={creds.username}
        onInput={(e) => setCreds('username', e.currentTarget.value)}
      />
      <input
        type="password"
        placeholder="password"
        value={creds.password}
        onInput={(e) => setCreds('password', e.currentTarget.value)}
      />
      <hr class="subsonic-divider" />
      <input
        placeholder="search songs..."
        value={query()}
        onInput={(e) => setQuery(e.currentTarget.value)}
      />
      <label class="inline-check">
        <input type="checkbox" checked={addToQueue()} onChange={(e) => setAddToQueue(e.currentTarget.checked)} />
        add to queue
      </label>
      <Show when={searching()}>
        <p class="subsonic-searching">searching...</p>
      </Show>
      <Show when={results().length > 0}>
        <div class="subsonic-results">
          <ul class="song-list">
            <For each={results()}>
              {(result) => (
                <li>
                  <div class="song-copy">
                    <span>{result.title}</span>
                    <small>{result.artist}{result.album ? ` · ${result.album}` : ''}</small>
                  </div>
                  <button
                    class="pill-button subtle"
                    type="button"
                    disabled={importingId() === result.id}
                    onClick={() => void importSong(result)}
                  >
                    {importingId() === result.id ? '...' : 'import'}
                  </button>
                </li>
              )}
            </For>
          </ul>
        </div>
      </Show>
    </div>
  )
}
