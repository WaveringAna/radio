# XRPC API Reference

The radio backend serves AT Protocol XRPC endpoints at `/xrpc/{nsid}`. Every
endpoint listed here requires a service JWT in the `Authorization: Bearer <token>`
header, scoped to the called NSID via the `lxm` claim, and the caller's DID must be
on the admin whitelist.

All endpoints share the data shapes defined in the `pet.nkp.radio` lexicon; see
[Shared Types](#shared-types) at the bottom.

## Authentication

🔒 marks an authenticated endpoint. Every endpoint below is authenticated.

A request is accepted only when **both** checks pass:

1. **Method binding** — the service JWT's `lxm` (lexicon method) claim must equal the
   NSID being called. A mismatch returns `AuthenticationRequired` (`401`).
2. **Admin whitelist** — the caller's DID (the JWT issuer) must be on the server's
   admin whitelist. A non-admin DID returns `AdminRequired` (`403`); a whitelist
   lookup failure returns `InvalidRequest` (`500`).

| Failure | Error | Status |
| --- | --- | --- |
| `lxm` claim does not match the NSID | `AuthenticationRequired` | `401` |
| Caller DID not on admin whitelist | `AdminRequired` | `403` |
| Whitelist lookup failed | `InvalidRequest` | `500` |

---

## Queue

### `pet.nkp.radio.queue.list` 🔒

**query** — Load the current radio snapshot, including playback state and the
upcoming queue.

**Params:** none.

**Response:**

| Field | Type | Notes |
| --- | --- | --- |
| `snapshot` | [`radioSnapshot`](#radiosnapshot) | Combined playback state, current song, and queue. |

**Errors:** `AuthenticationRequired`, `AdminRequired`, `InvalidRequest`.

---

### `pet.nkp.radio.queue.modify` 🔒

**procedure** — Modify the radio queue. The `action` field selects the operation;
the companion fields it requires are enforced by the server.

**Input** (`application/json`):

| Field | Type | Required | Notes |
| --- | --- | --- | --- |
| `action` | string | yes | One of `enqueue`, `remove`, `clear`, `reorder`. |
| `songIds` | string[] | for `enqueue` | Song ids to append, in order. Must be non-empty. |
| `queueId` | string | for `remove` | Queue item id to remove. |
| `queueIds` | string[] | for `reorder` | Queue item ids in the desired final order. |

**Response:**

| Field | Type | Notes |
| --- | --- | --- |
| `snapshot` | [`radioSnapshot`](#radiosnapshot) | Updated snapshot after the operation. |

**Errors:** `AuthenticationRequired`, `AdminRequired`, `InvalidRequest`,
`SongNotFound`, `QueueItemNotFound`.

| Error | When |
| --- | --- |
| `InvalidRequest` | Missing required companion field (e.g. `songIds` for `enqueue`), empty `songIds`, or an unknown `action`. |
| `SongNotFound` | An `enqueue` song id does not exist. |
| `QueueItemNotFound` | A `remove` queue item id does not exist. |

---

## Songs

### `pet.nkp.radio.songs.list` 🔒

**query** — List songs stored in the radio library.

**Params:** none.

**Response:**

| Field | Type | Notes |
| --- | --- | --- |
| `songs` | [`song`](#song)[] | Songs ordered newest first. |

**Errors:** `AuthenticationRequired`, `AdminRequired`, `InvalidRequest`.

---

### `pet.nkp.radio.songs.add` 🔒

**procedure** — Import one or more remote songs through the backend's URL importer
(HTTP(S) audio, playlists, or any `yt-dlp`-supported URL).

**Input** (`application/json`):

| Field | Type | Required | Notes |
| --- | --- | --- | --- |
| `sources` | [`songUrlSource`](#songurlsource)[] | yes | Remote audio sources to import, in order. 1–100 items. |

**Response:**

| Field | Type | Notes |
| --- | --- | --- |
| `songs` | [`song`](#song)[] | Imported or deduplicated songs. |
| `snapshot` | [`radioSnapshot`](#radiosnapshot) | Snapshot taken after the import. |

**Errors:** `AuthenticationRequired`, `AdminRequired`, `InvalidRequest`,
`InvalidUrl`, `DownloadFailed`, `UnsupportedAudio`.

| Error | When |
| --- | --- |
| `InvalidRequest` | `sources` is empty or contains more than 100 items. |
| `InvalidUrl` | A source URL is malformed or rejected. |
| `DownloadFailed` | Fetching the URL, running `yt-dlp`, or reading a playlist entry failed. |
| `UnsupportedAudio` | The downloaded media is missing, unreadable, or an unsupported format. |

---

## Shared Types

Defined in the `pet.nkp.radio` lexicon. `nullable` fields are present in responses
but may be `null`.

### `song`

Song metadata stored by the radio backend.

| Field | Type | Nullable | Notes |
| --- | --- | --- | --- |
| `id` | string | no | Stable song id (1–128 chars). |
| `title` | string | no | Song title (1–512 chars). |
| `artist` | string | no | Song artist (1–512 chars). |
| `album` | string | yes | Album title. |
| `genre` | string | yes | Genre. |
| `durationSeconds` | integer | yes | Duration in seconds. |
| `mimeType` | string | yes | Stored audio MIME type. |
| `hasCover` | boolean | no | Whether the song has cover art. |
| `addedByDid` | string | no | DID that uploaded the song. |
| `createdAt` | integer | no | Unix timestamp of upload (≥ 0). |
| `loudnessLufs` | string | yes | Integrated loudness in LUFS, as a decimal string. |
| `loudnessPeak` | string | yes | True peak in dBFS, as a decimal string. |

### `queueItem`

Queue item joined with its song metadata.

| Field | Type | Nullable | Notes |
| --- | --- | --- | --- |
| `id` | string | no | Stable queue item id (1–128 chars). |
| `position` | integer | no | Queue position; lower values play first (≥ 1). |
| `queuedByDid` | string | no | DID that queued the song. |
| `songId` | string | no | Queued song id (1–128 chars). |
| `title` | string | no | Queued song title (1–512 chars). |
| `artist` | string | no | Queued song artist (1–512 chars). |
| `album` | string | yes | Queued song album. |
| `addedByDid` | string | no | DID that originally uploaded the song. |

### `radioState`

Radio playback status persisted by the backend.

| Field | Type | Nullable | Notes |
| --- | --- | --- | --- |
| `currentSongId` | string | yes | Currently active song id, when one is selected. |
| `status` | string | no | Playback status: `playing`, `paused`, or `stopped`. |
| `startedAt` | integer | yes | Unix timestamp playback was last started. |
| `pausedAt` | integer | yes | Unix timestamp playback was last paused. |
| `positionSeconds` | integer | no | Stored playback offset in seconds (≥ 0). |
| `updatedByDid` | string | yes | DID or backend actor that last updated state. |

### `radioSnapshot`

Combined radio view returned to clients.

| Field | Type | Nullable | Notes |
| --- | --- | --- | --- |
| `state` | [`radioState`](#radiostate) | no | Current playback state. |
| `currentSong` | [`song`](#song) | yes | Full metadata for the current song. |
| `queue` | [`queueItem`](#queueitem)[] | no | Upcoming queued songs. |

### `songUrlSource`

A remote audio source to import through the backend URL importer.

| Field | Type | Required | Notes |
| --- | --- | --- | --- |
| `url` | string (uri) | yes | HTTP(S) audio, playlist, or `yt-dlp`-supported URL (8–4096 chars). |
| `title` | string | no | Title override for plain audio URLs. |
| `artist` | string | no | Artist override for plain audio URLs. |
| `album` | string | no | Album override. |
| `addToQueue` | boolean | no | Queue imported songs immediately. Default `false`. |

---

## Related records

### `pet.nkp.radio.preferences`

Not an XRPC endpoint — a singleton AT Protocol record holding radio UI settings
(`accentColor`, `theme` ∈ {`light`, `dark`, `system`}, `updatedAt`). Volume
intentionally stays browser-local and is not stored here.
