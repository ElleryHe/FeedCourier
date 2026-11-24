---
description: "REST API reference for FeedCourier (/api/v1/*) covering authentication, 2FA, feeds, channels, destinations (409 guard), SMTP per-recipient verification, plugins & marketplace, routes, tasks, settings (PUBLIC_URL), MCP tokens, and public /u/{token} & /v/{token} compliance endpoints."
---

# REST API Reference (`/api/v1/*`)

The embedded React 19 WebUI communicates with the Rust backend through a JSON REST API mounted under **`/api/v1/*`** (`src/api/mod.rs`, `src/api/auth.rs`, `src/api/dto.rs`, `src/api/handlers.rs`). These endpoints can also be called directly from automation scripts or `curl`.

---

## Authentication & Session Cookie (`fc_session`)

- **Public Endpoints**: `GET /api/v1/health`, `GET /api/v1/auth/status`, `POST /api/v1/auth/setup`, `POST /api/v1/auth/login`, `POST /api/v1/auth/login/2fa`, `POST /api/v1/auth/logout`, plus unauthenticated SMTP compliance endpoints `GET`/`POST /u/{token}` and `GET`/`POST /v/{token}` (registered before the SPA fallback).
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
    "version": "0.3.1",
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
| `POST /api/v1/feeds` | Returns **`201 Created`**. Create a feed (`CreateFeedRequest`) and run an immediate initial poll (`silent_init: true` by default; rolls back the feed row automatically if the initial poll fails with `SsrfBlocked`). |
| `PATCH /api/v1/feeds/{id}` | Update feed URL, title, tags, `poll_interval_secs`, `fetch_full_content`, `use_proxy`, `silent_init`, or `health_status`. |
| `DELETE /api/v1/feeds/{id}` | Delete a feed and cascade-delete its entries and tasks. |
| `POST /api/v1/feeds/{id}/poll` | Reset circuit-breaker state and trigger an immediate incremental poll (`PollFeedResponse`). |
| `POST /api/v1/feeds/{id}/clear-cache` | Delete all entries, fingerprints, and tasks of the feed, clear `ETag` / `Last-Modified` and failure state, and set `silent_init = false`. Body: `{ "poll_now"?: true }` (default `true`). Returns `{ "deleted_entries": 42, "poll": PollFeedResponse \| null }`. With `poll_now` the full backlog is queued oldest-first immediately; otherwise on the next scheduled poll. |
| `POST /api/v1/feeds/preview` | Dry-run fetch and parse a feed URL (`FeedPreviewRequest`) with optional Readability and content filter rules. |
| `POST /api/v1/feeds/preview/push` | Debug push of one entry to a channel (see below). |
| `POST /api/v1/feeds/opml/import` | Bulk-import feeds from OPML 2.0 XML (`{"opml_xml": "...", "silent_init": true}`). |
| `GET /api/v1/feeds/opml/export` | Export all subscribed feeds and their tags as an OPML 2.0 XML document. |

### `POST /api/v1/feeds/preview/push`
Delivers a single feed entry to a trusted destination for testing. It is **stateless**: no fingerprints are written, nothing enters the outbox, and route filters, time windows, and quotas are not applied. Notion pages use a `test-` prefixed `FC-ID`. For SMTP destinations, debug push strictly delivers **only to `verified` recipients** via isolated single-`To:` envelopes with RFC 8058 compliance headers (returning an error when `0` verified recipients exist).
- **Body**:
  ```json
  {
    "url": "https://example.com/feed.xml",
    "feed_id": 4,
    "fingerprint_sha256": "…",
    "fetch_full_content": false,
    "use_proxy": false,
    "destination_id": 5,
    "options": {
      "telegram_mode": "rich_message",
      "link_preview": true,
      "locale": "en",
      "timezone": "Asia/Shanghai",
      "plugins": {
        "link-rewrite": { "provider_id": 1 }
      }
    }
  }
  ```
  `url` and `destination_id` are required (legacy `channel_id` is still accepted as a fallback when that account owns **exactly 1 destination**, returning a validation error if multiple destinations exist). `feed_id` is optional (pass it when previewing a subscribed feed). Omit `fingerprint_sha256` to push the latest item. `options` is optional.
