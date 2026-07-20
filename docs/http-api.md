# HTTP API Reference

This documents the regular HTTP API served by the radio backend. It excludes the
AT Protocol XRPC endpoints under `/xrpc/*`; see [`xrpc.md`](./xrpc.md) for those.

All paths are relative to the backend base URL, for example
`https://radio.example.test`. JSON request and response bodies use camelCase
field names. Timestamps are Unix seconds.

## Authentication

Public read/listen endpoints do not require authentication:

- `GET /api/health`
- `GET /api/session`
- `GET /api/radio/state`
- `GET /api/radio/seek`
- `GET /api/radio/ws`
- `GET /api/radio/chat/ws`
- `GET /api/songs`
- `GET /api/songs/{songId}/audio`
- `GET /api/songs/{songId}/cover`
- `GET /api/songs/{songId}/cover/thumbnail`

Admin endpoints require a signed-in admin DID. The backend accepts the session
token either as:

- `Authorization: Bearer <sessionToken>`
- the configured session cookie

Admin failures are returned as JSON:

```json
{ "error": "unauthenticated" }
```

Common error codes:

| Status | Error | Meaning |
| --- | --- | --- |
| `400` | request-specific code | Invalid input. |
| `401` | `unauthenticated` | Missing or invalid session. |
| `403` | `admin_required` | Session DID is not on the admin allowlist. |
| `404` | request-specific code | Resource missing. |
| `500` | `internal_server_error` | Backend failure. |

## Shared Types

### `Song`

```ts
interface Song {
  id: string
  title: string
  artist: string
  album: string | null
  genre: string | null
  durationSeconds: number | null
  mimeType: string | null
  hasCover: boolean
  addedByDid: string
  createdAt: number
  loudnessLufs: number | null
  loudnessPeak: number | null
}
```

### `QueueItem`

Queue items are joined to song metadata by the backend.

```ts
interface QueueItem {
  id: string
  position: number
  queuedByDid: string
  songId: string
  title: string
  artist: string
  album: string | null
  genre: string | null
  durationSeconds: number | null
  mimeType: string | null
  hasCover: boolean
  addedByDid: string
  createdAt: number
  loudnessLufs: number | null
  loudnessPeak: number | null
}
```

### `RadioState`

```ts
interface RadioState {
  currentSongId: string | null
  status: "playing" | "paused" | "stopped"
  startedAt: number | null
  pausedAt: number | null
  positionSeconds: number
  updatedByDid: string | null
}
```

`positionSeconds` is already adjusted for elapsed wall time when `status` is
`playing`.

### `RadioSnapshot`

```ts
interface RadioSnapshot {
  state: RadioState
  currentSong: Song | null
  queue: QueueItem[]
}
```

### `RadioAlbum`

Computed by grouping songs with matching (normalized) album tags; there is no
persisted album entity.

```ts
interface RadioAlbum {
  id: string
  title: string
  tracks: Song[]
}
```

### `ChatMessage`

```ts
interface ChatMessage {
  id: string
  senderDid: string
  body: string
  createdAt: number
  kind: "user" | "now_playing"
}
```

## Health And Session

### `GET /api/health`

Returns:

```json
{ "ok": true }
```

### `GET /api/session`

Reads the bearer token or session cookie and returns the current auth state.

```ts
interface SessionResponse {
  authenticated: boolean
  accountDid: string | null
  isAdmin: boolean
}
```

### `GET /api/oauth/start?input=<handle-or-did>`

Starts the OAuth sign-in flow and redirects to the authorization server. `input`
is required.

### `GET /api/oauth/callback`

OAuth callback route. On success, sets the session cookie and redirects back to
the configured frontend success URL with the session token.

### `POST /api/logout`

Signs out the current session and clears the session cookie.

Success: `204 No Content`.

## Radio Read API

### `GET /api/radio/state`

Returns the current `RadioSnapshot`.

The backend lazily advances expired tracks before returning this snapshot, so a
fresh fetch should reflect the real current song and queue.

### `GET /api/radio/seek`

Returns the current playback position only:

```ts
interface RadioSeek {
  positionSeconds: number
}
```

Use this when a client already has the snapshot and only needs to resync the
audio element position.

## Radio Admin API

All endpoints in this section require an admin session.

### `POST /api/radio/queue`

Append an existing song to the queue.

Request:

```json
{ "songId": "song-id" }
```

Response: updated `RadioSnapshot`.

### `POST /api/radio/queue/album`

Append several existing songs in order.

Request:

```json
{ "songIds": ["song-1", "song-2"] }
```

Response: updated `RadioSnapshot`.

### `DELETE /api/radio/queue`

Clear every queued item.

Response: updated `RadioSnapshot`.

### `DELETE /api/radio/queue/{queueId}`

Remove one queue item.

Response: updated `RadioSnapshot`.

### `POST /api/radio/queue/reorder`

Set queue order by queue item id.

Request:

```json
{ "queueIds": ["queue-item-1", "queue-item-2"] }
```

The list should contain the queue ids in the desired final order.

