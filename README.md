# radio

small rust + solidjs radio server. the rust backend owns the xrpcs, media, audio storage, embedded read-only pds, and production static serving; the solid frontend owns browser oauth and the ui.

## setup & hosting

To run your own radio station and have it syndicated (so it automatically shows up on directories like `https://radio.wisp.place`):

1. **Install dependencies:**
   ```bash
   cp .env.example .env
   npm --prefix frontend install
   cargo check
   ```

2. **Expose it publicly:**
   Your station must be publicly reachable over standard HTTPS on a public domain name so ATProto relays and directory workers can crawl it. Setting up a reverse proxy (like Caddy or Nginx) or routing traffic through a Cloudflare Tunnel is the easiest way to expose it.

3. **Configure your `.env`:**
   Configure your public domain endpoints (you can use the exact same domain name for all of them!):
   ```env
   APP_URL=https://radio.yourdomain.com
   SERVICE_DID=did:web:radio.yourdomain.com
   STATION_URL=https://radio.yourdomain.com
   STATION_ANNOUNCE_WORKERS=https://syndication.sharkgirl.pet
   ```

When the backend starts up, it will automatically register your station's metadata and notify the syndication workers to crawl it. 

You can also trigger a manual crawl announcement directly to the public worker:
```bash
curl -X POST https://syndication.sharkgirl.pet/xrpc/com.atproto.sync.requestCrawl \
  -H "Content-Type: application/json" \
  -d '{"hostname": "radio.yourdomain.com"}'
```

## syndication worker

If you want to run your own syndication worker directory instead of using `https://syndication.sharkgirl.pet`:

1. **Configure environment:**
   ```bash
   cp syndication-worker/.env.example syndication-worker/.env
   # Edit syndication-worker/.env as needed
   ```

2. **Run it:**
   ```bash
   cargo run --manifest-path syndication-worker/Cargo.toml
   ```

It listens on `http://127.0.0.1:3300` by default. You can query its status and endpoints:
- `GET /health` - Overall health check.
- `GET /stations` - List of all crawled healthy stations.
- `GET /stations/{did}` - Detailed information on a specific station.
- `POST /xrpc/com.atproto.sync.requestCrawl` - Request the worker to crawl your public station.

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

## environment reference

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