- **Response (`200 OK`)**: `{ "ok": true, "message": "...", "entry_title": "...", "entry_url": "https://..." | null }`. Downstream delivery failures return `ok: false` with the diagnostic in `message` (still HTTP `200`).

---

## 5. Trusted Delivery Channels (Accounts), Destinations & Notion Onboarding

| Method & Path | Description |
| :--- | :--- |
| `GET /api/v1/channels` | List all channel accounts (`ChannelDto[]`) with secrets redacted (`has_password`, `bot_token_hint`, `secret_hint`, or schema-driven `has_<field>` / `<field>_hint` for `plugin` channels) and nested `destinations: ChannelDestinationDto[]` (each destination includes `enabled` and `auto_paused: bool`; SMTP destinations also include `smtp_summary` recipient counts; see [Security & Isolation](/reference/security)). |
| `POST /api/v1/channels` | Returns **`201 Created`**. Create and encrypt a new `smtp`, `telegram`, `notion`, or `plugin` **Channel Account** (`CreateChannelRequest`, including optional SMTP `sender_identity`, optional `name` auto-derived from Telegram `getMe`, SMTP `from_address`, Notion `/v1/users/me`, or channel plugin manifest name, and optional `initial_destination`). Automatically probes Telegram Bot API capabilities on creation (Telegram `api_base_url` enforces SSRF validation and returns `403 ssrf_blocked` on disallowed targets). For `kind: "plugin"`, validates account config via signed bridge `POST /validate` (`scope: "account"`). SMTP accounts use direct TCP/TLS connections (outbound HTTP/SOCKS proxies do not apply). |
| `PATCH /api/v1/channels/{id}` | Secret-preserving partial update for account credentials, `sender_identity`, proxy (HTTP-based channels), pacing, quotas, or enabled flag (`UpdateChannelRequest`). Omitting `password` / `bot_token` / `integration_secret` (or schema-marked secret fields on `plugin` channels) preserves the existing encrypted credential intact; passing `"password": null` on an SMTP account explicitly clears an optional SMTP password. |
| `DELETE /api/v1/channels/{id}` | Atomically delete a channel account. If any routes still reference destinations under this account and `?cascade=true` is not set, returns **`409 Conflict`** with `{ "error", "code": "conflict", "message", "referencing_routes": [{ "id", "name" }] }` (matching `DELETE /api/v1/destinations/{id}`). Passing **`?cascade=true`** deletes the account together with its destinations, referencing routes, and tasks in a single transaction. |
| `POST /api/v1/channels/{id}/test` | When `destination_id` is omitted, verifies account-level credentials (for SMTP, verifies connectivity/auth and sends a self-test email **only to `from_address`**; for Telegram, runs `getMe` + `sendRichMessage` probe; for Notion, calls `GET /v1/users/me`; for `plugin`, calls signed bridge `POST /test` with `scope: "account"`). Passing `{ "destination_id": 5, "locale"?: "en" \| "zh-CN" }` runs a destination test (same compliance rules as `POST /api/v1/destinations/{id}/test`). |
| `GET /api/v1/channels/{id}/destinations` | List all destinations belonging to channel account `{id}` (`ChannelDestinationDto[]`, including `auto_paused: bool`, schema-redacted secret fields on `plugin` destinations, and `smtp_summary` on SMTP destinations). |
| `POST /api/v1/channels/{id}/destinations` | Returns **`201 Created`**. Create and encrypt a new destination under channel account `{id}` (`CreateDestinationRequest`; optional `name` is auto-derived from Telegram `getChat`, masked SMTP first non-empty recipient such as `a***e@example.com`, Notion database title, or primary `plugin` destination field). For `plugin` accounts, validates destination config via signed bridge `POST /validate` (`scope: "destination"`). |
| `PATCH /api/v1/destinations/{id}` | Partially update a destination's label, target config (`DestinationPatch`), or `enabled` state (manually toggling `enabled` clears `auto_paused = false`; secret fields on `plugin` destinations are preserved when omitted or empty). Modifying SMTP `to_addresses` syncs `smtp_recipients` (new addresses start `unverified`, removed addresses are deleted, retained addresses preserve status). |
| `DELETE /api/v1/destinations/{id}` | Atomically delete a destination. If any routes still reference this destination and `?cascade=true` is not set, returns **`409 Conflict`** with `{ "error", "code": "conflict", "message", "referencing_routes": [{ "id", "name" }] }`. Passing **`?cascade=true`** deletes the destination together with its referencing routes and tasks. |
| `POST /api/v1/destinations/{id}/test` | Send an interactive test email, Telegram message, Notion verification page, or signed bridge `POST /test` (`scope: "destination"`) to destination `{id}` (`TestChannelResponse`). Optional body `{ "locale"?: "en" \| "zh-CN" }` selects the test message language. For SMTP destinations, strictly delivers **only to `verified` recipients** via isolated single-`To:` envelopes with RFC 8058 compliance headers, returning an error when `0` verified recipients exist. |
| `POST /api/v1/channels/notion/init-db` | Provision a new 8-column Notion database under a parent page (`Notion-Version: 2026-03-11`, `initial_data_source.properties`, including optional `Short URL`). |
| `POST /api/v1/channels/notion/inspect-schema` | Inspect an existing Notion `data_source_id` (or database URL) and optionally auto-patch missing standard columns (`patch_missing: true`). |