Response: updated `RadioSnapshot`.

### `POST /api/radio/control/{action}`

Controls playback. `{action}` is one of:

- `play`
- `pause`
- `stop`
- `skip`
- `previous`

Request:

```json
{ "intent": "explicit_admin_action" }
```

The explicit `intent` value is required to avoid accidental control calls.

Response: updated `RadioSnapshot`.

## Songs

### `GET /api/songs`

Returns `Song[]`, newest first.

### `POST /api/songs`

Upload a local audio file with `multipart/form-data`. Requires admin.

Fields:

| Field | Required | Notes |
| --- | --- | --- |
| `file` | yes | Audio file bytes. Playlist files are rejected here. |
| `title` | usually | Can be omitted only when embedded metadata or filename parsing can fill it. |
| `artist` | usually | Can be omitted only when embedded metadata or filename parsing can fill it. |
| `album` | no | Empty values are stored as `null`. |
| `genre` | no | Empty values are ignored. |
| `durationSeconds` | no | Positive integer. If omitted, embedded metadata may fill it. |
| `addToQueue` | no | String `true` to queue immediately. Any other value is false. |

Response: created or deduplicated `Song`.

Example:

```bash
curl -X POST "$BASE/api/songs" \
  -H "Authorization: Bearer $TOKEN" \
  -F "file=@./track.mp3;type=audio/mpeg" \
  -F "title=Track Title" \
  -F "artist=Artist Name" \
  -F "album=Album Name" \
  -F "addToQueue=true"
```

### `POST /api/songs/from-url`

Import audio from an HTTP(S) URL or a supported `yt-dlp` URL. Requires admin.

Request:

```ts
interface UrlSongRequest {
  url: string
  title?: string
  artist?: string
  album?: string
  addToQueue?: boolean
}
```

For plain HTTP(S) audio and playlist URLs, `title` and `artist` are required.
For `yt-dlp` sources such as YouTube, SoundCloud, Bandcamp, and Vimeo, title and
artist can be inferred from the source metadata.

Response: imported `Song`. Playlist URLs import up to 100 same-origin entries and
return the first imported song; finished queue/state updates also appear on the
radio websocket.

Common errors:

| Status | Error | Meaning |
| --- | --- | --- |
| `400` | `invalid_url` | URL is not HTTP(S). |
| `400` | `missing_title` | Plain URL import needs a title. |
| `400` | `missing_artist` | Plain URL import needs an artist. |
| `400` | `url_fetch_failed` | Backend could not fetch the URL. |
| `400` | `playlist_requires_batch_import` | Direct file upload was a playlist. Use URL import. |
| `422` | `source_unavailable` | `yt-dlp` says the source is removed/private/unavailable. |
| `502` | `ytdlp_failed` | `yt-dlp` failed for another reason. |

### `PUT /api/songs/{songId}`

Update editable metadata. Requires admin.

Request:

```ts
interface SongMetadataRequest {
  title: string
  artist: string
  album?: string | null
  genre?: string | null
  durationSeconds?: number | null
}
```

Response: updated `Song`.

### `DELETE /api/songs/{songId}`

Delete a song. Requires admin.

The delete also removes queued entries for that song through database cascades.

Response: updated `RadioSnapshot`.

### `PUT /api/songs/{songId}/cover`

Upload or replace cover art with `multipart/form-data`. Requires admin.

Fields:

| Field | Required | Notes |
| --- | --- | --- |
| `cover` | yes | Image bytes. The field content type is stored with the image. |

Response: updated `Song`.

## Audio And Cover Files

### `GET /api/songs/{songId}/audio`

Returns the stored audio bytes for a song. No auth required.

Use the `id` from `currentSong.id`, `queue[].songId`, or `GET /api/songs`:

```html
<audio src="https://radio.example.test/api/songs/song-id/audio" controls></audio>
```

The response uses the stored MIME type when available, otherwise
`application/octet-stream`.

Audio supports byte ranges:

```http
Range: bytes=0-1048575
```

Range behavior:

| Request | Response |
| --- | --- |
| no `Range` | `200 OK`, full body, `Accept-Ranges: bytes`, `Content-Length` |
| valid single range | `206 Partial Content`, `Content-Range`, `Accept-Ranges: bytes`, `Content-Length` |
| invalid range | `416 Range Not Satisfiable`, `Content-Range: bytes */<total>` |

Supported range forms:

- `bytes=<start>-<end>`
- `bytes=<start>-`
- `bytes=-<suffixLength>`

Multiple ranges are not supported.

Errors:

| Status | Error | Meaning |
| --- | --- | --- |
| `404` | `song_not_found` | No song row exists. |
| `404` | `audio_not_found` | Song row exists but the file cannot be read. |

### `GET /api/songs/{songId}/cover`

Returns original cover art bytes. No auth required.

Response headers include:

- `Content-Type`: stored cover MIME type or `application/octet-stream`
- `Cache-Control: public, max-age=31536000, immutable`
- `Content-Length`

Error: `404 { "error": "cover_not_found" }`.

### `GET /api/songs/{songId}/cover/thumbnail`

