import { API_BASE } from './radio'

/**
 * Auth session payload returned by the backend.
 */
export interface SessionResponse {
  /** Whether the current browser session is authenticated. */
  authenticated: boolean
  /** DID of the signed-in account when present. */
  accountDid?: string | null
  /** Whether the current account is in the admin DID whitelist. */
  isAdmin: boolean
}

/**
 * Admin permission exposed by the backend.
 */
export interface AdminPermission {
  /** Stable permission key used by backend feature gates. */
  key: string
  /** Human-readable permission description. */
  description: string
}

/**
 * Admin permission payload returned by the backend.
 */
export interface AdminPermissionsResponse {
  /** Admin DIDs allowed to manage privileged radio features. */
  whitelistedDids: string[]
  /** Permissions granted to whitelisted admin DIDs. */
  permissions: AdminPermission[]
}

/**
 * Loads the current auth session from the backend.
 * @returns The current auth session payload.
 * @throws Error When the backend session endpoint fails.
 */
export async function fetchSession(): Promise<SessionResponse> {
  const response = await fetch(`${API_BASE}/api/session`, {
    credentials: 'include',
  })

  if (!response.ok) {
    throw new Error('failed to load session state')
  }

  return (await response.json()) as SessionResponse
}

/**
 * Loads admin DID whitelist and placeholder radio permissions.
 * @returns The configured admin whitelist and available permissions.
 * @throws Error When the current session is not an admin or the request fails.
 */
export async function fetchAdminPermissions(): Promise<AdminPermissionsResponse> {
  const response = await fetch(`${API_BASE}/api/admin/permissions`, {
    credentials: 'include',
  })

  if (response.status === 401) {
    throw new Error('sign in before poking the admin panel.')
  }

  if (response.status === 403) {
    throw new Error('this did is not on the admin whitelist.')
  }

  if (!response.ok) {
    throw new Error('failed to load admin permissions')
  }

  return (await response.json()) as AdminPermissionsResponse
}

/**
 * Adds a DID to the persisted admin whitelist.
 * @param did DID to whitelist.
 * @returns Updated admin permissions.
 * @throws Error When the request fails.
 */
export async function addAdminDid(did: string): Promise<AdminPermissionsResponse> {
  const response = await fetch(`${API_BASE}/api/admin/dids`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    credentials: 'include',
    body: JSON.stringify({ did }),
  })

  if (!response.ok) {
    throw new Error('failed to add admin did')
  }

  return (await response.json()) as AdminPermissionsResponse
}

/**
 * Removes a DID from the persisted admin whitelist.
 * @param did DID to remove.
 * @returns Updated admin permissions.
 * @throws Error When the request fails.
 */
export async function removeAdminDid(did: string): Promise<AdminPermissionsResponse> {
  const response = await fetch(`${API_BASE}/api/admin/dids/${encodeURIComponent(did)}`, {
    method: 'DELETE',
    credentials: 'include',
  })

  if (!response.ok) {
    throw new Error('failed to remove admin did')
  }

  return (await response.json()) as AdminPermissionsResponse
}

/**
 * Starts the server-side OAuth flow by redirecting the browser.
 * @param input Handle or DID entered by the user.
 * @throws Error When the provided input is empty.
 */
export function startSignIn(input: string): void {
  const value = input.trim()
  if (!value) {
    throw new Error('please enter a handle or did first.')
  }

  window.location.assign(`${API_BASE}/api/oauth/start?input=${encodeURIComponent(value)}`)
}

/**
 * Deletes the current backend app session.
 * @returns A promise that resolves when logout succeeds.
 * @throws Error When the backend logout endpoint fails.
 */
export async function signOut(): Promise<void> {
  const response = await fetch(`${API_BASE}/api/logout`, {
    method: 'POST',
    credentials: 'include',
  })

  if (!response.ok) {
    throw new Error('logout fully faceplanted. try again?')
  }
}

/**
 * Reads the auth error code from the current location search string.
 * @param search Search string to inspect.
 * @returns The auth error code when present.
 */
export function readAuthError(search: string = window.location.search): string | null {
  return new URLSearchParams(search).get('error')
}

/**
 * Maps backend auth error codes into display text.
 * @param code Backend error code.
 * @returns Human-readable auth error copy.
 */
export function formatAuthError(code: string): string {
  switch (code) {
    case 'missing_input':
      return 'you need to enter a handle or did before starting oauth.'
    case 'missing_code':
      return 'the oauth callback came back without a code. rude.'
    case 'oauth_start_failed':
      return 'could not kick off the oauth flow. check the backend logs.'
    case 'oauth_callback_failed':
      return 'oauth callback failed. the backend probably has the real tea.'
    case 'session_create_failed':
      return 'oauth worked, but persisting the app session did not.'
    default:
      return code
  }
}
