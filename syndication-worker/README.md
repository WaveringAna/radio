# sister-radio syndication worker

This is a small collector for `pet.nkp.radio.station` records. It embeds Hydrant as a library, watches the filtered AT Protocol firehose, crawls for repos that publish the station lexicon, keeps the latest station record in memory, and serves the collected records over HTTP.

Seed DIDs are optional. They are useful for bootstrapping known stations immediately, while Hydrant's crawler and firehose discovery continue to find live broadcasts.

## Run

```bash
cp syndication-worker/.env.example syndication-worker/.env
$EDITOR syndication-worker/.env
cargo run --manifest-path syndication-worker/Cargo.toml
```

Useful endpoints:

```bash
curl http://127.0.0.1:3300/health
curl http://127.0.0.1:3300/stations
curl http://127.0.0.1:3300/stations/did:web:radio.example.com
```

## Environment

| variable | default | description |
| --- | --- | --- |
| `SYNDICATION_BIND_ADDR` | `127.0.0.1:3300` | HTTP API bind address. |
| `SYNDICATION_SEED_DIDS` | empty | Optional comma-separated DIDs to track and backfill immediately. |
| `HYDRANT_DATABASE_PATH` | `./hydrant.db` | Hydrant database directory. |
| `HYDRANT_ENABLE_FIREHOSE` | `true` in this worker | Listen to relay/PDS firehose ingestion for live station commits. |
| `HYDRANT_ENABLE_CRAWLER` | `true` in this worker | Discover repos that have matching station records. |
| `HYDRANT_RELAY_HOST` | `wss://relay.fire.hose.cam/` | Firehose relay source. |
| `HYDRANT_CRAWLER_URLS` | `by_collection::https://lightrail.microcosm.blue` | Filter-aware crawler source. |
| `HYDRANT_ENABLE_BACKFILL` | `true` in `.env.example` | Fetch repo contents for discovered or seeded station repos. |
