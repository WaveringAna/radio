import { createResource, createSignal, onCleanup, Show } from 'solid-js'
import { getListenerOptOut, setListenerOptOut, setSessionToken } from './lib/radio'
import AdminPage from './pages/AdminPage'
import QueueControlPage from './pages/QueueControlPage'
import RadioPage from './pages/RadioPage'
import './App.css'
import {
  fetchSession,
  formatAuthError,
  readAuthError,
  signOut,
  startSignIn,
} from './lib/auth'

function currentPath(): string {
  return window.location.pathname
}

/**
 * Root single-page app shell.
 * @returns The routed app view.
 */
export default function App() {
  // Capture Bearer token delivered via ?token= after OAuth callback and store it.
  const urlParams = new URLSearchParams(window.location.search)
  const urlToken = urlParams.get('token')
  if (urlToken) {
    setSessionToken(urlToken)
    urlParams.delete('token')
    const remaining = urlParams.toString()
    window.history.replaceState({}, '', remaining ? `/?${remaining}` : '/')
  }

  const [input, setInput] = createSignal('')
  const [localError, setLocalError] = createSignal<string | null>(null)
  const [path, setPath] = createSignal(currentPath())
  const [session, { refetch }] = createResource(fetchSession)
  const [listenerOptOut, setListenerOptOutSignal] = createSignal(getListenerOptOut())

  const toggleListenerOptOut = (next: boolean) => {
    setListenerOptOutSignal(next)
    setListenerOptOut(next)
  }

  const updatePath = () => setPath(currentPath())
  window.addEventListener('popstate', updatePath)
  onCleanup(() => window.removeEventListener('popstate', updatePath))

  const navigate = (to: string) => (event: MouseEvent) => {
    event.preventDefault()
    window.history.pushState({}, '', to)
    setPath(to)
  }

  const authError = () => readAuthError() ?? localError()

  const beginSignIn = (event: SubmitEvent) => {
    event.preventDefault()

    try {
      setLocalError(null)
      startSignIn(input())
    } catch (error) {
      setLocalError(error instanceof Error ? error.message : 'sign-in exploded a little')
    }
  }

  const handleSignOut = async () => {
    setLocalError(null)

    try {
      await signOut()
      await refetch()
    } catch (error) {
      setLocalError(error instanceof Error ? error.message : 'logout fully faceplanted. try again?')
    }
  }

  const signInForm = () => (
    <form onSubmit={beginSignIn}>
      <label>
        handle or did
        <br />
        <input
          type="text"
          name="input"
          autocomplete="username"
          placeholder="ana.bsky.social"
          value={input()}
          onInput={(event) => {
            setLocalError(null)
            setInput(event.currentTarget.value)
          }}
        />
      </label>
      <div>
        <button type="submit">sign in</button>
      </div>
    </form>
  )

  return (
    <main>
      <nav class="top-nav">
        <a href="/" onClick={navigate('/')} aria-current={path() === '/' ? 'page' : undefined}>
          radio
        </a>
        <Show when={session()?.isAdmin}>
          <a href="/queue-control" onClick={navigate('/queue-control')} aria-current={path() === '/queue-control' ? 'page' : undefined}>
            queue control
          </a>
        </Show>
        <a href="/auth" onClick={navigate('/auth')} aria-current={path() === '/auth' || path() === '/admin' ? 'page' : undefined}>
          auth
        </a>
      </nav>

      <Show when={!session.loading} fallback={<p>checking session...</p>}>
        <section hidden={path() !== '/'}>
          <RadioPage />
        </section>

        <section hidden={path() !== '/queue-control'}>
          <QueueControlPage isAdmin={session()?.isAdmin ?? false} />
        </section>

        <section hidden={path() !== '/auth' && path() !== '/admin'}>
          <h1>sign in with bluesky oauth</h1>
          <Show when={session()?.authenticated} fallback={signInForm()}>
            <p>you are signed in.</p>
            <p>account did: {session()?.accountDid}</p>
            <Show when={session()?.isAdmin}>
              <AdminPage accountDid={session()?.accountDid ?? ''} isAdmin={session()?.isAdmin ?? false} />
            </Show>
            <button type="button" onClick={handleSignOut}>
              sign out
            </button>
          </Show>
          <label class="listener-opt-out">
            <input
              type="checkbox"
              checked={listenerOptOut()}
              onChange={(event) => toggleListenerOptOut(event.currentTarget.checked)}
            />
            <span>opt out of showing my profile in the listener counter</span>
          </label>
        </section>
      </Show>

      <Show when={authError()}>
        {(message) => <p>{formatAuthError(message())}</p>}
      </Show>
    </main>
  )
}
