# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/ElleryHe/FeedCourier/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/ElleryHe/FeedCourier/releases/tag/v0.1.0