### Plugin Channel Config Shape (`kind = "plugin"`)

When creating or updating an HTTP Bridge channel account (`ChannelKind::Plugin`), bind `plugin_id` on the account and pass schema-defined fields inside `config`:

```json
{
  "name": "Self-hosted ntfy",
  "kind": "plugin",
  "config": {
    "kind": "plugin",
    "plugin_id": "ntfy",
    "config": {
      "server_url": "https://ntfy.example.com",
      "access_token": "tk_..."
    }
  },
  "initial_destination": {
    "name": "Release Alerts",
    "config": {
      "kind": "plugin",
      "config": {
        "topic": "feedcourier-alerts",
        "priority": "high"
      }
    }
  }
}
```

Secret fields declared in the bridge manifest's `account_schema` / `destination_schema` (such as `"format": "password"` or token/secret keys) are encrypted at rest with ChaCha20-Poly1305 and automatically redacted on `GET` responses as `has_<field>: true|false` and `<field>_hint: "tk_••••"` (omitting plaintext).

### SMTP Per-Recipient Compliance (`/api/v1/destinations/{id}/recipients*`)

| Method & Path | Description |
| :--- | :--- |
| `GET /api/v1/destinations/{id}/recipients` | Decrypt the SMTP destination config and return `SmtpRecipientDto[]` with `id`, `email`, `status` (`unverified` / `pending` / `verified` / `unsubscribed` / `bounced`), `code_expires_at`, `code_attempts`, `last_code_sent_at`, consent/revocation audit fields (`consent_method`, `consented_at`, `revoked_method`, `revoked_at`), and redacted `bounce_diagnostic`. |
| `POST /api/v1/destinations/{id}/recipients/{rid}/send-verification` | Atomically reserves the rate-limit slot in `write_pool` and immediately sends a verification email containing a 6-digit code (30-minute TTL, max 5 wrong attempts) plus an optional `/v/{token}` confirmation link when `PUBLIC_URL` is configured (bypasses outbox queue & quotas; rolls back reservation if SMTP send fails). Enforces a 60s resend cooldown and 5/hour cap, returning **`429 Too Many Requests`** with a **`Retry-After`** header (seconds remaining). Returns `SendVerificationResponse`. |
| `POST /api/v1/destinations/{id}/recipients/{rid}/verify` | Body `{ "code": "123456" }`. Verifies the 6-digit code in constant time, transitions the recipient to `verified` (`consent_method: "code"`), automatically resumes the destination (`enabled = true, auto_paused = false`) if it was `auto_paused`, and immediately wakes any deferred outbox tasks for this destination. Reaching 5 wrong attempts atomically clears the active code (invalidating prior `/v/{token}` links with `410 Gone`). |
| `POST /api/v1/destinations/{id}/recipients/{rid}/reactivate` | Resets an `unsubscribed` or `bounced` recipient back to `unverified` so it can be re-verified before receiving deliveries again. |
| `POST /api/v1/destinations/{id}/recipients/{rid}/unsubscribe` | Administrator-initiated unsubscribe (`status: "unsubscribed"`, `revoked_method: "admin"`). |

