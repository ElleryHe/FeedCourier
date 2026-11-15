---
description: "REST API reference for FeedCourier (/api/v1/*) covering authentication, overview metrics, feeds, OPML import/export, channels, routes, tasks, backups, and MCP tokens."
---

# REST API Reference (`/api/v1/*`)

The embedded React 19 WebUI communicates with the Rust backend through a JSON REST API mounted under **`/api/v1/*`** (`src/api/mod.rs`, `src/api/auth.rs`, `src/api/dto.rs`, `src/api/handlers.rs`). These endpoints can also be called directly from automation scripts or `curl`.

---

## Authentication & Session Cookie (`fc_session`)

- **Public Endpoints**: `GET /api/v1/health`, `GET /api/v1/auth/status`, `POST /api/v1/auth/setup`, `POST /api/v1/auth/login`, and `POST /api/v1/auth/logout`.
- **Protected Endpoints**: All other `/api/v1/*` routes require a valid **`fc_session`** HttpOnly cookie (`SameSite=Lax`, `Path=/`, 30-day TTL / `2,592,000` seconds) issued by `/api/v1/auth/setup` or `/api/v1/auth/login`.
- **Error Envelope**: Every non-2xx response returns a structured JSON error:
  ```json
  {
    "error": "validation_error",
    "message": "poll_interval_secs must be greater than 0"
  }
  ```

---

## 1. Health & Authentication

### `GET /api/v1/health`
Liveness probe and AGPLv3 compliance metadata endpoint.
- **Response (`200 OK`)**:
  ```json
  {
    "name": "FeedCourier",
    "version": "0.1.2",
    "license": "AGPL-3.0",
    "repo_url": "https://github.com/ElleryHe/FeedCourier",
    "status": "ok"
  }
  ```

### `GET /api/v1/auth/status`
Returns whether initial admin password setup is required, whether the caller's `fc_session` cookie is authenticated, and build metadata.
- **Response (`200 OK`)**: `AuthStatusResponse` (`setup_required`, `authenticated`, `version`, `repo_url`, `license`).

### `POST /api/v1/auth/setup`
Sets the initial administrator password on first boot (only succeeds when `setup_required == true`; minimum 8 characters), hashes it with `Argon2id`, and sets the `fc_session` cookie.
- **Body**: `{"password": "your-strong-passphrase"}`

### `POST /api/v1/auth/login`
Verifies the administrator password against the stored `Argon2id` hash and sets the `fc_session` cookie.
- **Body**: `{"password": "your-strong-passphrase"}`

### `POST /api/v1/auth/logout`
Deletes the active session from `sessions` and clears the `fc_session` cookie.

---

## 2. Overview & Maintenance

### `GET /api/v1/overview`
Returns aggregated counts across feeds, entries, outbox tasks, and active channel cooldowns.
- **Response (`200 OK`)**:
  ```json
  {
    "feeds": { "active": 12, "backoff": 0, "circuit_open": 0, "paused": 1 },
    "entries": { "total_fingerprints": 1840, "active_payloads": 24 },
    "tasks": {
      "queued": 2,
      "delivering": 0,
      "retrying": 0,
      "deferred": 0,
      "dead_letter": 0,
      "delivered": 1420
    },
    "active_cooldowns": []
  }
  ```

### `POST /api/v1/maintenance/gc`
Runs the `NOT EXISTS` payload garbage collector immediately, purging expired entry bodies and sessions and running `PRAGMA optimize`.
- **Response (`200 OK`)**: `{"purged_entries": 18}`

### `GET /api/v1/maintenance/backup`
Executes an online `VACUUM INTO` snapshot and streams the resulting SQLite database file as a binary attachment.

---

## 3. Feeds & OPML 2.0

