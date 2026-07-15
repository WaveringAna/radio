import { createResource, createSignal, onCleanup, Show } from 'solid-js'
import { CircleUserRound, ListMusic, LogIn, LogOut, Radio, ShieldCheck } from 'lucide-solid'
import { getListenerOptOut, setListenerOptOut } from './shared/lib/radio'
import AdminPage from './pages/admin/AdminPage'
import QueueControlPage from './pages/queue-control/QueueControlPage'
import RadioPage from './pages/radio/RadioPage'
import './App.css'
import {
  fetchSession,
  formatAuthError,
  readAuthError,
  signOut,
  startSignIn,
} from './shared/lib/auth'

function currentPath(): string {
  return window.location.pathname
}

/**
 * Root single-page app shell.
 * @returns The routed app view.
 */
export default function App() {
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

  const beginSignIn = async (event: SubmitEvent) => {
    event.preventDefault()

    try {
      setLocalError(null)
      await startSignIn(input())
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
    <form class="account-sign-in" onSubmit={beginSignIn}>
      <label class="account-field">
        <span>handle or did</span>
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
      <button class="account-primary-action" type="submit">
        <LogIn size={16} strokeWidth={1.9} aria-hidden="true" />
        <span>sign in</span>
      </button>
    </form>
  )

  return (
    <main>
      <nav class="top-nav" aria-label="primary navigation">
        <a href="/" onClick={navigate('/')} aria-current={path() === '/' ? 'page' : undefined}>
          <Radio size={15} strokeWidth={1.9} aria-hidden="true" />
          <span>radio</span>
        </a>
        <a
          href="/queue-control"
          aria-label="queue control"
          onClick={navigate('/queue-control')}
          aria-current={path() === '/queue-control' ? 'page' : undefined}
        >
          <ListMusic size={15} strokeWidth={1.9} aria-hidden="true" />
          <span class="nav-label-full">queue control</span>
          <span class="nav-label-short" aria-hidden="true">queue</span>
        </a>
        <a href="/auth" onClick={navigate('/auth')} aria-current={path() === '/auth' || path() === '/admin' ? 'page' : undefined}>
          <CircleUserRound size={15} strokeWidth={1.9} aria-hidden="true" />
          <span>auth</span>
        </a>
      </nav>

      <Show when={path() === '/'}>
        <section>
          <RadioPage session={session()} />
        </section>
      </Show>

      <Show when={path() === '/queue-control'}>
        <section>
          <QueueControlPage session={session()} sessionLoading={session.loading} />
        </section>
      </Show>

      <Show when={path() === '/auth' || path() === '/admin'}>
        <section class="account-page">
          <Show when={!session.loading} fallback={<p>checking session...</p>}>
            <header class="account-header">
              <p class="eyebrow">bluesky oauth</p>
              <h1>{session()?.authenticated ? 'account connected' : 'sign in'}</h1>
            </header>
            <Show when={session()?.authenticated} fallback={signInForm()}>
              <div class="account-session">
                <span class="account-session-icon" aria-hidden="true">
                  <CircleUserRound size={20} strokeWidth={1.8} />
                </span>
                <div class="account-session-copy">
                  <span>connected account</span>
                  <code title={session()?.accountDid ?? undefined}>{session()?.accountDid}</code>
                </div>
                <button class="account-sign-out" type="button" onClick={handleSignOut}>
                  <LogOut size={16} strokeWidth={1.9} aria-hidden="true" />
                  <span>sign out</span>
                </button>
              </div>
              <label class="listener-opt-out">
                <span class="listener-opt-out-copy">
                  <strong>listener visibility</strong>
                  <small>hide my profile from the live listener count</small>
                </span>
                <span class="listener-opt-out-switch">
                  <input
                    type="checkbox"
                    role="switch"
                    checked={listenerOptOut()}
                    onChange={(event) => toggleListenerOptOut(event.currentTarget.checked)}
                  />
                  <span aria-hidden="true" />
                </span>
              </label>
              <Show when={session()?.isAdmin}>
                <div class="account-admin-heading">
                  <ShieldCheck size={17} strokeWidth={1.8} aria-hidden="true" />
                  <span>station admin</span>
                </div>
                <AdminPage accountDid={session()?.accountDid ?? ''} isAdmin={session()?.isAdmin ?? false} />
              </Show>
            </Show>
          </Show>
        </section>
      </Show>

      <Show when={authError()}>
        {(message) => <p>{formatAuthError(message())}</p>}
      </Show>
    </main>
  )
}