---

## 5b. Plugins & Marketplace (`/api/v1/plugins*`)

| Method & Path | Description |
| :--- | :--- |
| `GET /api/v1/plugins` | List installed plugins (`PluginDto[]`), including `source` / `runtime` (`builtin` \| `bridge`), `kind` (`processor` \| `channel`), `bridge_url`, `compatible` / `compatibility_reason`, `manifest` (with `instance_schema`, `route_option_schema`, `account_schema`, `destination_schema`, `capabilities`), `enabled`, `installed_version`, `settings`, `instance_count`, and redacted `instances: PluginInstanceDto[]`. |
| `POST /api/v1/plugins/bridges` | Register an out-of-process HTTP Bridge Plugin (`{ "bridge_url", "bridge_secret", "enabled"?: true, "dry_run"?: false }`). Probes signed `GET /manifest` (HMAC-SHA256) and validates Protocol v1 compatibility. With `dry_run: true`, returns **`200 OK`** (`BridgeManifestPreviewResponse`) without persisting; otherwise encrypts `bridge_secret`, persists the plugin, reloads the in-memory registry, and returns **`201 Created`** (`PluginDto`; returns **`409 Conflict`** if the manifest `id` is already installed). |
| `PATCH /api/v1/plugins/{id}` | Body `{ "enabled"?: bool, "settings"?: object, "bridge_url"?: string, "bridge_secret"?: string }`. Globally enable/disable a plugin or update plugin-level settings (disabling preserves route configs). For bridge plugins, also supports in-place `bridge_url` updates and `bridge_secret` rotation, performing a mandatory live signed `GET /manifest` probe and enforcing manifest `id`, `kind`, and Protocol v1 compatibility checks before persisting. |
| `POST /api/v1/plugins/{id}/refresh-manifest` | Re-fetch a registered bridge plugin's signed `GET /manifest` to update version, schemas, and capabilities. Strictly forbids `kind` mutation (`processor` ↔ `channel`) and syncs `capabilities_json` on all referencing `kind = 'plugin'` channels. |
| `DELETE /api/v1/plugins/{id}` | Delete a registered HTTP Bridge Plugin. Refuses built-in plugins with **`400 Bad Request`**. If still referenced by any `kind = 'plugin'` channels or routes (`options.plugins` / `options.plugin_order`) and `?cascade=true` is not set, returns **`409 Conflict`** with `{ "error", "code": "conflict", "message", "referencing_channels": [{ "id", "name" }], "referencing_routes": [{ "id", "name" }] }`. Passing **`?cascade=true`** deletes the plugin together with referencing channel accounts (and their destinations/routes/tasks) and strips processor references from remaining routes. |
| `GET /api/v1/plugins/{id}/instances` | List configured provider instances (`PluginInstanceDto[]`) for plugin `{id}` with secret fields redacted (`has_signature`, `signature_hint`, `has_password`, or schema-driven `has_<field>` / `<field>_hint`) and `cache_entries` count. |
| `POST /api/v1/plugins/{id}/instances` | Returns **`201 Created`**. Validate, encrypt (`ChaCha20-Poly1305`), and store a new provider instance (`CreatePluginInstanceRequest`: `{ "name", "config", "enabled"?: true }`). |
| `PATCH /api/v1/plugins/{id}/instances/{iid}` | Secret-preserving partial update of a provider instance (`UpdatePluginInstanceRequest`; omitting or sending `""` for secret fields preserves the stored secret). |
| `DELETE /api/v1/plugins/{id}/instances/{iid}` | Delete a provider instance. If referenced by any route (`routes.options_json`), returns **`409 Conflict`** with `referencing_routes: [{ "id", "name" }]`. |
| `POST /api/v1/plugins/{id}/instances/{iid}/test` | Execute a live connectivity test (for `link-rewrite`, shortens `https://feedcourier.ellery.me/` via `ProtectedHttpClient`; for bridge processors, calls signed `POST /test` with `scope: "instance"`). Returns `{ "ok": true, "message": "..." }`. |
| `DELETE /api/v1/plugins/{id}/instances/{iid}/cache` | Clear all `plugin_link_cache` rows for provider `{iid}`. Returns `{ "cleared_entries": N }`. |
| `GET /api/v1/plugins/market` | Fetch the plugin marketplace index via `ProtectedHttpClient` (6h in-memory cache; 60s negative-cache TTL on compiled snapshot fallback; preserves valid remote cache on transient refresh errors; pass `?refresh=true` to bypass cache). Returns `{ "source": "remote" \| "snapshot", "index_url", "generated_at", "plugins": MarketPluginItemDto[] }` annotated with `kind`, `installed`, and `enabled`, with `homepage`/`repository` restricted strictly to `http://` or `https://` URLs. |