Returns a generated 128 by 128 JPEG thumbnail. No auth required.

Response headers include:

- `Content-Type: image/jpeg`
- `Cache-Control: public, max-age=31536000, immutable`
- `Content-Length`

Error: `404 { "error": "cover_not_found" }`.

## Radio Websocket

### `GET /api/radio/ws`

Open this as a websocket. Use `wss://` for HTTPS sites and `ws://` for HTTP.

Example URL:

```text
wss://radio.example.test/api/radio/ws
```

On connect, the server sends:

1. `snapshotChanged` with the current `RadioSnapshot`.
2. `viewerCountChanged` with the current listener count and listener DIDs.

Server messages:

```ts
type RadioEvent =
  | { type: "snapshotChanged"; snapshot: RadioSnapshot }
  | { type: "viewerCountChanged"; viewerCount: number; listenerDids: string[] }
  | { type: "viewerKeepalive" }
```

`snapshotChanged` is broadcast after queue, song, upload, and admin playback
mutations. Natural track-end advancement intentionally does not broadcast every
time; clients should self-advance locally and can resync with
`GET /api/radio/state` or `GET /api/radio/seek`.

Viewer presence is opt-in. Send a stable per-tab `viewerId` after opening:

```json
{ "type": "viewerHello", "viewerId": "tab-uuid", "did": "did:plc:..." }
```

When the server sends `viewerKeepalive`, respond with:

```json
{ "type": "viewerKeepalive", "viewerId": "tab-uuid", "did": "did:plc:..." }
```

`did` is optional. It is accepted only if it starts with `did:` and contains
ASCII letters, digits, `:`, `-`, `_`, or `.`. `viewerId` must be at most 128
characters and contain only ASCII letters, digits, `-`, or `_`.

The server sends keepalives every 60 seconds. If a registered viewer does not
send a keepalive for about 70 seconds, the server closes the socket and removes
that viewer from the count.

## Chat Websocket

### `GET /api/radio/chat/ws`

Open this as a websocket for chat messages and now-playing breadcrumbs.

On connect, the server sends recent history:

```ts
{ type: "history"; messages: ChatMessage[] }
```

Server messages:

```ts
type ChatEvent =
  | { type: "history"; messages: ChatMessage[] }
  | { type: "message"; message: ChatMessage }
  | { type: "messageDeleted"; id: string }
  | { type: "messagesPurged"; senderDid: string }
```

To send a chat message, send the session token inside the websocket message:

```json
{ "type": "send", "text": "hello", "token": "session-token" }
```

Chat sends are ignored without an authenticated session. Empty messages,
messages over 1000 characters, and messages from banned DIDs are also ignored.
The server does not send per-message error events for rejected chat sends.

## Chat Moderation API

All endpoints in this section require admin.

### `DELETE /api/radio/chat/messages/{messageId}`

Delete one chat message.

Success: `204 No Content`.

Error: `404 { "error": "chat_message_not_found" }`.

### `GET /api/radio/chat/bans`

Returns:

```ts
interface ChatBan {
  did: string
  bannedByDid: string
  reason: string | null
  createdAt: number
}
```

### `POST /api/radio/chat/bans`

Ban a DID and purge its messages from live clients.

Request:

```json
{ "did": "did:plc:...", "reason": "optional reason" }
```

Response: `ChatBan`.

### `DELETE /api/radio/chat/bans/{did}`

Remove a ban.

Success: `204 No Content`.

Error: `404 { "error": "ban_not_found" }`.

## Subsonic Import Helpers

These endpoints are admin-only helpers for importing from an external Subsonic
server. They are separate from the Subsonic-compatible `/rest/*` shim.

### `POST /api/subsonic/search`

Request:

```ts
interface SubsonicSearchRequest {
  serverUrl: string
  username: string
  password: string
  query: string
}
```

Response:

```ts
interface SubsonicSongResult {
  id: string
  title: string
  artist: string
  album: string | null
  durationSeconds: number | null
  coverArtId: string | null
}
```

### `POST /api/songs/from-subsonic`

Request:

```ts
interface SubsonicImportRequest {
  serverUrl: string
  username: string
  password: string
  songId: string
  coverArtId?: string | null
  addToQueue?: boolean
}
```

Response: imported `Song`.

### `POST /api/songs/from-subsonic-share`

Request:

```ts
interface SubsonicShareImportRequest {
  shareUrl: string
  addToQueue?: boolean
}
```

Response: imported `Song`.

## Admin Allowlist API

### `GET /api/admin/permissions`

Requires admin. Returns:

```ts
interface AdminPermissionsResponse {
  whitelistedDids: string[]
  permissions: { key: string; description: string }[]
}
```

### `POST /api/admin/dids`

Requires admin. Adds a DID to the admin allowlist.

Request:

```json
{ "did": "did:plc:..." }
```

Response: `AdminPermissionsResponse`.

### `DELETE /api/admin/dids/{did}`

Requires admin. Removes a DID from the admin allowlist.

Response: `AdminPermissionsResponse`.
