---
description: "REST API reference for FeedCourier (/api/v1/*) covering username/password and 2FA authentication, account security, overview metrics, feeds (clear cache, debug push), OPML import/export, channels, routes (language, time windows), tasks, global queue control, system settings (timezone), backups, and MCP tokens."
---

# REST API Reference (`/api/v1/*`)

The embedded React 19 WebUI communicates with the Rust backend through a JSON REST API mounted under **`/api/v1/*`** (`src/api/mod.rs`, `src/api/auth.rs`, `src/api/dto.rs`, `src/api/handlers.rs`). These endpoints can also be called directly from automation scripts or `curl`.

---

## Authentication & Session Cookie (`fc_session`)

- **Public Endpoints**: `GET /api/v1/health`, `GET /api/v1/auth/status`, `POST /api/v1/auth/setup`, `POST /api/v1/auth/login`, `POST /api/v1/auth/login/2fa`, and `POST /api/v1/auth/logout`.
- **Protected Endpoints**: All other `/api/v1/*` routes require a valid **`fc_session`** HttpOnly cookie (`SameSite=Lax`, `Path=/`, 30-day TTL / `2,592,000` seconds) issued by `/api/v1/auth/setup`, `/api/v1/auth/login` (when 2FA is off), or `/api/v1/auth/login/2fa`.
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
    "version": "0.1.5",
    "license": "AGPL-3.0",
    "repo_url": "https://github.com/ElleryHe/FeedCourier",
    "status": "ok"
  }
  ```

### `GET /api/v1/auth/status`
Returns whether initial admin setup is required, whether the caller's `fc_session` cookie is authenticated, and build metadata. The username is never exposed.
- **Response (`200 OK`)**: `AuthStatusResponse` (`setup_required`, `authenticated`, `version`, `repo_url`, `license`).

### `POST /api/v1/auth/setup`
Creates the initial administrator account on first boot (only succeeds when `setup_required == true`; password minimum 8 characters), hashes the password with `Argon2id`, and sets the `fc_session` cookie.
- **Body**: `{"username": "Courier", "password": "your-strong-passphrase"}` — `username` is optional (defaults to `Courier`; 1–64 characters, trimmed, no control characters).
- **Response (`200 OK`)**: `{"ok": true}`

### `POST /api/v1/auth/login`
Verifies the username and password. With 2FA disabled it sets the `fc_session` cookie directly; with 2FA enabled it returns a short-lived token for the second step instead.
- **Body**: `{"username": "Courier", "password": "your-strong-passphrase"}`
- **Response (`200 OK`)**:
  - 2FA off: `{"ok": true, "mfa_required": false}` plus `Set-Cookie: fc_session`.
  - 2FA on, credentials valid: `{"ok": false, "mfa_required": true, "mfa_token": "..."}` with no cookie. The `mfa_token` is valid for 5 minutes.
- **Errors**: `401` `{"error": "unauthorized", "code": "unauthorized", "message": "..."}` for a wrong username or password (the message is generic and does not reveal which field was wrong); `429` with a `Retry-After` header after too many failed attempts.

### `POST /api/v1/auth/login/2fa`
Completes a 2FA sign-in and sets the `fc_session` cookie.
- **Body**: `{"mfa_token": "...", "code": "123456"}` — `code` is either a 6-digit TOTP code or a recovery code (`XXXX-XXXX-XXXX`, case-insensitive, dashes optional). A recovery code can be used once.
- **Response (`200 OK`)**: `{"ok": true, ...}`

### `POST /api/v1/auth/logout`
Deletes the active session from `sessions` and clears the `fc_session` cookie.

---

## 2. Account & Security

All account endpoints require a signed-in session.

| Method & Path | Description |
| :--- | :--- |
| `GET /api/v1/account` | Returns `AccountDto`: `{ "username": "Courier", "username_env_managed": false, "totp_enabled": false, "recovery_codes_remaining": 0 }`. `username_env_managed: true` means the `ADMIN_USERNAME` environment variable overrides the username at every restart. |
| `PATCH /api/v1/account` | Change the username and/or password. Body: `{ "current_password": "...", "new_username"?: "...", "new_password"?: "...", "code"?: "..." }`. `code` (TOTP or recovery code) is **required** when `new_password` is set and 2FA is enabled. Changing the password revokes all other sessions. Returns `AccountDto`. |
| `POST /api/v1/account/2fa/setup` | Body: `{ "current_password": "..." }`. Generates a pending TOTP secret (not active yet) and returns `{ "secret": "<base32>", "otpauth_url": "otpauth://...", "qr_svg": "<svg ...>" }`. Returns `409` if 2FA is already enabled. |
| `POST /api/v1/account/2fa/enable` | Body: `{ "code": "123456" }` (a TOTP code from the pending secret). Activates 2FA and returns `{ "recovery_codes": [ ...10 codes ] }`, **shown only once**. |
| `POST /api/v1/account/2fa/disable` | Body: `{ "current_password": "...", "code": "..." }`. Disables 2FA and removes the secret and recovery codes. Returns `AccountDto`. |
| `POST /api/v1/account/2fa/recovery-codes` | Body: `{ "current_password": "..." }`. Replaces the recovery codes with 10 new ones (only when 2FA is enabled) and returns `{ "recovery_codes": [...] }`. |

Re-authentication failures on these endpoints (wrong `current_password`, wrong `code`) return `403` rather than `401`, so clients do not mistake them for an expired session; repeated failures are throttled with `429`.

If the authenticator and recovery codes are lost, restart once with `RESET_2FA=true` (see [Lost Authenticator](/guide/operations#lost-authenticator-reset-2fa)).

---

## 3. Overview & Maintenance

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

## 4. Feeds & OPML 2.0

| Method & Path | Description |
| :--- | :--- |
| `GET /api/v1/feeds` | List all subscribed feeds (`FeedDto[]`). |
| `POST /api/v1/feeds` | Create a feed (`CreateFeedRequest`) and run an immediate initial poll (`silent_init: true` by default; rolls back the feed row automatically if the initial poll fails with `SsrfBlocked`). |
| `PATCH /api/v1/feeds/{id}` | Update feed URL, title, tags, `poll_interval_secs`, `fetch_full_content`, `use_proxy`, `silent_init`, or `health_status`. |
| `DELETE /api/v1/feeds/{id}` | Delete a feed and cascade-delete its entries and tasks. |
| `POST /api/v1/feeds/{id}/poll` | Reset circuit-breaker state and trigger an immediate incremental poll (`PollFeedResponse`). |
| `POST /api/v1/feeds/{id}/clear-cache` | Delete all entries, fingerprints, and tasks of the feed, clear `ETag` / `Last-Modified` and failure state, and set `silent_init = false`. Body: `{ "poll_now"?: true }` (default `true`). Returns `{ "deleted_entries": 42, "poll": PollFeedResponse \| null }`. With `poll_now` the full backlog is queued oldest-first immediately; otherwise on the next scheduled poll. |
| `POST /api/v1/feeds/preview` | Dry-run fetch and parse a feed URL (`FeedPreviewRequest`) with optional Readability and content filter rules. |
| `POST /api/v1/feeds/preview/push` | Debug push of one entry to a channel (see below). |
| `POST /api/v1/feeds/opml/import` | Bulk-import feeds from OPML 2.0 XML (`{"opml_xml": "...", "silent_init": true}`). |
| `GET /api/v1/feeds/opml/export` | Export all subscribed feeds and their tags as an OPML 2.0 XML document. |

### `POST /api/v1/feeds/preview/push`
Delivers a single feed entry to a channel for testing. It is **stateless**: no fingerprints are written, nothing enters the outbox, and route filters, time windows, and quotas are not applied. Notion pages use a `test-` prefixed `FC-ID`.
- **Body**:
  ```json
  {
    "url": "https://example.com/feed.xml",
    "feed_id": 4,
    "fingerprint_sha256": "…",
    "fetch_full_content": false,
    "use_proxy": false,
    "channel_id": 2,
    "options": { "telegram_mode": "rich_message", "link_preview": true, "locale": "en" }
  }
  ```
  `url` and `channel_id` are required. `feed_id` is optional (pass it when previewing a subscribed feed). Omit `fingerprint_sha256` to push the latest item. `options` is optional.
- **Response (`200 OK`)**: `{ "ok": true, "message": "...", "entry_title": "...", "entry_url": "https://..." \| null }`. Downstream delivery failures return `ok: false` with the diagnostic in `message` (still HTTP `200`).

---

## 5. Trusted Delivery Channels & Notion Onboarding

| Method & Path | Description |
| :--- | :--- |
| `GET /api/v1/channels` | List all channels (`ChannelDto[]`) with secrets redacted (`has_password`, `bot_token_hint`, `secret_hint`; see [Security & Isolation](/reference/security)). |
| `POST /api/v1/channels` | Create and encrypt a new `smtp`, `telegram`, or `notion` channel (`CreateChannelRequest`). Automatically probes Telegram Bot API capabilities on creation. |
| `PATCH /api/v1/channels/{id}` | Secret-preserving partial update (`UpdateChannelRequest`). Omitting `password` / `bot_token` / `integration_secret` preserves the existing encrypted credential intact. |
| `DELETE /api/v1/channels/{id}` | Delete a channel and cascade-delete its routes and tasks. |
| `POST /api/v1/channels/{id}/test` | Send an interactive test email, Telegram message, or Notion verification page (`TestChannelResponse`). Optional body `{ "locale"?: "en" \| "zh-CN" }` selects the test message language (the WebUI sends its current interface language). |
| `POST /api/v1/channels/notion/init-db` | Provision a new 7-column Notion database under a parent page (`Notion-Version: 2026-03-11`, `initial_data_source.properties`). |
| `POST /api/v1/channels/notion/inspect-schema` | Inspect an existing Notion `data_source_id` (or database URL) and optionally auto-patch missing standard columns (`patch_missing: true`). |

---

## 6. Routes, Outbox Tasks & Queue Control

| Method & Path | Description |
| :--- | :--- |
| `GET /api/v1/routes` | List all delivery routes (`RouteDto[]`). |
| `POST /api/v1/routes` | Create a delivery route (`CreateRouteRequest`) binding `feed_ids` / `match_tags` and content filters to a `channel_id`, with route `options` (below). |
| `PATCH /api/v1/routes/{id}` | Update a delivery route (`UpdateRouteRequest`), including `options`. |
| `DELETE /api/v1/routes/{id}` | Delete a delivery route. |
| `GET /api/v1/tasks` | List outbox tasks (`?status=dead_letter&limit=50`, accepts `queued`, `delivering`, `retrying`, `deferred`, `dead_letter`, `delivered`, or `all`). |
| `POST /api/v1/tasks/retry` | Requeue specific tasks (`{"task_ids": [1, 2]}`) or all dead-letter tasks (`{"all_dead_letter": true}`) and wake the outbox dispatcher. |
| `GET /api/v1/tasks/queue` | Global outbox state: `{ "paused": false, "resume_at": null }`. A pause that has already auto-resumed is reported as `paused: false`. |
| `PUT /api/v1/tasks/queue` | Body `{ "action": "pause", "resume_in_secs"?: 3600, "resume_at"?: "2026-01-01T08:00:00Z" }` or `{ "action": "resume" }`. Returns the same shape as `GET`. While paused, feeds are still polled and tasks accumulate; on resume all due tasks across feeds are delivered oldest-first by `COALESCE(published_at, discovered_at)`. |


### Route `options`

`options` appears in route create, update, list, and get payloads:

```json
{
  "telegram_mode": "rich_message",
  "link_preview": true,
  "locale": "en",
  "active_windows": [{ "start": "22:00", "end": "06:00" }],
  "active_weekdays": [1, 2, 3, 4, 5]
}
```

| Field | Description |
| :--- | :--- |
| `telegram_mode` | `"rich_message"` (default), `"compact_card"`, or `"media_mode"`. |
| `link_preview` | Telegram link preview toggle. |
| `locale` | Delivery language of this route (`"en"` or `"zh-CN"`). The WebUI pre-fills the current interface language; routes without it inherit the legacy `settings.locale`. |
| `active_windows` | Daily delivery windows (`HH:MM`) in the system timezone. `end` earlier than `start` wraps midnight; `start == end` means the whole day; an empty list means always. |
| `active_weekdays` | `1` (Monday) – `7` (Sunday); empty means every day. For a window wrapping midnight, the weekday of the day the window starts is used. |

Tasks outside the window or weekdays are **still queued**: their status stays `queued` / `deferred` and `next_attempt_at` is set to the next window start.

---

## 7. System Settings & MCP Bearer Tokens

| Method & Path | Description |
| :--- | :--- |
| `GET /api/v1/settings` | Get runtime settings: `{ "outbound_proxy_url", "ssrf_allowlist", "timezone": "America/Los_Angeles", "locale" }`. `timezone` is an IANA name (default from `TIMEZONE` / `TZ`). `locale` is a **legacy fallback** delivery language for routes without their own `options.locale`; the WebUI no longer writes it or uses it as the interface language. |
| `PUT /api/v1/settings` | Hot-update any subset of `timezone`, `outbound_proxy_url`, and `ssrf_allowlist`. Network fields are applied (and active runtime clients reloaded) only when at least one of them is present. An invalid IANA timezone returns `400`. |
| `GET /api/v1/mcp-tokens` | List issued MCP Bearer token metadata (`id`, `name`, `token_prefix`, `scope`, `last_used_at`, `created_at`). |
| `POST /api/v1/mcp-tokens` | Issue a new `readonly` or `admin` MCP Bearer token (`CreateMcpTokenResponse`, returning the one-time `plaintext_token` and `client_config_json`). |
| `DELETE /api/v1/mcp-tokens/{id}` | Revoke an MCP Bearer token immediately. |