---

## 6. Routes, Outbox Tasks & Queue Control

| Method & Path | Description |
| :--- | :--- |
| `GET /api/v1/routes` | List all delivery routes (`RouteDto[]`, including both `channel_id` and `destination_id`). |
| `POST /api/v1/routes` | Returns **`201 Created`**. Create a delivery route (`CreateRouteRequest`) binding `feed_ids` / `match_tags` and content filters to a trusted `destination_id` (primary; legacy `channel_id` fallback accepted only when that account owns exactly 1 destination), with route `options` (below). |
| `PATCH /api/v1/routes/{id}` | Update a delivery route (`UpdateRouteRequest`), including `destination_id` (or single-destination `channel_id`) and `options`. Changing the target destination atomically repoints pending `queued`/`deferred`/`retrying` tasks (clearing `checkpoint_json` and resetting `attempt_count = 0`; active `delivering` tasks are untouched). Changing `active_windows`, `active_weekdays`, or `timezone` resets `next_attempt_at = now` on `queued`/`deferred` tasks and wakes the outbox immediately. |
| `DELETE /api/v1/routes/{id}` | Delete a delivery route. |
| `GET /api/v1/tasks` | List outbox tasks (`DeliveryTaskDto[]`, including `destination_id` and `destination_name`; `?status=dead_letter&limit=50`, accepts `queued`, `delivering`, `retrying`, `deferred`, `dead_letter`, `delivered`, or `all`). |
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
  "timezone": "America/New_York",
  "active_windows": [{ "start": "22:00", "end": "06:00" }],
  "active_weekdays": [1, 2, 3, 4, 5],
  "plugin_order": ["link-rewrite", "ai-summary"],
  "plugins": {
    "link-rewrite": { "provider_id": 1 },
    "ai-summary": {},
    "optional-tagger": { "enabled": false }
  }
}
```

| Field | Description |
| :--- | :--- |
| `telegram_mode` | `"rich_message"` (default), `"compact_card"`, or `"media_mode"`. |
| `link_preview` | Telegram link preview toggle. |
| `locale` | Delivery language of this route (`"en"` or `"zh-CN"`). The WebUI pre-fills the current interface language; routes without it inherit the legacy `settings.locale`. |
| `timezone` | Optional per-route IANA timezone override (e.g., `"Asia/Shanghai"`, `"America/New_York"`; omit or pass `""` to inherit `settings.timezone`). Governs `active_windows` / `active_weekdays`, SMTP email article timestamps, and Telegram rich-message `<footer>` timestamps (`YYYY-MM-DD HH:MM (UTC±HH:MM)`). |
| `active_windows` | Daily delivery windows (`HH:MM`) in the **route timezone** (inheriting system timezone), evaluated with DST-safe boundaries. `end` earlier than `start` wraps midnight; only `00:00`–`00:00` (or an empty list `[]`) means all day—any other `start == end` window returns **`400 Bad Request`**. |
| `active_weekdays` | `1` (Monday) – `7` (Sunday); empty means every day. For a window wrapping midnight, the weekday of the day the window starts is used. |
| `plugin_order` | Optional ordered array of enabled processor plugin IDs controlling the `on_render` pipeline execution sequence (maximum **64** entries). Plugins present in `plugins` but omitted from `plugin_order` execute afterwards in ascending ID order. |
| `plugins` | Optional map of per-plugin route configuration keyed by plugin ID (e.g., `{"link-rewrite": {"provider_id": 1}}`). Supports `{}` for zero-argument processor plugins and `"enabled": false` to explicitly disable a plugin on this route. Validated on route create/update against installed processor plugins and provider instances. |

Tasks outside the window or weekdays are **still queued**: their status stays `queued` / `deferred` and `next_attempt_at` is set to the next window start.

---

## 7. System Settings & MCP Bearer Tokens

| Method & Path | Description |
| :--- | :--- |
| `GET /api/v1/settings` | Get runtime settings: `{ "outbound_proxy_url", "ssrf_allowlist", "timezone": "America/Los_Angeles", "locale", "public_url", "public_url_env_managed" }`. `timezone` is an IANA name (default from `TIMEZONE` / `TZ`). `public_url_env_managed: true` indicates `PUBLIC_URL` is locked by the environment variable. `locale` is a **legacy fallback** delivery language for routes without their own `options.locale`. |
| `PUT /api/v1/settings` | Hot-update any subset of `timezone`, `outbound_proxy_url`, `ssrf_allowlist`, and `public_url` (passing `null` or `""` clears `public_url` unless `PUBLIC_URL` is set in the environment, which returns `400`). Network fields are applied (and active runtime clients reloaded) only when at least one of them is present. An invalid IANA timezone or malformed `public_url` returns `400`. |
| `GET /api/v1/mcp-tokens` | List issued MCP Bearer token metadata (`id`, `name`, `token_prefix`, `scope`, `last_used_at`, `created_at`). |
| `POST /api/v1/mcp-tokens` | Returns **`201 Created`**. Issue a new `readonly` or `admin` MCP Bearer token (`CreateMcpTokenResponse`, returning the one-time `plaintext_token` and `client_config_json`). |
| `DELETE /api/v1/mcp-tokens/{id}` | Revoke an MCP Bearer token immediately. |

---

## 8. Public SMTP Compliance Endpoints (`/u/{token}`, `/v/{token}`)

Mounted directly at root scope (before the SPA fallback, including explicit public `404` handlers for bare `/u`, `/u/`, `/v`, `/v/` so they never fall through to the WebUI SPA), unauthenticated, capped to a **`4 KiB` (`4,096`-byte) request body limit**, rate-limited to **`30` requests / `60s` per IP** (bounded at **`8,192` buckets**, using `ConnectInfo<SocketAddr>` by default when `TRUST_PROXY=false`, or `X-Real-IP` / rightmost `X-Forwarded-For` when `TRUST_PROXY=true`), and served with `X-Robots-Tag: noindex, nofollow`, strict CSP, `Referrer-Policy: no-referrer`, and `Cache-Control: no-store`:

| Method & Path | Behavior |
| :--- | :--- |
| `GET /u/{token}` | **Strictly read-only** localized unsubscribe confirmation page displaying the destination name and a masked email (`a***e@example.com`) with a relative `action=""` `POST` form button (subpath `PUBLIC_URL` safe). Returns `404` on invalid/tampered tokens. |
| `POST /u/{token}` | Idempotently marks the recipient `unsubscribed` (`revoked_method: "one_click"` when body contains `List-Unsubscribe=One-Click`, otherwise `"link"`). If all recipients on the destination are now `unsubscribed` or `bounced`, marks the destination `enabled = false, auto_paused = true`. Supports RFC 8058 one-click POST without authentication. |
| `GET /v/{token}` | **Strictly read-only** localized double-opt-in confirmation page displaying a masked email and a relative `action=""` `POST` confirm button (prevents email link scanners from auto-verifying on prefetch). Returns `404` on tampered tokens and `410 Gone` if expired, locked after 5 wrong code attempts, or superseded by a newer code. |
| `POST /v/{token}` | Marks the recipient `verified` (`consent_method: "link"`), automatically resumes the destination (`enabled = true, auto_paused = false`) if it was `auto_paused`, and immediately wakes any deferred outbox tasks for that destination. Returns `410 Gone` if expired, locked, or superseded. |