| Method & Path | Description |
| :--- | :--- |
| `GET /api/v1/feeds` | List all subscribed feeds (`FeedDto[]`). |
| `POST /api/v1/feeds` | Create a feed (`CreateFeedRequest`) and run an immediate initial poll (`silent_init: true` by default; rolls back the feed row automatically if the initial poll fails with `SsrfBlocked`). |
| `PATCH /api/v1/feeds/{id}` | Update feed URL, title, tags, `poll_interval_secs`, `fetch_full_content`, `use_proxy`, `silent_init`, or `health_status`. |
| `DELETE /api/v1/feeds/{id}` | Delete a feed and cascade-delete its entries and tasks. |
| `POST /api/v1/feeds/{id}/poll` | Reset circuit-breaker state and trigger an immediate incremental poll (`PollFeedResponse`). |
| `POST /api/v1/feeds/preview` | Dry-run fetch and parse a feed URL (`FeedPreviewRequest`) with optional Readability and content filter rules. |
| `POST /api/v1/feeds/opml/import` | Bulk-import feeds from OPML 2.0 XML (`{"opml_xml": "...", "silent_init": true}`). |
| `GET /api/v1/feeds/opml/export` | Export all subscribed feeds and their tags as an OPML 2.0 XML document. |

---

## 4. Trusted Delivery Channels & Notion Onboarding

| Method & Path | Description |
| :--- | :--- |
| `GET /api/v1/channels` | List all channels (`ChannelDto[]`) with secrets redacted (`has_password`, `bot_token_hint`, `secret_hint`; see [Security & Isolation](/reference/security)). |
| `POST /api/v1/channels` | Create and encrypt a new `smtp`, `telegram`, or `notion` channel (`CreateChannelRequest`). Automatically probes Telegram Bot API capabilities on creation. |
| `PATCH /api/v1/channels/{id}` | Secret-preserving partial update (`UpdateChannelRequest`). Omitting `password` / `bot_token` / `integration_secret` preserves the existing encrypted credential intact. |
| `DELETE /api/v1/channels/{id}` | Delete a channel and cascade-delete its routes and tasks. |
| `POST /api/v1/channels/{id}/test` | Send an interactive test email, Telegram message, or Notion verification page (`TestChannelResponse`). |
| `POST /api/v1/channels/notion/init-db` | Provision a new 7-column Notion database under a parent page (`Notion-Version: 2026-03-11`, `initial_data_source.properties`). |
| `POST /api/v1/channels/notion/inspect-schema` | Inspect an existing Notion `data_source_id` (or database URL) and optionally auto-patch missing standard columns (`patch_missing: true`). |

---

## 5. Routes & Outbox Tasks

| Method & Path | Description |
| :--- | :--- |
| `GET /api/v1/routes` | List all delivery routes (`RouteDto[]`). |
| `POST /api/v1/routes` | Create a delivery route (`CreateRouteRequest`) binding `feed_ids` / `match_tags` and content filters to a `channel_id`. |
| `PATCH /api/v1/routes/{id}` | Update a delivery route (`UpdateRouteRequest`). |
| `DELETE /api/v1/routes/{id}` | Delete a delivery route. |
| `GET /api/v1/tasks` | List outbox tasks (`?status=dead_letter&limit=50`, accepts `queued`, `delivering`, `retrying`, `deferred`, `dead_letter`, `delivered`, or `all`). |
| `POST /api/v1/tasks/retry` | Requeue specific tasks (`{"task_ids": [1, 2]}`) or all dead-letter tasks (`{"all_dead_letter": true}`) and wake the outbox dispatcher. |

---

## 6. System Settings & MCP Bearer Tokens

| Method & Path | Description |
| :--- | :--- |
| `GET /api/v1/settings` | Get runtime network settings (`outbound_proxy_url` and `ssrf_allowlist`). |
| `PUT /api/v1/settings` | Hot-update `outbound_proxy_url` and `ssrf_allowlist` in `system_settings` and reload `ProtectedHttpClient`. |
| `GET /api/v1/mcp-tokens` | List issued MCP Bearer token metadata (`id`, `name`, `token_prefix`, `scope`, `last_used_at`, `created_at`). |
| `POST /api/v1/mcp-tokens` | Issue a new `readonly` or `admin` MCP Bearer token (`CreateMcpTokenResponse`, returning the one-time `plaintext_token` and `client_config_json`). |
| `DELETE /api/v1/mcp-tokens/{id}` | Revoke an MCP Bearer token immediately. |
