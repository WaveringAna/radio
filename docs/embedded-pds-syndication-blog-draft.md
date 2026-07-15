# Draft: Syndicating Sister Radio With an Embedded Read-Only PDS

Sister Radio needed a way for independent radio backends to find each other without turning one central service into the source of truth for every station. The design we ended up with is small, weird in a useful way, and very ATProto-shaped:

1. Each radio backend publishes one world-readable station record from an embedded read-only PDS.
2. A syndication worker watches ATProto repo events for that record type.
3. The frontend asks the worker which stations are live, then targets the selected station's XRPC service.
4. The listener signs in once with their own ATProto account, and the same OAuth session works across stations through PDS-proxied service XRPC.

The inspiration is Streamplace's excellent writeup, [How Streamplace Works: Embedded PDS](https://blog.stream.place/3lut7mgni5s2k). Streamplace uses an embedded PDS because it wants a node to host protocol records itself. We are using the same general "static PDS" idea in a much narrower way: publish a tiny station advertisement, not a whole user-content store.

That constraint matters. A fully writable public database of songs would be convenient, but it also makes abuse and takedown surfaces bigger. For syndication, we only need a station to say:

```json
{
  "$type": "pet.nkp.radio.station",
  "url": "https://radio.example.com",
  "name": "radio",
  "updatedAt": "2026-07-05T11:47:05Z"
}
```

Everything else can stay on the radio backend.

The discovery path looks like this:

```text
station backend
  -> embedded read-only PDS record
  -> relay crawl/firehose
  -> Hydrant inside the syndication worker
  -> health-filtered /stations API
  -> frontend tune-in strip
```

## The Embedded PDS

Each station backend has a public `did:web` identity. The backend serves:

- `/.well-known/atproto-did`
- `/.well-known/did.json`
- a small set of repo/sync XRPC endpoints
- the station record at `at://<station-did>/pet.nkp.radio.station/self`

