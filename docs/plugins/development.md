---
description: "FeedCourier Plugin Protocol v1 specification: manifest.schema.json, HTTP Bridge wire protocol (bridge-protocol.schema.json), HMAC-SHA256 signing, channel-template starter kit, and registry submissions."
---

# Plugin Development & Protocol v1

FeedCourier defines a stable **Plugin Protocol v1 (`api_version: "1"`)** shared by first-party compiled plugins, out-of-process **HTTP Bridge Plugins** (`v0.3.1+`), and the public marketplace registry ([`ElleryHe/FeedCourier-Plugins`](https://github.com/ElleryHe/FeedCourier-Plugins)).

---

## 1. HTTP Bridge Protocol v1 (`schema/bridge-protocol.schema.json`)

Out-of-process bridge plugins are standalone HTTP(S) services (typically deployed as a Docker Compose sidecar in any language — Rust, Go, Python, Node.js, etc.).

### Request Authentication (HMAC-SHA256)

Every request sent by FeedCourier carries:

| Header | Description |
| :--- | :--- |
| `X-FeedCourier-Timestamp` | Unix epoch seconds when FeedCourier signed the request. |
| `X-FeedCourier-Signature` | `v1=<hex(HMAC-SHA256(BRIDGE_SECRET, "{timestamp}.{raw_body}"))>` |
| `X-FeedCourier-Plugin-Id` | Registered plugin ID (`"pending"` during initial `GET /manifest`). |
| `X-FeedCourier-Idempotency-Key` | Deterministic SHA-256 key on `/deliver`, `/hooks/on_render`, and `/hooks/on_delivered`. |

Bridge servers **MUST** verify the signature using constant-time comparison and reject requests whose timestamp falls outside a **`±300s`** replay window.

### Endpoints & Limits

| Endpoint | Kind | Timeout | Purpose |
| :--- | :---: | :---: | :--- |
| `GET /manifest` | All | `10s` | Return runtime `PluginManifest` JSON (`id`, `name`, `version`, `api_version: "1"`, `kind: "channel" \| "processor"`, `min_core`, `hooks`, schemas, `capabilities`). |
| `POST /validate` | All | `10s` | Validate `{ scope: "account" \| "destination" \| "instance", config }`; return `{ ok: true, normalized_config? }` or `400 { ok: false, error }`. |
| `POST /test` | All | `10s` | Execute an interactive connectivity test for `{ scope, account?, destination?, config? }`; return `{ ok: true, message }`. |
| `POST /deliver` | `channel` | `30s` | Deliver a rendered article to the custom channel. |
| `POST /hooks/on_render` | `processor` | `10s` | Transform `content_html` / `primary_link` before channel rendering (15s per-plugin total hook budget). |
| `POST /hooks/on_delivered` | Any declaring `on_delivered` | `10s` | Receive persistent at-least-once post-delivery notifications. |

**Hard Core Boundaries**:
- Response body cap: **`1 MiB`** on all endpoints.
- Redirects are strictly rejected (`redirect::Policy::none()`) and `OUTBOUND_PROXY` is never applied to bridges.
- HTML returned by `POST /hooks/on_render` is **always re-sanitized through `ammonia`** before entering any downstream renderer.
- Serialized `checkpoint` JSON returned by `POST /deliver` is capped at **`16 KiB`**.

### `POST /deliver` Contract (`kind: "channel"`)

**Request body**:

```json
{
  "idempotency_key": "<sha256>",
  "task_id": 42,
  "attempt": 1,
  "account": { "server_url": "https://ntfy.sh", "access_token": "tk_..." },
  "destination": { "topic": "feedcourier-alerts", "priority": "default" },
  "entry": {
    "title": "Article Title",
    "url": "https://example.com/post",
    "summary_text": "Concise summary",
    "content_html": "<p>Sanitized HTML (post on_render)</p>",
    "plain_text": "Plain text fallback",
    "author": "Alice",
    "published_at": 1732492800,
    "media": []
  },
  "feed": { "id": 1, "title": "Blog", "url": "https://example.com/rss.xml" },
  "route": { "id": 3, "name": "Alerts", "locale": "en", "timezone": "UTC" },
  "checkpoint": null
}
```

**Response body (`200 OK`)**:

```json
{
  "outcome": "delivered",
  "external_id": "msg-123",
  "checkpoint": { "cursor": "optional-state" },
  "diagnostic": "Optional human-readable note"
}
```

- `outcome`: `"delivered"` | `"retry"` (with optional `retry_after_secs`) | `"permanent_failure"` | `"deferred"` (with optional `next_window_at` or `retry_after_secs`).
- Non-2xx HTTP fallback mapping: HTTP **`429`** → `retry` (honors `Retry-After` and triggers credential-group cooldown); **`5xx`** and network/timeout errors → `retry`; other **`4xx`** → `permanent_failure`.

### `POST /hooks/on_delivered` Contract

When a delivery succeeds, FeedCourier inserts one row into `plugin_events` **inside the same SQLite transaction** as marking `delivery_tasks.status = 'delivered'` for every enabled bridge plugin subscribed to `on_delivered` on that route or channel. A dedicated background worker dispatches events with `X-FeedCourier-Idempotency-Key` and retries failures with exponential backoff (`15s, 30s, 60s, 120s`, max **5 attempts**) without affecting delivery status or channel quotas.

---

## 2. Starter Kit & Reference Channel (`FeedCourier-Plugins`)

Start a new channel plugin in seconds from **[`ElleryHe/FeedCourier-Plugins`](https://github.com/ElleryHe/FeedCourier-Plugins)**:

- **`channels/channel-template/`**: Zero-dependency **Node.js 24** template (`node:http` + `node:crypto`), implementing HMAC-SHA256 verification, all 4 endpoints, `mapUpstreamStatusToOutcome()`, a `Dockerfile`, and `node:test` unit tests.
- **`channels/ntfy/`**: Production-ready reference channel plugin for `ntfy.sh` / self-hosted ntfy servers.

```bash
cd FeedCourier-Plugins/channels/channel-template
node --test server.test.mjs
BRIDGE_SECRET="dev-secret" PORT=8787 node server.mjs
```

---

## 3. Marketplace Manifest Schema (`schema/manifest.schema.json`)

Each plugin listing lives at `registry/<id>.json` in `ElleryHe/FeedCourier-Plugins` (Draft 2020-12):

| Field | Type | Description |
| :--- | :--- | :--- |
| `id`, `name`, `version`, `api_version` | `string` | Unique kebab-case ID (`registry/<id>.json`), display name, SemVer version, `"1"`. |
| `kind` | `string` | `"builtin"` \| `"processor"` \| `"channel"` \| `"external"` \| `"coming_soon"`. |
| `runtime` | `string` | Optional `"builtin"` \| `"bridge"`. |
| `min_core` | `string` | Minimum compatible FeedCourier version (checked by WebUI & API before install). |
| `category`, `icon`, `description` | `string` | Marketplace filtering and card metadata. |
| `homepage`, `repository` | `string` | Strict `https://` documentation and source links. |
| `image`, `compose_snippet` | `string` | Optional container image and Docker Compose YAML snippet for bridge plugins. |
| `hooks` | `string[]` | `"on_render"`, `"on_ingest"`, `"on_delivered"` (optional empty array `[]` for `kind: "channel"`). |
| `permissions` | `string[]` | Declared capabilities (`["network"]`). |
| `account_schema`, `destination_schema`, `capabilities` | `object` | Declarative JSON Schemas driving WebUI forms (`format: "password"` and `x-secret: true` fields are encrypted & masked automatically). |

### Submitting to `ElleryHe/FeedCourier-Plugins`

1. Fork `ElleryHe/FeedCourier-Plugins`, add `registry/<id>.json` (and optional channel source/tests under `channels/<id>/`).
2. Validate locally with Node.js 24+:
   ```bash
   node scripts/build-index.mjs --check
   node --test channels/*/*.test.mjs
   ```
3. **Do not manually edit `index.json` in your PR.** GitHub Actions validates PRs and automatically rebuilds `index.json` upon merge to `main`.
