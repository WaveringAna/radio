import { createEffect, createResource, createSignal, For, Show } from 'solid-js'
import { Plus, Trash2 } from 'lucide-solid'
import { addAdminDid, fetchAdminPermissions, removeAdminDid } from '../../shared/lib/auth'
import type { RadioTarget } from '../../shared/lib/radio'
import { resolveAtprotoProfile, type AtprotoProfile } from '../../shared/lib/atproto'

interface AdminPageProps {
  accountDid: string
  isAdmin: boolean
  target?: RadioTarget
}

function readableError(error: unknown): string {
  return error instanceof Error ? error.message : 'admin panel had a tiny meltdown.'
}

/**
 * Renders authentication and admin-whitelist management.
 * @param props Admin page auth context.
 * @returns The admin page view.
 */
export default function AdminPage(props: AdminPageProps) {
  const [newDid, setNewDid] = createSignal('')
  const [localError, setLocalError] = createSignal<string | null>(null)
  const [profiles, setProfiles] = createSignal<Record<string, AtprotoProfile>>({})
  const inFlightDids = new Set<string>()

  createEffect(() => {
    const dids = adminPermissions()?.whitelistedDids ?? []
    for (const did of dids) {
      if (profiles()[did] || inFlightDids.has(did)) continue
      inFlightDids.add(did)
      void resolveAtprotoProfile(did)
        .then((profile) => setProfiles((current) => ({ ...current, [did]: profile })))
        .finally(() => inFlightDids.delete(did))
    }
  })
  const [adminPermissions, { mutate: mutateAdmins }] = createResource(
    () => ({ isAdmin: props.isAdmin, target: props.target }),
    ({ isAdmin, target }) => (isAdmin ? fetchAdminPermissions(target) : undefined),
  )

  const addDid = async (event: SubmitEvent) => {
    event.preventDefault()
    const did = newDid().trim()
    if (!did) return

    try {
      setLocalError(null)
      mutateAdmins(await addAdminDid(did, props.target))
      setNewDid('')
    } catch (error) {
      setLocalError(readableError(error))
    }
  }

  const removeDid = async (did: string) => {
    try {
      setLocalError(null)
      mutateAdmins(await removeAdminDid(did, props.target))
    } catch (error) {
      setLocalError(readableError(error))
    }
  }

  return (
    <section class="admin-page auth-admin-page">
      <p>signed in as: {props.accountDid}</p>
      <Show when={props.isAdmin} fallback={<p>this did is not on the admin whitelist.</p>}>
        <Show when={!adminPermissions.loading} fallback={<p>loading admin permissions...</p>}>
          <Show when={!adminPermissions.error} fallback={<p>{readableError(adminPermissions.error)}</p>}>
            <section class="glass-card admin-card">
              <div class="section-heading">
                <p class="eyebrow">whitelisted admin dids</p>
                <span>{adminPermissions()?.whitelistedDids.length ?? 0}</span>
              </div>
              <form class="admin-inline-form" onSubmit={addDid}>
                <input placeholder="did:plc:..." value={newDid()} onInput={(event) => setNewDid(event.currentTarget.value)} />
                <button class="icon-button" type="submit" aria-label="add admin did">
                  <Plus size={18} />
                </button>
              </form>
              <ul class="song-list admin-whitelist-list">
                <For each={adminPermissions()?.whitelistedDids ?? []}>
                  {(did) => {
                    const profile = () => profiles()[did]
                    return (
                      <li>
                        <div class="song-copy">
                          <Show when={profile()} fallback={<span class="did-fallback">{did}</span>}>
                            {(p) => (
                              <div class="admin-profile-row">
                                <span class="profile-handle">@{p().handle}</span>
                                <span class="profile-did-sub">{did}</span>
                              </div>
                            )}
                          </Show>
                        </div>
                        <button class="icon-button" type="button" aria-label="remove admin did" onClick={() => void removeDid(did)}>
                          <Trash2 size={17} />
                        </button>
                      </li>
                    )
                  }}
                </For>
              </ul>
            </section>
          </Show>
        </Show>
      </Show>
      <Show when={localError()}>{(message) => <p class="error-copy">{message()}</p>}</Show>
    </section>
  )
}