In Sister Radio, this is built in Rust with [`jacquard-repo`](https://crates.io/crates/jacquard-repo), a Rust library for building and reading ATProto repositories. On boot, the backend loads or creates a secp256k1 signing key, stores it in SQLite, and exposes the public key in the DID document. Then it creates an in-memory ATProto repository with one record: `pet.nkp.radio.station/self`.

The useful bit is that this is a real signed repository. The backend does not just serve JSON that looks like ATProto. It builds a repo commit, writes the station record into the MST, calculates the record CID, emits a CAR for sync endpoints, and prepares a `subscribeRepos` commit frame.

The repo is intentionally rebuilt from one record on startup and on manual reemit. For this use case, long commit history is not valuable: consumers only need the latest station advertisement. The persisted pieces are the signing key and the station metadata, so the DID key stays stable while the one-record repo can be regenerated.

The minimum read-only PDS surface is intentionally small:

```text
com.atproto.server.describeServer
com.atproto.repo.describeRepo
com.atproto.repo.getRecord
com.atproto.repo.listRecords
com.atproto.sync.getRepo
com.atproto.sync.getRecord
com.atproto.sync.listRepos
com.atproto.sync.subscribeRepos
```

That is enough for relays and crawlers to resolve the DID, describe the repo, fetch the record, and subscribe to the repo's commit stream.

## Reemitting the Station Record

Publishing once is not quite enough. If the relay has already seen the repo at an old commit, asking it to crawl again may not produce a new event because the relay can dedupe on the repo's current rev/commit. For syndication we want the station to be able to say "I am still here" in a way that produces a fresh repo update.

So Sister Radio has a reemit path:

```bash
curl -X POST https://radio.example.com/api/syndication/announce
```

That route:

1. Refreshes the station record's `updatedAt`.
2. Rebuilds the one-record embedded repo with a fresh commit.
3. Swaps the live PDS state in the backend.
4. Calls `com.atproto.sync.requestCrawl` on each configured relay.

The startup path does the same thing when `STATION_ANNOUNCE_ON_STARTUP=true`, so a station can announce itself every time the backend comes up behind its public URL.

The backend only announces its configured public host. It does not accept arbitrary hostnames from callers. That keeps the route from turning into an open relay-crawl spam tool.

There is still an operational caveat: an unauthenticated announce route can be called repeatedly, even if it can only announce its own host. In production this should have a small rate limit or be restricted to admins. The current safe boundary is "no arbitrary hostname"; the next safe boundary is "no unbounded repeated `requestCrawl` calls."

## The Syndication Worker

The syndication worker is the other half of the system. It embeds [Hydrant](https://tangled.org/ptr.pet/hydrant) as a library instead of implementing firehose and crawl plumbing from scratch. Hydrant is a collector/indexer layer for ATProto repo events; in this worker it gives us filtered crawl and firehose ingestion.

Hydrant is configured to care about one collection:

```text
pet.nkp.radio.station
```

The worker enables:

- firehose listening, so fresh station commits arrive in real time
- crawler discovery, so repos that already contain the station record can be found
- collection filtering, so the worker is not trying to index the whole network

When Hydrant emits a record event for `pet.nkp.radio.station`, the worker parses it into a station view and keeps the latest record by DID.

The worker does not index every syntactically valid record blindly. Today it requires a `did:web` station DID and checks that the record's `url` hostname matches the hostname embedded in that DID. A record from `did:web:radio.example.com` can advertise `https://radio.example.com`; it cannot advertise `https://some-other-host.example`. Health checks then verify that the advertised backend responds like a Sister Radio station.

That is not the same as a full trust model. Anyone who controls their own domain can still publish a station record for that domain and pass health checks. For now, that is an acceptable property for an open discovery layer. A curated directory can add allowlists, moderation, reputation, or signed endorsements later without changing the station-record mechanism.

The HTTP API is intentionally tiny:

```text
GET /health
GET /stations
GET /stations/{did}
```

The frontend only needs `/stations`. That endpoint is now health-filtered: the worker keeps indexed station records internally, periodically probes each station backend, and only returns stations that are currently considered alive.

The health checker probes:

```text
<station-url>/api/health
<station-url>/health
```

Newly discovered stations are hidden until they pass health once. Already-healthy stations can tolerate a configurable number of misses before being hidden. This makes the worker opinionated about liveness without making one transient network failure immediately remove a station.

The knobs are:

```text
SYNDICATION_HEALTH_INTERVAL_SECS=30
SYNDICATION_HEALTH_TIMEOUT_SECS=5
SYNDICATION_HEALTH_FAILURE_THRESHOLD=2
```

This gives us a central discovery view without making the central worker authoritative over station data. It does gate discovery visibility, because unhealthy or mismatched stations are hidden, but the source of truth for a station's advertisement is still the station's own embedded PDS record.

## The Frontend Tune-In Flow

The frontend loads the local station plus syndicated stations from the worker and renders them as a station strip. Each station has:

- a public URL
- an optional DID from the station record
- an API base for local fallback or remote calls

When a listener selects a station, the frontend stores the selected station URL and constructs a `RadioTarget`:

```ts
type RadioTarget = {
  did?: string
  baseUrl?: string
  serviceId?: string
}
```

All radio operations take that target. Public reads can hit the selected backend directly. Authenticated XRPCs go through ATProto OAuth and service auth.

If the worker has a DID for the station, the frontend uses it directly as the XRPC audience. If the user only has a remembered URL, the frontend falls back to fetching `/.well-known/atproto-did` from that backend before constructing the proxy target.

## One Login, Many Stations

The login model is the part that makes this feel like ATProto instead of a custom federation protocol.

The browser signs into the listener's own PDS using `@atcute/oauth-browser-client`. That gives the frontend one OAuth session for the listener's DID. Sister Radio stores that browser-side session locally.

When the user interacts with a station, the frontend does not use a server-side session cookie. It creates an atcute client with an `OAuthUserAgent` and a dynamic proxy target:

```ts
const proxy = `${stationDid}#radio_xrpc`
```

For multipart uploads, the frontend sends the same proxy target in the `atproto-proxy` header. For normal JSON XRPCs, atcute's client proxy setting handles it.

That means the request path is:

```text
browser
  -> listener's PDS OAuth session
  -> atproto proxy target did:web:station.example.com#radio_xrpc
  -> selected station backend
```

The station's DID document advertises `#radio_xrpc` as an `AtprotoService` with the backend's service endpoint. The listener's PDS can mint service-auth for that target, and the station backend can verify that the request came from an authenticated ATProto identity.

Admin actions are still gated by the station. The backend checks the caller DID against `ADMIN_DIDS` or whatever admin state the station owns. So the same login can technically reach every station, but it only grants control where that DID is allowed.

This is what lets the UI have one account session and still tune into different backends. Selecting another station changes the XRPC audience and proxy target, not the user's login.

## Why This Shape Works

The nice thing about this design is the separation of concerns:

- The embedded PDS publishes a small, signed, relay-visible station record.
- The syndication worker indexes and health-checks those records.
- The frontend uses the worker for discovery but talks to stations directly for radio behavior.
- The user's PDS handles OAuth and service proxying.
- Each station keeps its own queue, songs, chat, admin policy, and media.

The central worker can disappear and existing station URLs still work. A station can disappear and the worker will eventually stop returning it. A user can sign in once and use the same identity across stations, but stations keep their own authorization boundaries.

It is not a general-purpose public database. That is deliberate. For radio syndication, the public record only needs to say where the station is and when it last announced itself. The heavier, riskier data stays with the station that owns it.

## What Is Still Open

There are a few obvious next steps.

First, the worker can rank stations by liveness and listener count once the station health response includes listener metadata. Right now the health probe only answers whether the station should be returned at all.

Second, the read-only embedded PDS surface should eventually have conformance tests. Streamplace's article calls out the same rough edge: there is no tiny official checklist for "the smallest compliant static PDS."

Third, reemit policy needs product judgment. Startup announce is useful, but stations may also want scheduled reemits, manual "broadcast started" announces, or separate records for currently-live shows.

Fourth, the lexicon should evolve conservatively. New station metadata should be added as optional fields first, following normal ATProto lexicon compatibility expectations, so old workers can keep indexing the required `url`, `name`, and `updatedAt` fields.

The core pattern is already useful, though: a radio backend can advertise itself with a real ATProto repo, a worker can collect those records without owning the stations, and one ATProto login can follow the listener from station to station.
