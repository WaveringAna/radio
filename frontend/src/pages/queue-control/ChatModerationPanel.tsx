import { createEffect, createResource, createSignal, For, onCleanup, Show } from 'solid-js'
import { Ban, Trash2 } from 'lucide-solid'
import { ProfileAvatar } from '../../shared/components/ProfileAvatar'
import { resolveAtprotoProfile, type AtprotoProfile } from '../../shared/lib/atproto'
import {
  createChatBan,
  deleteChatMessage,
  fetchChatBans,
  openChatSocket,
  removeChatBan,
  type ChatBan,
  type ChatEvent,
  type ChatMessage,
} from '../../shared/lib/radio'

function fallbackProfile(did: string): AtprotoProfile {
  return { did, handle: did }
}

function formatTime(unixSeconds: number): string {
  return new Date(unixSeconds * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
}

/**
 * Admin moderation cockpit: deletes chat messages and manages the ban list.
 * @returns The chat moderation panel view.
 */
export function ChatModerationPanel() {
  const [messages, setMessages] = createSignal<ChatMessage[]>([])
  const [connected, setConnected] = createSignal(false)
  const [bans, { refetch: refetchBans }] = createResource(fetchChatBans, { initialValue: [] })
  const [banInputDid, setBanInputDid] = createSignal('')
  const [banReason, setBanReason] = createSignal('')
  const [actionError, setActionError] = createSignal<string | null>(null)
  const [profiles, setProfiles] = createSignal<Record<string, AtprotoProfile>>({})
  const inFlightDids = new Set<string>()

  const profileFor = (did: string) => profiles()[did] ?? fallbackProfile(did)

  createEffect(() => {
    let socket: WebSocket | null = null
    let reconnectTimer: number | null = null
    let reconnectAttempt = 0
    let cancelled = false

    const connect = () => {
      if (cancelled) return
      socket = openChatSocket()

      socket.addEventListener('open', () => {
        reconnectAttempt = 0
        setConnected(true)
      })

      socket.addEventListener('message', (event) => {
        const data = JSON.parse(event.data) as ChatEvent
        if (data.type === 'history') {
          setMessages(data.messages)
        } else if (data.type === 'message') {
          setMessages((current) => [...current, data.message])
        } else if (data.type === 'messageDeleted') {
          setMessages((current) => current.filter((entry) => entry.id !== data.id))
        } else if (data.type === 'messagesPurged') {
          setMessages((current) => current.filter((entry) => entry.senderDid !== data.senderDid))
        }
      })

      const scheduleReconnect = () => {
        setConnected(false)
        if (cancelled || reconnectTimer !== null) return
        const delay = Math.min(30000, 500 * 2 ** Math.min(reconnectAttempt, 6))
        reconnectAttempt += 1
        reconnectTimer = window.setTimeout(() => {
          reconnectTimer = null
          connect()
        }, delay)
      }

      socket.addEventListener('close', scheduleReconnect)
      socket.addEventListener('error', () => socket?.close())
    }

    connect()

    onCleanup(() => {
      cancelled = true
      if (reconnectTimer !== null) window.clearTimeout(reconnectTimer)
      socket?.close()
    })
  })

  const resolveDids = (dids: string[]) => {
    const fresh = dids.filter(
      (did, index, values) => values.indexOf(did) === index && !profiles()[did] && !inFlightDids.has(did),
    )
    for (const did of fresh) {
      inFlightDids.add(did)
      void resolveAtprotoProfile(did)
        .then((profile) => setProfiles((current) => ({ ...current, [did]: profile })))
        .finally(() => inFlightDids.delete(did))
    }
  }

  createEffect(() => {
    resolveDids(messages().filter((message) => message.kind === 'user').map((message) => message.senderDid))
  })

  createEffect(() => {
    // Reading `bans()` throws if the resource is in error state, so guard it
    // so a ban-fetch failure can't cancel message-sender resolution.
    let banList: ChatBan[] = []
    try {
      banList = bans()
    } catch (error) {
      console.warn('failed to read chat bans for profile resolution', error)
      return
    }
    resolveDids([
      ...banList.map((ban) => ban.did),
      ...banList.map((ban) => ban.bannedByDid),
    ])
  })

  const removeMessage = async (id: string) => {
    setActionError(null)
    try {
      await deleteChatMessage(id)
    } catch (error) {
      setActionError(error instanceof Error ? error.message : 'failed to delete message')
    }
  }

  const banSender = async (did: string, reason?: string) => {
    setActionError(null)
    try {
      await createChatBan(did, reason)
      void refetchBans()
    } catch (error) {
      setActionError(error instanceof Error ? error.message : 'failed to ban did')
    }
  }

  const submitBanForm = async (event: SubmitEvent) => {
    event.preventDefault()
    const did = banInputDid().trim()
    if (!did) return
    await banSender(did, banReason().trim() || undefined)
    setBanInputDid('')
    setBanReason('')
  }

  const unban = async (did: string) => {
    setActionError(null)
    try {
      await removeChatBan(did)
      void refetchBans()
    } catch (error) {
      setActionError(error instanceof Error ? error.message : 'failed to remove ban')
    }
  }

  const visibleUserMessages = () =>
    messages()
      .filter((message) => message.kind === 'user')
      .slice()
      .reverse()

  return (
    <section class="chat-moderation-card">
      <div class="section-heading">
        <p class="eyebrow">chat moderation</p>
        <span>{connected() ? 'live' : 'offline'}</span>
      </div>

      <Show when={actionError()}>
        {(message) => <p class="error-copy chat-moderation-error">{message()}</p>}
      </Show>

      <div class="chat-moderation-section">
        <h3 class="chat-moderation-subtitle">recent messages</h3>
        <ul class="chat-moderation-list">
          <For
            each={visibleUserMessages()}
            fallback={<li class="list-empty">no messages yet</li>}
          >
            {(message) => {
              const profile = () => profileFor(message.senderDid)
              return (
                <li>
                  <ProfileAvatar profile={profile()} />
                  <div class="chat-moderation-copy">
                    <div class="chat-moderation-meta">
                      <span class="chat-moderation-handle">@{profile().handle}</span>
                      <span class="chat-moderation-time">{formatTime(message.createdAt)}</span>
                    </div>
                    <p class="chat-moderation-body">{message.body}</p>
                  </div>
                  <div class="chat-moderation-actions">
                    <button
                      class="icon-button"
                      type="button"
                      aria-label="delete message"
                      title="delete message"
                      onClick={() => void removeMessage(message.id)}
                    >
                      <Trash2 size={16} />
                    </button>
                    <button
                      class="icon-button"
                      type="button"
                      aria-label="ban sender"
                      title="ban sender"
                      onClick={() => void banSender(message.senderDid)}
                    >
                      <Ban size={16} />
                    </button>
                  </div>
                </li>
              )
            }}
          </For>
        </ul>
      </div>

      <div class="chat-moderation-section">
        <h3 class="chat-moderation-subtitle">banned dids · {bans().length}</h3>
        <form class="chat-moderation-ban-form" onSubmit={submitBanForm}>
          <input
            placeholder="did:plc:…"
            value={banInputDid()}
            onInput={(event) => setBanInputDid(event.currentTarget.value)}
          />
          <input
            placeholder="reason (optional)"
            value={banReason()}
            onInput={(event) => setBanReason(event.currentTarget.value)}
          />
          <button class="pill-button" type="submit" disabled={banInputDid().trim().length === 0}>
            ban
          </button>
        </form>
        <ul class="chat-moderation-list">
          <For each={bans()} fallback={<li class="list-empty">no bans</li>}>
            {(ban: ChatBan) => {
              const profile = () => profileFor(ban.did)
              return (
                <li>
                  <ProfileAvatar profile={profile()} />
                  <div class="chat-moderation-copy">
                    <div class="chat-moderation-meta">
                      <span class="chat-moderation-handle">@{profile().handle}</span>
                      <span class="chat-moderation-time">{formatTime(ban.createdAt)}</span>
                    </div>
                    <Show when={ban.reason}>
                      <p class="chat-moderation-body chat-moderation-reason">{ban.reason}</p>
                    </Show>
                  </div>
                  <button
                    class="pill-button subtle"
                    type="button"
                    onClick={() => void unban(ban.did)}
                  >
                    unban
                  </button>
                </li>
              )
            }}
          </For>
        </ul>
      </div>
    </section>
  )
}
