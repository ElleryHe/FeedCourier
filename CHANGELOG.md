# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/ElleryHe/FeedCourier/compare/v0.1.5...HEAD
[0.1.5]: https://github.com/ElleryHe/FeedCourier/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/ElleryHe/FeedCourier/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/ElleryHe/FeedCourier/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/ElleryHe/FeedCourier/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/ElleryHe/FeedCourier/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/ElleryHe/FeedCourier/releases/tag/v0.1.0
