# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/ElleryHe/FeedCourier/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/ElleryHe/FeedCourier/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/ElleryHe/FeedCourier/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/ElleryHe/FeedCourier/releases/tag/v0.1.0
