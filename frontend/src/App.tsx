import { createResource, createSignal, onCleanup, Show } from 'solid-js'
import { Monitor, Moon, MoonStar } from 'lucide-solid'
import AdminPage from './AdminPage'
import RadioPage from './RadioPage'
import './App.css'
import {
  fetchSession,
  formatAuthError,
  readAuthError,
  signOut,
  startSignIn,
} from './auth'
import {
  applyThemePreference,
  nextThemePreference,
  readThemePreference,
  type ThemePreference,
} from './theme'

function currentPath(): string {
  return window.location.pathname
}

function ThemeIcon(props: { preference: () => ThemePreference }) {
  return (
    <Show
      when={props.preference() === 'system'}
      fallback={props.preference() === 'light' ? <Moon size={17} /> : <MoonStar size={17} />}
    >
      <Monitor size={17} />
    </Show>
  )
}

/**
 * Root single-page app shell.
 * @returns The routed app view.
 */
export default function App() {
  const [input, setInput] = createSignal('')
  const [localError, setLocalError] = createSignal<string | null>(null)
  const [path, setPath] = createSignal(currentPath())
  const [theme, setTheme] = createSignal(readThemePreference())
  const [session, { refetch }] = createResource(fetchSession)

  applyThemePreference(theme())

  const updatePath = () => setPath(currentPath())
  window.addEventListener('popstate', updatePath)
  onCleanup(() => window.removeEventListener('popstate', updatePath))

  const navigate = (to: string) => (event: MouseEvent) => {
    event.preventDefault()
    window.history.pushState({}, '', to)
    setPath(to)
  }

  const switchTheme = () => {
    const nextTheme = nextThemePreference(theme())
    setTheme(nextTheme)
    applyThemePreference(nextTheme)
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
        <a href="/auth" onClick={navigate('/auth')} aria-current={path() === '/auth' || path() === '/admin' ? 'page' : undefined}>
          auth
        </a>
        <button class="theme-switch" type="button" onClick={switchTheme} aria-label={`theme: ${theme()}`} title={`theme: ${theme()}`}>
          <ThemeIcon preference={theme} />
        </button>
      </nav>

      <Show when={!session.loading} fallback={<p>checking session...</p>}>
        <section hidden={path() === '/auth' || path() === '/admin'}>
          <RadioPage isAdmin={session()?.isAdmin ?? false} />
        </section>

        <Show when={path() === '/auth' || path() === '/admin'}>
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
        </Show>
      </Show>

      <Show when={authError()}>
        {(message) => <p>{formatAuthError(message())}</p>}
      </Show>
    </main>
  )
}
