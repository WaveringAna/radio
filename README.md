# radio

small rust + solidjs radio server. the rust backend owns the xrpcs, media, audio storage, embedded read-only pds, and production static serving; the solid frontend owns browser oauth and the ui.

## setup

```bash
cp .env.example .env
npm --prefix frontend install
cargo check
```

edit `.env` for your local url, database, admin dids, and audio directory.

## environment

| variable | default | description |
| --- | --- | --- |
| `BIND_ADDR` | `0.0.0.0:3000` | socket address the rust backend listens on |
| `APP_URL` | `http://127.0.0.1:3000` | public backend url used for oauth client metadata and service defaults |
| `FRONTEND_URL` | `APP_URL` | frontend origin used for the browser oauth `/auth` redirect |
| `SERVICE_DID` | `did:web:localhost` | service DID expected as the service-auth JWT audience |
| `SERVICE_IDS` | `#radio_xrpc` | comma-separated DID document service ids; quote values starting with `#` in `.env` |
| `SERVICE_ENDPOINT` | `APP_URL` with `http://` rewritten to `https://` | endpoint published in `/.well-known/did.json` for PDS proxy service routing |
| `STATION_URL` | `SERVICE_ENDPOINT` | public radio backend URL advertised by the embedded read-only PDS |
| `STATION_ANNOUNCE_RELAYS` | `https://relay.fire.hose.cam` | comma-separated relay HTTP origins to notify with `com.atproto.sync.requestCrawl` |
| `STATION_ANNOUNCE_WORKERS` | empty | comma-separated syndication worker origins to notify with the same `requestCrawl` call |
| `STATION_ANNOUNCE_ON_STARTUP` | `true` | whether to announce the public station host after the listener starts |
| `STATION_NAME` | `radio` | station name advertised by the embedded read-only PDS |
| `STATION_DESCRIPTION` | empty | optional station description advertised by the embedded read-only PDS |
| `PDS_SIGNING_KEY_HEX` | generated and stored in sqlite | optional 32-byte secp256k1 private key hex for signing the embedded PDS repo |
| `OAUTH_AUTHORIZATION_SERVER` | `https://bsky.social` | authorization server advertised by `/.well-known/oauth-protected-resource` |
| `DATABASE_URL` | `sqlite://radio.db` | sqlite database url |
| `ADMIN_DIDS` | empty | comma-separated did allowlist for admin actions |
| `AUDIO_DIR` | `data/audio` | uploaded audio and cover storage directory |

## syndication & public hosting

To have your station discovered and syndicated (so it appears on public directories like `https://radio.wisp.place`):

1. **Public Domain & SSL/TLS**: Your station **must** be publicly reachable over standard HTTPS on a public domain name (e.g., `https://radio.yourdomain.com`). Relays and syndication workers cannot crawl local addresses (`localhost`, `127.0.0.1`), private IPs, or servers with invalid/self-signed SSL certificates.
2. **Reverse Proxy Setup**: Run the backend behind a reverse proxy (such as Caddy, Nginx, or a Cloudflare Tunnel) to manage HTTPS certificates and forward inbound traffic to the Rust backend (default `BIND_ADDR=0.0.0.0:3000`).
3. **Environment Setup**: In your `.env` file, configure your public endpoints:
   - `APP_URL=https://radio.yourdomain.com`
   - `SERVICE_DID=did:web:radio.yourdomain.com`
   - `STATION_URL=https://radio.yourdomain.com`
   - `STATION_ANNOUNCE_WORKERS=https://syndication.sharkgirl.pet` (or the URL of the active syndication directory worker)

When the backend boots (or receives a manual announcement), it publishes the station's metadata in the embedded read-only PDS under the record `pet.nkp.radio.station/self` and prompts the relays and syndication workers in your `.env` to crawl its public DID document and repository.

You can force a syndication crawl announcement manually without restarting:

```bash
curl -X POST http://127.0.0.1:3000/api/syndication/announce
```
## development

run the backend:

```bash
cargo run
```

run the frontend dev server:

```bash
npm --prefix frontend run dev
```

vite proxies `/api`, `/xrpc`, `/.well-known`, and `/client-metadata.json` to `http://127.0.0.1:3000` in dev.

## production build

```bash
npm --prefix frontend run build
cargo run --release
```

`npm --prefix frontend run build` writes the normal vite output to `frontend/dist`, then copies it into the backend `static/` directory. the rust backend serves `static/` and falls back to `static/index.html` for frontend routes.

## useful paths

- `src/` rust backend
- `frontend/` solidjs frontend
- `static/` frontend bundle served by rust
- `data/audio/` default uploaded audio storage
- `migrations/` sqlite migrations
