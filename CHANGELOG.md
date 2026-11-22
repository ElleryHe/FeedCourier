# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0] - 2026-11-23

### Added

- **Built-In Plugin Framework (Protocol v1) & Marketplace (`src/plugins/`, `migrations/0003_plugins_and_smtp_compliance.sql`, `ElleryHe/FeedCourier-Plugins`)**:
  - Introduced FeedCourier Plugin Protocol v1 (`api_version: "1"`) with delivery-time `on_render` hooks (and reserved `on_ingest` / `on_delivered` hook contracts for future releases), non-destructive `RenderOverrides` (`content_html`, `primary_link`, `warnings`), per-route plugin configuration stored in `RouteOptions.plugins`, and SQLite tables `plugins`, `plugin_instances` (ChaCha20-Poly1305 encrypted), and `plugin_link_cache` (automatically pruned after 90 days of inactivity).
  - Added `/api/v1/plugins`, `/api/v1/plugins/{id}`, `/api/v1/plugins/{id}/instances*` (with `409 Conflict` route-reference delete protection, live connectivity tests, and per-provider cache clearing), and `/api/v1/plugins/market` (SSRF-guarded remote index loader with 6-hour in-memory cache, `PLUGIN_INDEX_URL` override, and compiled-in `src/plugins/market_snapshot.json` fallback).
  - Scaffolded the independent [`ElleryHe/FeedCourier-Plugins`](https://github.com/ElleryHe/FeedCourier-Plugins) registry repository (`schema/manifest.schema.json`, `registry/link-rewrite.json`, zero-dependency Node.js 24 validator/index builder, and GitHub Actions workflows).
  - Added the WebUI **Plugins** console (Installed + Marketplace tabs, provider CRUD/test/cache controls, schema-driven collapsible route plugin options, route card plugin badges, and amber partial-rewrite warnings in the Outbox view).
- **First-Party Link Rewrite (YOURLS) Plugin (`src/plugins/link_rewrite.rs`)**:
  - Added built-in YOURLS link rewriting supporting multiple encrypted provider instances, `signature` (recommended), `timed_signature` (`md5(timestamp + token)`), and `password` authentication modes, duplicate-URL (`code: "error:url"`) handling, `exclude_domains` (`*.example.com` wildcard matching + automatic provider self-host skip), `pass_title`, `use_proxy`, and `strict_mode` (whole-task retry on failure vs. default per-URL graceful degradation with task warnings).
  - Enforced a 50-URL per-article cap, 4-way concurrency, 5-second per-URL timeout, `64 KiB` response limit, and SQLite short-link caching (`plugin_link_cache`). Strictly rewrites only HTTP/HTTPS `<a href>` and primary links—preserving all media `src` URLs, Telegram `link_preview_options.url`, SMTP compliance links, and Notion's original `Link` column.
- **SMTP Per-Recipient Compliance, Double Opt-In & RFC 8058 One-Click Unsubscribe (`src/channels/smtp.rs`, `src/api/public.rs`, `src/crypto.rs`)**:
  - Switched outbox SMTP delivery to individual per-recipient envelopes (single `To:` address, deterministic per-recipient `Message-ID`, 1-second inter-recipient delay with lease renewal, and `sent_recipients` checkpoint resume; quota ledger continues counting 1 per article task).
  - Added `smtp_recipients` (keyed by HKDF-derived `address_hmac` with zero plaintext emails stored in table) and `/api/v1/destinations/{id}/recipients*` endpoints for manual-trigger double opt-in verification: 6-digit SHA-256 hashed codes (30-minute TTL, max 5 attempts, 60-second resend cooldown, 5/hour cap), optional `/v/{token}` confirmation links when `PUBLIC_URL` is configured, immediate wakeup of deferred tasks upon verification, administrator unsubscribe/reactivate controls, synchronous 5xx hard-bounce marking (`bounced`), and automatic destination pause when all recipients are unsubscribed or bounced.
  - Added `PUBLIC_URL` (environment variable or WebUI **Settings**), optional CAN-SPAM `sender_identity` on SMTP accounts, RFC 8058 headers (`List-Unsubscribe`, `List-Unsubscribe-Post: List-Unsubscribe=One-Click`, `List-Id: <dest-{id}.{from_domain}>`, `Auto-Submitted: auto-generated`), and unauthenticated `/u/{token}` & `/v/{token}` endpoints registered before SPA fallback.
  - Added automatic pre-v0.3.0 online `VACUUM INTO` snapshot backup (`<DATA_DIR>/pre-v0.3.0-backup.db`) before migration `0003` and idempotent startup backfill (`backfill_legacy_smtp_recipients`, `status = 'verified'`, `consent_method = 'legacy'`, marked by `system_settings.smtp_recipient_model_version = 1`).
- **MCP `list_plugins` & Route Plugin Configuration (`src/mcp/tools.rs`)**:
  - Expanded `/mcp` to **10 tools** by adding `readonly` secret-free `list_plugins` (exposing only plugin/provider IDs, names, hooks, and enabled status), and added `plugins` support to `configure_route` and `list_feeds_status`.
- **Bilingual Plugins & SMTP Compliance Documentation (`docs/`)**:
  - Added the **Plugins / 插件** documentation section with a build-time VitePress marketplace loader (`docs/.vitepress/plugins.data.ts`, remote fetch with snapshot fallback), YOURLS setup & `SSRF_ALLOWLIST` guide, Protocol v1 development reference, and Nginx/Caddy reverse-proxy examples exposing only `/u/` and `/v/`.

### Changed

- **Destination Delete Reference Guard (`DELETE /api/v1/destinations/{id}`)**: Deleting a destination still referenced by one or more routes now returns **`409 Conflict`** listing `referencing_routes`, requiring explicit `?cascade=true` to cascade-delete the destination together with its routes.
- **Notion 8-Column Standard Schema (`Short URL`)**: `POST /api/v1/channels/notion/init-db` now provisions an optional `Short URL` (`url`) property alongside the 7 existing columns so Notion destinations preserve the original article permalink in `Link` while recording shortened URLs from route plugins in `Short URL` when mapped (`inspect-schema` detects and optionally patches it).

### Security

- **Zero-PII HKDF/HMAC Public Compliance Tokens & Read-Only GET Invariant (`src/crypto.rs`, `src/api/public.rs`)**: Derived domain-separated HKDF-SHA256 subkeys (`fc-smtp-addr-v1`, `fc-unsub-v1`, `fc-verify-v1`) with constant-time validation, kept `GET /u/{token}` and `GET /v/{token}` strictly read-only against mail-scanner prefetch, and enforced per-IP sliding-window rate limiting (`30 req / 60s`), `X-Robots-Tag: noindex, nofollow`, strict CSP, `Referrer-Policy: no-referrer`, and `Cache-Control: no-store`.
- **Plugin SSRF & Secret Isolation (`src/plugins/`)**: Routed all plugin API calls (YOURLS) and remote marketplace index fetches (`PLUGIN_INDEX_URL`) through `ProtectedHttpClient` with connect-time `SsrfDnsResolver` and `SSRF_ALLOWLIST` enforcement, encrypted provider configs at rest with ChaCha20-Poly1305, masked secrets in `/api/v1/plugins*`, and omitted all credentials/URLs from MCP `list_plugins`.

### Fixed

- **Pre-DDL Backup Sidecar Hygiene (`src/db/mod.rs`)**: Verified both target `.db` and companion `-wal`/`-shm` sidecar non-existence before `VACUUM INTO` (appending collision suffixes when needed) and cleaned up residual sidecars after backup completion.
- **Notion `database_id` → `data_source_id` Auto-Writeback (`src/channels/notion.rs`)**: Persisted resolved `data_source_id` values back to re-encrypted `channel_destinations` rows after `404 object_not_found` fallback resolution so subsequent deliveries avoid extra 404 round-trips.
- **Full HTML5 Entity Decoding & Fractional Percentage Image Widths (`src/ingest/html.rs`, `src/channels/smtp.rs`)**: Upgraded HTML attribute decoding to full HTML5 named entities via `html-escape` (accurate `<a href>` rewriting and email attribute escaping) and preserved fractional percentage widths (such as `width="33.3%"`) in SMTP email images.
- **Bilingual Release Pipeline Documentation Parity (`docs/zh/reference/releasing.md`)**: Aligned the Chinese Mermaid release workflow diagram with the English reference.

## [0.2.0] - 2026-11-22

### Breaking Changes

- **Two-Layer Delivery Channel Model (`Channel Account` + Multiple `Destinations`)**:
  - Split delivery channels into a **Channel Account** (`channels` — storing encrypted account credentials, outbound proxy toggle, shared pacing/jitter, rolling hourly/daily quotas, and shared cooldowns) and one or more child **Destinations** (`channel_destinations` — storing independently encrypted Telegram `chat_id`/`message_thread_id`, SMTP `to_addresses`/`reply_to`, or Notion `data_source_id`/`column_mapping`, each with its own enable toggle and connectivity test).
  - **Automatic Atomic SQLite Migration & Pure Pre-DDL Backup**: On first startup against an existing `v0.1.x` database, `Database::connect` creates a consistent online `VACUUM INTO` snapshot at `<DATA_DIR>/pre-v0.2.0-backup.db` **before** `migrations/0002_channel_destinations.sql` runs—producing a pure `v0.1.x` SQLite snapshot compatible with rolling back directly to `v0.1.6` (if a backup already exists, a UTC-timestamped `pre-v0.2.0-backup-<timestamp>.db` file is written without overwriting it). `Database::migrate_channels_to_v2` then splits all pre-v0.2.0 channels inside a single SQLite transaction: legacy SMTP rows merge **only** when `host`, `port`, `username`, `password`, `security`, and `from_address` all match, SMTP `credential_group_key` includes the password SHA-256 digest (keeping distinct API keys under a shared username such as SendGrid's `apikey` isolated), merged quotas/pacing/cooldown/proxy are aggregated from **enabled** legacy channels (falling back to all rows only when every merged channel is disabled), trailing 24-hour deliveries are backfilled into `delivery_quota_ledger`, all `routes` and `delivery_tasks` (including `delivered` and `dead_letter` history) are repointed with a post-migration `destination_id IS NOT NULL` integrity check, and `system_settings.channel_model_version = 2` is recorded for idempotency.
  - **Telegram Bot API SSRF Enforcement (Upgrade Note for Local Bot API Servers)**: All Telegram Bot API requests—including official `https://api.telegram.org` and custom `api_base_url` / self-hosted Local Bot API servers—now enforce connect-time `SsrfPolicy` DNS validation (sharing the hot-reloadable `SSRF_ALLOWLIST`) with automatic redirects disabled, and `api_base_url` only allows `http`/`https` URLs without userinfo, query, or fragment (SSRF denials return `403 ssrf_blocked`). Existing internal Local Bot API endpoints are **not** auto-allowlisted: operators running a Local Bot API server on a private/loopback/Docker address must explicitly add its hostname or IP/CIDR to `SSRF_ALLOWLIST`.
  - **Shared Account-Level Rate Limiting & Persistent Quota Ledger (`delivery_quota_ledger`)**: Minimum interval, random jitter, rolling hourly/daily quotas, and upstream `429` cooldowns are enforced across all destinations belonging to the same Channel Account and any sibling accounts sharing `credential_group_key` (applying the strictest effective quota across the group). Rolling 1h/24h quota counts and `last_completed_at` pacing timestamps are stored in the new `delivery_quota_ledger` table (without business foreign keys, pruned after 24 hours), so deleting feeds, routes, or destinations—or clearing feed cache—never resets quota usage or pacing timestamps.
  - **REST API & MCP Destination Targeting**: Route creation/updates (`POST`/`PATCH /api/v1/routes`, MCP `configure_route`) and debug pushes (`POST /api/v1/feeds/preview/push`, MCP `test_push_feed_entry`) now take `destination_id` as the primary target. Legacy `channel_id` payloads remain accepted when the account owns exactly one destination and return a validation error when multiple destinations exist. Added `/api/v1/channels/{id}/destinations`, `/api/v1/destinations/{id}`, and `/api/v1/destinations/{id}/test`; `POST /api/v1/channels/{id}/test` now verifies account credentials when `destination_id` is omitted.

### Added

- **Auto-Fetched Account & Destination Default Names**: Leaving the display name blank when creating a Channel Account or Destination automatically derives it from Telegram `getMe` (`@username`) / `getChat` (group or channel title), SMTP `from_address` / first recipient address, or Notion `/v1/users/me` integration name / database title.
- **Per-Route Timezone Override (`RouteOptions.timezone`)**: Routes now accept an optional IANA timezone (e.g., `Asia/Shanghai`, `America/New_York`) that overrides the system timezone for that route's delivery time windows/weekdays, SMTP email article timestamps, and Telegram rich-message `<footer>` timestamps—formatted with explicit UTC offsets such as `2026-11-08 11:00 (UTC+08:00)`. Configurable in the WebUI route editor, REST API, debug push, and MCP (`configure_route`, `test_push_feed_entry`).
- **Feeds List Tag Filter, Persistent Sort & Pagination**: Narrowed the Feeds search box to feed title and URL; added a single-select **Tag** filter dropdown (with per-tag counts and an **Untagged** option), a persistent **Sort** control (added time or `Intl.Collator` CJK/Latin-aware title, ascending/descending), and **15 / 30 / 50** per-page pagination persisted in `localStorage`.
- **Channels & Routes Page Filters**: Added a **Channel Kind** filter (`All` / `Telegram` / `SMTP` / `Notion`) to the **Channels** view and cascading **Channel Kind → Account** filters to the **Routes** view.
- **Semantic Outbox Queue Status Colors**: Standardized Outbox badges, HTTP/SMTP status codes, attempt-count escalation (`1` neutral, `≥ 2` amber, maxed red), and status-matched diagnostic log text so successful delivery logs no longer render in red.

### Changed

- **Telegram Rich Message Typography (`telegram-rich 0.0.2`)**: Upgraded to `telegram-rich 0.0.2` and refined `rich_message` layout—demoting article titles from `<h1>` to `<h2>`, moving `Source · Author · Localized Publish Time` into a native Telegram `<footer>` below the title, inserting an `<hr>` divider before the body, enabling `heading_offset = 2` (`h1→h3`, `h2→h4`, …, up to `h6`), and enabling `group_consecutive_media = true` to merge 2+ consecutive images into `<tg-collage>` (or `<tg-slideshow>` when exceeding collage limits).
- **Redacted MCP Destinations Discovery (`list_feeds_status`)**: Exposed secret-free `destinations` summaries (`id`, `channel_id`, `name`, `kind`, `enabled`) both nested under each channel and as a top-level list without ever leaking credentials, email addresses, `chat_id`, or `data_source_id`; `inspect_failed_deliveries` now includes `destination_id` and `destination_name`.
- **Pacing & Jitter Documentation**: Clarified that random jitter (`0..=jitter_max_secs`) is sampled independently after every delivered task (never shared across a batch) and held in memory only (after a restart, the first delivery enforces `min_interval_secs` from SQLite `delivery_quota_ledger` `last_completed_at`).
- **In-Flight Task Synchronization on Route Updates (`PATCH /api/v1/routes/{id}`, MCP `configure_route`)**: Changing a route's target destination atomically repoints pending `queued`, `deferred`, and `retrying` tasks to the new `destination_id` (clearing `checkpoint_json` and resetting `attempt_count = 0`, while leaving active `delivering` tasks untouched). Changing `active_windows`, `active_weekdays`, or `timezone` immediately resets `next_attempt_at = now` on `queued` and `deferred` tasks and wakes the outbox dispatcher.
- **Explicit SMTP Password Clearing (`PATCH /api/v1/channels/{id}`)**: Omitting `password` continues to preserve the existing encrypted SMTP password, while explicitly passing `"password": null` clears an optional SMTP password.

### Fixed

- **DST-Safe Delivery Windows & Strict Equal-Boundary Validation (`src/schedule.rs`)**: Handled Daylight Saving Time transitions cleanly—spring-forward gaps clamp window boundaries forward to the first valid local instant, and fall-back ambiguous intervals use the earliest occurrence for window starts and the latest occurrence for window ends. Restricted all-day equal-boundary windows strictly to `00:00`–`00:00` (or an empty `active_windows` list), rejecting any other `start == end` window with `400 Bad Request`.
- **SMTP Responsive Images, Figures, Tables & Code Blocks (`src/channels/smtp.rs`)**: Rewrote `<img>` tags in email bodies with Outlook-compatible `width="592"` (preserving explicitly narrower widths and relative percentage widths), stripped fixed `height` attributes, and added responsive inline CSS (`max-width:100%;height:auto`) alongside `<figure>`/`<figcaption>`, `<table>`, and `<pre>` width/overflow rules and `<head>` CSS fallbacks so wide feed images and tables no longer overflow the `640px` email card.

## [0.1.6] - 2026-11-18

### Added

- **Feed Debug Push (`POST /api/v1/feeds/preview/push`, WebUI preview modal, MCP `test_push_feed_entry`)**: send the latest (or any previewed) live item of a subscribed or ad-hoc feed straight to a chosen channel with a chosen Telegram mode and delivery language. Stateless — no fingerprints, queue tasks, or quota usage; Notion pages use a `test-` prefixed `FC-ID`, so the same item can be re-pushed while tuning.
- **Per-Feed Cache Clearing (`POST /api/v1/feeds/{id}/clear-cache`, MCP `manage_feed` `clear_cache`)**: wipes a feed's entries/fingerprints/history tasks and HTTP validators, and optionally re-polls immediately to queue the full backlog again.
- **Global Queue Pause & Scheduled Resume (`GET`/`PUT /api/v1/tasks/queue`, MCP `manage_outbox_queue`)**: pause delivery now or until a time/duration while feeds keep polling and tasks accumulate; on resume everything is delivered as one merged chronological stream.
- **Route Delivery Windows & System Timezone**: routes accept daily `active_windows` (`HH:MM`–`HH:MM`, midnight-wrapping) and `active_weekdays`; outside a window tasks stay queued with `next_attempt_at` set to the next window start. The IANA system timezone is configured via `TIMEZONE`/`TZ` or *Settings* (default `America/Los_Angeles`; the English quick start sets `TZ=America/Los_Angeles`, the Chinese one `TZ=Asia/Shanghai`).
- **Administrator Username & TOTP Two-Factor Authentication**: the WebUI login now uses a username (default `Courier`, override with `ADMIN_USERNAME`) so password managers autofill correctly. *Settings → Account & Security* adds RFC 6238 TOTP (QR + manual secret, encrypted at rest, replay protection), 10 single-use recovery codes, password/username changes (other sessions are revoked on password change), a two-step login challenge with failure throttling, and `RESET_2FA=true` for emergency recovery.
- **MCP**: `configure_route` gains `locale`, `active_windows`, `active_weekdays`; `list_feeds_status` reports `timezone` and queue state; two new admin tools `manage_outbox_queue` and `test_push_feed_entry` (9 tools total).

### Changed

- **Chronological Backlog Delivery**: new/backlog items are now stored and enqueued oldest → newest (reverse feed order or explicit `published_at`), and the outbox claims tasks by `COALESCE(published_at, discovered_at)`, so a non-silent new subscription no longer pushes the newest article first and several feeds merge into one ordered stream.
- **Delivery Language Is Per Route**: `RouteOptions.locale` (default = the WebUI language when the route is created) controls SMTP/Telegram template language; routes created before this release inherit the previous system language. The WebUI language switcher is a browser-local preference again (sidebar bottom-left / mobile top bar) and no longer changes delivery.
- Channel connectivity tests use the current WebUI language.

## [0.1.5] - 2026-11-18

### Added

- **Release Changelog RSS 2.0 & Atom 1.0 Feeds (`docs/.vitepress/config.mts`, `CHANGELOG.md`)**:
  - Added static build-time generation of `/feed.xml` (RSS 2.0) and `/atom.xml` (Atom 1.0) directly from `CHANGELOG.md`, complete with `<link rel="alternate" type="application/rss+xml">` and `application/atom+xml` auto-discovery headers, top-bar RSS social link, and README links so users can subscribe to FeedCourier's own release notes.
- **Documentation Site SEO Hardening, IndexNow & `llms.txt` (`docs/`, `.github/workflows/indexnow.yml`)**:
  - Added build-time generation of `/llms.txt` (structured English documentation index and page summaries) and `/llms-full.txt` (complete single-file plain-text Markdown documentation) with `<link rel="alternate" type="text/plain" href="https://feedcourier.ellery.me/llms.txt" />` discovery.
  - Added Cloudflare Workers Static Assets `docs/public/_headers` (`X-Robots-Tag: noindex, nofollow` on `*.workers.dev/*`) and set `"workers_dev": false` in `docs/wrangler.jsonc` so preview/worker subdomains cannot compete with `https://feedcourier.ellery.me` in search rankings.
  - Added legacy version route isolation (`noindex, follow`, canonical rewrite to the latest version path, and `sitemap.xml` exclusion for `/v<N>/*` or `version: legacy` frontmatter).
  - Added optional IndexNow key-file emission (`/<INDEXNOW_KEY>.txt`) at build time and `.github/workflows/indexnow.yml` to automatically push modified documentation and feed URLs to `https://api.indexnow.org/indexnow` after merges to `main`.
- **Automated Release Tagging on `main` (`.github/workflows/release.yml`)**:
  - Updated the release workflow to trigger on `push` to `main` (alongside manual `v*` tag pushes): when `[package].version` in `Cargo.toml` does not yet have a corresponding `v<version>` Git tag, the workflow automatically creates and pushes the tag before building multi-platform binaries, multi-arch Docker images, and the GitHub Release.

### Changed

- **Unified Console & Delivery Language Setting in System Settings (`web/src/views/SettingsView.tsx`, `web/src/components/AppShell.tsx`, `web/src/App.tsx`)**:
  - Relocated the console language selector (`English` / `简体中文`) from the sidebar footer and mobile top bar into **System Settings**, accompanied by an explicit helper notice explaining that switching the console language also updates the delivery template language for newly dispatched SMTP, Telegram, and Notion notifications (`AuthGateView` retains a login-screen language toggle prior to authentication).
  - Hydrated the active WebUI locale from `/api/v1/settings` (`system_settings.locale`) upon authentication so new browser sessions reflect the server's configured language.
- **GitHub Actions Runner Upgrades (`.github/workflows/release.yml`, `.github/workflows/ci.yml`)**:
  - Upgraded the macOS release binary runner from `macos-latest` to `macos-26` (`aarch64-apple-darwin` + `x86_64-apple-darwin` cross-compilation), upgraded `actions/upload-artifact` to `@v7`, and pinned `ubuntu-24.04` across all Linux CI and release jobs.

### Fixed

- **Scheduled Feed Polling Stall & UTF-8 HTML Entity Slice Panic (`src/ingest/html.rs`, `src/scheduler/mod.rs`, `src/db/repo.rs`, `web/src/views/FeedsView.tsx`)**:
  - Fixed a byte-index slice panic in `decode_basic_html_entities` (`&s[i..limit]` when `i + 12` landed inside a multi-byte UTF-8 character after an unescaped `&` in feed titles or author names) that terminated the background scheduler loop and left `next_poll_at` timestamps frozen in the past.
  - Isolated each scheduled feed poll inside its own `tokio::spawn` task with a hard `90s` timeout and outer `catch_unwind` recovery so a slow or panicking feed can never stall the scheduler loop or block sibling feeds in the same batch.
  - Clamped overdue `next_poll_at` timestamps to `now` when updating a feed's `poll_interval_minutes`, and added a `30s` `refetchInterval` on the WebUI **Feeds** view so background scheduler polls update relative timestamps live.
- **WebUI Theme Mode Reactivity & Dark-Mode Text Selection Contrast (`web/src/components/AppShell.tsx`, `web/index.html`)**:
  - Replaced per-component `useThemeMode` state with a shared `useSyncExternalStore` theme store and live `matchMedia('(prefers-color-scheme: dark)')` listener so desktop and mobile theme controls stay synchronized and `system` mode reacts immediately to OS appearance changes.
  - Added explicit dark-mode text selection styling (`dark:selection:bg-emerald-500/30 dark:selection:text-emerald-50`) on `<body>` in `web/index.html` so highlighted text remains clearly legible on dark backgrounds.

## [0.1.4] - 2026-11-18

### Added

- **System Locale Persistence & Localized Push Templates (`src/channels/`, `src/api/`, `web/`)**:
  - Added `AppLocale` (`en` / `zh-CN`) backed by SQLite `system_settings.locale` (with optional `LOCALE` environment variable preset and partial `{ "locale": ... }` hot-updates on `GET`/`PUT /api/v1/settings`), automatically synchronized whenever the administrator signs into the WebUI or switches UI languages.
  - Localized fixed notification strings across Telegram (`Link` / `链接`, `Media N` / `媒体 N`, `Read original` / `阅读原文`, truncation notice, and channel test messages) and SMTP email templates (`Read original →` / `阅读原文 →`, truncation notice, and footer).

### Changed

- **Native Multi-Runner Container Builds & Cargo Dependency Layer Caching (`.github/workflows/release.yml`, `Dockerfile`)**:
  - Replaced QEMU user-mode `linux/arm64` emulation in the release workflow with parallel native builds on `ubuntu-24.04` (`linux/amd64`) and `ubuntu-24.04-arm` (`linux/arm64`) using `push-by-digest`, followed by `docker buildx imagetools create` multi-arch manifest merging.
  - Added a version-neutral `cargo-recipe` manifest sanitization stage in `Dockerfile` so Cargo dependency compilation layers remain cached across semantic version bumps.
- **Telegram `media_mode` Hyperlink Captions (`src/channels/telegram.rs`)**:
  - Switched `MediaMode` captions and zero-media fallback messages to `parse_mode: "HTML"`, rendering the source permalink as a clean `<a href="...">Link</a>` (`链接` in `zh-CN`) hyperlink and compactly appending oversized media fallbacks on the same line (` · Media N` / ` · 媒体 N`).
- **Minimalist OpenGraph & Social Preview Banner (`docs/`)**:
  - Replaced `docs/public/og-image.png` with a streamlined `1280×640` (`2:1`) dark topographic signal-wave brand banner (`og:image:width = 1280`, `og:image:height = 640`).

### Fixed

- **Telegram `media_mode` Inline Video Playback & Pure-Rust MP4 Metadata Extraction (`src/channels/telegram.rs`)**:
  - Bypassed Telegram's direct URL pass-through for `Video` attachments and ambiguous/extensionless `<enclosure>` items so origin CDNs omitting `.mp4` extensions or serving `application/octet-stream` never cause Telegram to silently downgrade videos into unpreviewable `Document` files.
  - Added magic-byte container sniffing (`sniff_media_bytes`) to promote misclassified attachments and normalize video multipart uploads to `video/mp4` + `.mp4`, paired with a zero-dependency ISO-BMFF (`MP4`/`MOV`/`M4V`) `moov -> mvhd / tkhd / stsd` parser (`parse_mp4_metadata`, including 90°/270° portrait rotation matrix handling) so `sendVideo` and `sendMediaGroup` (`InputMediaVideo`) always carry `supports_streaming: true`, `width`, `height`, and `duration`.

## [0.1.3] - 2026-11-16

### Added

- **WebUI Bilingual (`English` / `简体中文`) Internationalization (`web/`)**:
  - Added zero-dependency, strongly typed English and Simplified Chinese dictionaries (`web/src/i18n/en.ts`, `web/src/i18n/zh.ts`, `web/src/i18n/index.tsx`) with automatic browser language detection (`zh*` → `zh-CN`, fallback `en`), `localStorage` (`fc-locale`) persistence, and live `<html lang>`, page title, and relative-time formatting synchronization.
  - Added one-click language switching and full bilingual UI coverage across `AuthGateView`, `AppShell` (desktop sidebar and mobile top bar), `OverviewView`, `FeedsView`, `ChannelsView`, `RoutesView`, `TasksView`, `SettingsView`, and shared UI primitives while preserving raw backend diagnostic logs intact.
- **Documentation Site `robots.txt` & Static Build-Time SEO (`docs/`)**:
  - Added `docs/public/robots.txt` (`Allow: /`, `Sitemap: https://feedcourier.ellery.me/sitemap.xml`) and a `1200×630` static social preview card (`docs/public/og-image.png`).
  - Added unique `description` frontmatter across all 34 English and Simplified Chinese documentation pages, and configured VitePress `transformHead` to inject `<link rel="canonical">`, bidirectional `hreflang` (`en-US`, `zh-CN`, `x-default`), Open Graph / Twitter Card metadata, `404` `noindex`, and home-page `Schema.org` `SoftwareApplication` JSON-LD at static build time.

### Changed

- **Container Entrypoint Automatic Bind-Mount Ownership & Privilege Drop (`Dockerfile`, `docker-entrypoint.sh`)**:
  - Added `docker-entrypoint.sh` using pre-installed `setpriv` (`util-linux` in `debian:bookworm-slim`) so containers started as `root` automatically create/fix ownership on `$DATA_DIR` (`/data`) and `exec` as non-root `10001:10001` (PID 1) with `--inh-caps=-all`, while passing through directly when started with `--user`.
  - Added `shellcheck` linting and a root-owned `0755` bind-mount privilege-drop verification step to `.github/workflows/ci.yml`.

## [0.1.2] - 2026-11-15

### Added

- **Dedicated Architecture & Development Manual Topics (`docs/`)**:
  - Split the VitePress documentation site into two independent sections with dedicated sidebars—**User Guide** and **Architecture Manual**—and added quick top-bar navigation links for **Overview** and **Quick Start**.
  - Added dedicated bilingual technical reference pages for **[Channel Protocols & Delivery](https://feedcourier.ellery.me/reference/channel-protocols)** (SMTP `minijinja` rendering, Telegram Bot API 10.4 `sendRichMessage` / media batching, and Notion `2026-03-11` Markdown v2 / async tasks) and **[Security & Isolation](https://feedcourier.ellery.me/reference/security)** (`ChaCha20-Poly1305` credential storage, WebUI destination lock, `<untrusted_feed_content>` sanitization, and connect-time DNS SSRF guards).
- **CI Dockerfile Build Verification**:
  - Added `docker-web-stage` verification in `.github/workflows/ci.yml` to validate the `Dockerfile` frontend build stage on every push and pull request.

### Changed

- **Documentation & README Tone Refinement**:
  - Rewrote `README.md`, `README.zh-CN.md`, and all bilingual User Guide pages in a clear, factual, user-oriented style, keeping channel setup and MCP guides focused on practical credentials and configuration while relocating low-level API version headers and internal implementation details to the Architecture Manual.
  - Streamlined README header badges (`CI`, `Release` version shield, `Docker Hub`, and `License: AGPL-3.0`), unified human-facing license labels to `AGPL-3.0` across WebUI, REST API, MCP metadata, and documentation, and updated the documentation footer to `Copyright © <currentYear> FeedCourier` with automatic build-time and client-side year synchronization.
- **Container Build & CI Runtime Upgrades**:
  - Upgraded `Dockerfile` Stage 1 builder image from `node:24-alpine` to `node:26-alpine` (Node.js 26 LTS) using `npm install -g pnpm@10` in place of the removed bundled `corepack`.
  - Upgraded GitHub Actions workflows to `actions/checkout@v7`, `pnpm/action-setup@v6`, and `docker/login-action@v4`, and removed the redundant root `wrangler.jsonc` and unused `dockerhub-description` release step.

## [0.1.1] - 2026-11-15

### Added

- **Bilingual VitePress Static Documentation Site (`docs/`)**:
  - 32 bilingual pages (`/` English and `/zh/` Simplified Chinese) covering Quick Start, Configuration & Proxy/SSRF, Operations & Hot Backup, Pacing & Quota Guardrails, SMTP Newsletters, Telegram Bot API 10.4, Notion `2026-03-11`, MCP Client Setup, 7 MCP Tools Reference, MCP Security & Trust Boundaries, System Architecture, SQLite Database Schema, `/api/v1/*` REST API, and Releasing & CI/CD.
  - Cloudflare Workers Static Assets deployment configuration (`docs/wrangler.jsonc`) serving **[feedcourier.ellery.me](https://feedcourier.ellery.me)** with native local search and client-only Mermaid diagram rendering.
- **Unified Brand Logo Suite**:
  - Transparent geometric vector brand assets (`logo-light.svg`, `logo-dark.svg`, `favicon.svg`, and `logo.png`) integrated across GitHub `<picture>` dark/light README headers, the VitePress documentation site, and the embedded React 19 WebUI (`AppShell` sidebar header, `AuthGateView` login/bootstrap card, and browser favicon).
- **CI Documentation Verification & Dependabot Coverage**:
  - Added `Docs Site Build & Link Check` CI job in `.github/workflows/ci.yml` to verify VitePress static builds and fail on dead internal links.
  - Added `/docs` npm dependency update tracking and non-LTS Node.js Docker major ignores in `.github/dependabot.yml`.

### Changed

- **Streamlined Root README Entrypoints**:
  - Rewrote `README.md` and `README.zh-CN.md` into concise bilingual entrypoints (<100 lines) with architecture overview, Docker Quick Start, and structured deep links to `feedcourier.ellery.me`, while removing the legacy root `feedcourier.md` design draft.
- **GitHub Actions Native `node24` Runtime Upgrades**:
  - Upgraded CI and Release workflow actions to native `node24` major releases (`actions/setup-node@v7`, `actions/download-artifact@v8`, `docker/setup-qemu-action@v4`, `docker/setup-buildx-action@v4`, `docker/metadata-action@v6`, `docker/build-push-action@v7`, and `softprops/action-gh-release@v3`).

## [0.1.0] - 2026-11-14

### Added

- **Guarded Feed Ingest & Ephemeral Storage Pipeline**:
  - RSS, Atom, and JSON Feed polling (`feed-rs`) with conditional HTTP caching (`ETag` and `Last-Modified`), keyword/regex route filters, and OPML 2.0 import/export.
  - Optional per-feed Readability full-text extraction (`dom_smoothie`) behind bounded concurrency semaphores with graceful fallback to feed summaries.
  - Permanent 4-tier anti-drift SHA-256 deduplication (native entry ID, canonicalized permalink stripped of tracking parameters, explicit published timestamp + title, and sanitized plain-text prefix).
  - Silent initialization (`silent_init`) for newly added and OPML-imported feeds, zero-refcount `NOT EXISTS` payload garbage collection (`RETENTION_HOURS`), and online `VACUUM INTO` SQLite backups.
- **Lease-Based SQLite WAL Outbox & Scheduler**:
  - Split SQLite WAL connection architecture pairing a serialized single-writer pool (`max_connections = 1`) with a concurrent `query_only = ON` read pool.
  - Atomic `lease_until` task claiming and crash-safe `checkpoint_json` state reconciliation without startup scan races (`queued`, `delivering`, `retrying`, `deferred`, `delivered`, `dead_letter`).
  - Shared-credential anti-spam pacing with configurable minimum interval, random jitter, `429 Retry-After` cooldowns, and rolling hourly/daily quota guardrails.
- **SMTP Newsletter Delivery Channel**:
  - Responsive inline-CSS single-column HTML emails (`minijinja`) with `multipart/alternative` plain-text fallback (`html2text`) sized below Gmail's `102 KB` clipping threshold.
  - SPF/DKIM/DMARC-safe `Force-From` sender alignment, `Auto-Submitted: auto-generated` and per-feed `List-Id` headers, and deterministic fingerprint-derived `Message-ID` headers for client-side deduplication.
- **Telegram Bot API 10.4 Delivery Channel**:
  - `rich_message` mode powered by [`telegram-rich`](https://crates.io/crates/telegram-rich) (`sendRichMessage` Rich HTML normalization, block-boundary truncation within Bot API 10.4 limits, runtime capability probing, and automatic fallback to Compact Card on older Local Bot API Servers).
  - `compact_card` mode (`sendMessage` digest within `4,096` chars with route-level `link_preview` control).
  - `media_mode` with `<enclosure>`, `media:content`, and inline media extraction, Unicode-segmented 3-line captions, zero-disk anti-hotlink multipart streaming fallback, and checkpointed 10-item `sendMediaGroup` batch recovery.
- **Notion `2026-03-11` Delivery Channel**:
  - Zero-friction 7-column database auto-provisioning via `initial_data_source.properties` (`Title`, `Link`, `Source`, `Published`, `State`, `Tag`, `FC-ID`) or binding/patching of an existing `data_source_id`.
  - HTML-to-Markdown v2 normalizer (`htmd`) with dynamic outer code-fence expansion, `5,000`-block boundary truncation, `allow_async: true` background task polling (`/v1/async_tasks/{id}`), and `FC-ID` query reconciliation for effective-once crash recovery.
- **Embedded React 19 WebUI & REST API**:
  - Single-binary embedded React 19 + TypeScript + Vite management console (`rust-embed`) and `/api/v1/*` REST API for feeds, encrypted channels, routes, outbox diagnostics, dead-letter retries, selective HTTP/SOCKS5 proxy routing (`OUTBOUND_PROXY`), and database backups.
- **Native Streamable HTTP MCP Server (`/mcp`)**:
  - Built-in `rmcp` Streamable HTTP endpoint with `readonly` (`list_feeds_status`, `inspect_failed_deliveries`, `preview_feed_latest`) and `admin` (`manage_feed`, `configure_route`, `reset_and_poll_feed`, `retry_dead_tasks`) permission scopes.
- **Multi-Arch Packaging**:
  - Non-root Debian Bookworm runtime container image (`UID/GID 10001`, `/data` volume) and prebuilt release binaries for Linux and macOS (`x86_64` and `aarch64`).

### Security

- **Connect-Time DNS SSRF & Redirect Guard (`src/net/ssrf.rs`)**: All outbound requests to user-supplied or feed-derived URLs validate resolved IPv4/IPv6 addresses prior to TCP connect (blocking loopback, link-local, cloud metadata `169.254.169.254`, and private RFC1918/ULA ranges against DNS rebinding), re-validate every `30x` redirect hop, enforce strict response stream caps, and support explicit `SSRF_ALLOWLIST` CIDR/hostname exceptions.
- **Encrypted Credential Vault & Scoped Auth**: Channel credentials are encrypted at rest with `ChaCha20-Poly1305` AEAD (`master.key`, `0600`); WebUI administrator passwords use `Argon2id` PHC hashing with `HttpOnly` session cookies (`fc_session`); and `/mcp` Bearer tokens are stored exclusively as `SHA-256` hashes.
- **MCP Trust Boundaries & Untrusted Content Framing**: Channel secrets and recipient targets (email addresses, `chat_id`, `data_source_id`) are strictly locked to the WebUI and opaque to `/mcp` tools, while untrusted external feed text surfaced to AI agents is sanitized, truncated, and isolated inside `<untrusted_feed_content>` XML boundary tags (`src/mcp/sanitize.rs`), complemented by `ammonia` HTML sanitization across ingest and delivery pipelines.

[Unreleased]: https://github.com/ElleryHe/FeedCourier/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/ElleryHe/FeedCourier/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/ElleryHe/FeedCourier/compare/v0.1.6...v0.2.0
[0.1.6]: https://github.com/ElleryHe/FeedCourier/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/ElleryHe/FeedCourier/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/ElleryHe/FeedCourier/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/ElleryHe/FeedCourier/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/ElleryHe/FeedCourier/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/ElleryHe/FeedCourier/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/ElleryHe/FeedCourier/releases/tag/v0.1.0
