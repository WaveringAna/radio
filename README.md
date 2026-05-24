# radio

small rust + solidjs radio server. the rust backend owns the api, auth, audio storage, and production static serving; the solid frontend is used for the browser ui.

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
| `APP_URL` | `http://127.0.0.1:3000` | public backend url used for oauth client metadata, callbacks, and service defaults |
| `SERVICE_DID` | `did:web:localhost` | service DID expected as the service-auth JWT audience |
| `SERVICE_IDS` | `#radio_xrpc` | comma-separated DID document service ids; quote values starting with `#` in `.env` |
| `SERVICE_ENDPOINT` | `APP_URL` with `http://` rewritten to `https://` | endpoint published in `/.well-known/did.json` for PDS proxy service routing |
| `OAUTH_AUTHORIZATION_SERVER` | `https://bsky.social` | authorization server advertised by `/.well-known/oauth-protected-resource` |
| `CORS_ORIGIN` | `http://127.0.0.1:5173` | allowed browser origin in dev; also used as the frontend redirect base |
| `DATABASE_URL` | `sqlite://radio.db` | sqlite database url |
| `SESSION_COOKIE_NAME` | `radio_session` | auth session cookie name |
| `SESSION_TTL_DAYS` | `30` | session lifetime in days |
| `ADMIN_DIDS` | empty | comma-separated did allowlist for admin actions |
| `AUDIO_DIR` | `data/audio` | uploaded audio and cover storage directory |

## development

run the backend:

```bash
cargo run
```

run the frontend dev server:

```bash
npm --prefix frontend run dev
```

vite proxies `/api` to `http://127.0.0.1:3000` in dev.

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
