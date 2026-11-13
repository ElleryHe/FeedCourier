# FeedCourier

**English** | [简体中文](./README.zh-CN.md)

[![CI](https://github.com/ElleryHe/FeedCourier/actions/workflows/ci.yml/badge.svg)](https://github.com/ElleryHe/FeedCourier/actions/workflows/ci.yml)
[![License: AGPL-3.0-only](https://img.shields.io/badge/License-AGPL--3.0--only-blue.svg)](./LICENSE)
[![Rust 2024](https://img.shields.io/badge/Rust-2024_Edition-orange.svg)](https://www.rust-lang.org/)

**FeedCourier** is a lightweight, single-binary pure-Rust **RSS / Atom / JSON Feed streaming delivery gateway** (`<20 MB` idle RSS, single SQLite WAL volume at `/data`) designed for individuals and small engineering teams.

Inspired by `rss2email`'s single-entry reliability—without multi-tenant bloat or local content hoarding—FeedCourier routes feed entries in real time to **SMTP Newsletters**, **Telegram Bot API 10.4** (`sendRichMessage` via [`telegram-rich`](https://github.com/ElleryHe/telegram-rich), Compact Card, and Media Mode), and **Notion (`2026-03-11` Data Sources & Markdown v2)**, managed identically through an **embedded React 19 WebUI** and a **native Streamable HTTP MCP server (`/mcp`)**.

---

## Core Design Principles

1. **Delivery Gateway, Not an Online Reader** — FeedCourier never hoards full-article archives or media caches on disk. Deep reading and knowledge archiving belong in your email client, Telegram stream, or Notion database.
2. **Permanent Fingerprints, Ephemeral Payloads** — Compact 64-char SHA-256 deduplication fingerprints persist permanently in SQLite, while sanitized HTML bodies and media metadata exist only during active queueing/retry windows (`RETENTION_HOURS=24`) and are reclaimed automatically. Media relays stream in-memory without touching disk.
3. **Single-Entry Streaming with Anti-Spam Pacing** — No rigid daily cron digests. Every matched article enqueues an isolated transactional task (`1 Task = 1 Entry × 1 Route`) drained smoothly with configurable **minimum interval + random jitter** and **hourly/daily quota guardrails** shared across channels using the same underlying account credential.
4. **Unified Human & AI Plane with Strict Trust Boundaries** — The embedded WebUI and `/mcp` endpoint share one core engine, while sensitive credentials and recipient destinations remain strictly locked inside WebUI-managed encrypted channels.

---

## Architecture Highlights

```mermaid
flowchart LR
    subgraph Ingest["Guarded Ingest Pipeline"]
        F["RSS / Atom / JSON Feed"] --> HC["SSRF-Guarded HTTP Client"]
        HC --> R["Optional Readability (dom_smoothie)"]
        R --> D["4-Tier SHA-256 Dedup & Ammonia Sanitizer"]
    end

    subgraph Storage["SQLite WAL (/data)"]
        D --> E[("entries (Permanent Hash + Ephemeral Body)")]
        E --> T[("delivery_tasks (Lease + Checkpoint Outbox)")]
        GC["Zero-Refcount NOT EXISTS GC"] -.-> E
    end

    subgraph Targets["Paced Delivery Channels"]
        T --> SMTP["SMTP Newsletter (Force-From / Inline CSS)"]
        T --> TG["Telegram Bot API 10.4 (Rich / Card / Media)"]
        T --> NT["Notion 2026-03-11 (Data Source + Markdown v2)"]
    end

    UI["Embedded React 19 WebUI"] <--> Storage
    MCP["Streamable HTTP /mcp (readonly | admin)"] <--> Storage
```

### 1. Permanent 4-Tier Anti-Drift Deduplication & Zero-Refcount `NOT EXISTS` GC

To prevent duplicate floods caused by broken feed generators or tracking-link mutations, FeedCourier computes a per-feed SHA-256 fingerprint across a strict 4-tier fallback chain:

1. **Tier 1 — Native Entry ID**: Atom `<id>`, RSS `<guid>`, or JSON Feed `id`.
2. **Tier 2 — Canonical Permalink**: Normalized URL stripped of marketing parameters (`utm_*`, `fbclid`, `gclid`, `spm`, etc.) and scheme noise.
3. **Tier 3 — Timestamped Title**: `title + original published_at` (**only** when the feed explicitly supplies a publication timestamp, avoiding synthetic "now" timestamp drift).
4. **Tier 4 — Content Prefix**: `title + first 512 bytes` of sanitized plain text.

New feeds and OPML imports support **silent initialization** (`silent_init`, optionally pushing only the single newest entry to verify channel connectivity). Once an entry is older than `RETENTION_HOURS` (default `24h`) and **`NOT EXISTS`** any non-terminal task (`queued`, `delivering`, `retrying`, `deferred`) referencing it, the background GC purges its HTML and media columns—requiring **zero reference counting** and naturally covering unrouted entries, deleted routes, or disabled channels.

### 2. Lease-Based Outbox & Crash-Safe `checkpoint_json` Recovery

```mermaid
stateDiagram-v2
    [*] --> queued
    queued --> delivering: "Atomic claim (set lease_until)"
    deferred --> delivering: "Quota window opens"
    retrying --> delivering: "next_attempt_at reached"
    delivering --> delivering: "Expired lease takeover (reconcile checkpoint_json)"
    delivering --> delivered: "Downstream confirmed"
    delivering --> retrying: "Transient error / 429 Retry-After cooldown"
    delivering --> deferred: "Hourly / daily quota reached"
    delivering --> dead_letter: "Max attempts or permanent 4xx error"
    dead_letter --> queued: "Manual WebUI / MCP retry"
    delivered --> [*]
```

Workers atomically claim outbox tasks by setting `lease_until` over a serialized single-writer SQLite pool (`max_connections = 1`) paired with a concurrent `query_only = ON` read pool. If a container restarts or a worker crashes mid-flight, expired `delivering` tasks are automatically reclaimed without startup scan races and reconciled against `checkpoint_json`:

| Channel | Delivery Semantics | Crash & Rate-Limit Recovery Mechanism |
| :--- | :--- | :--- |
| **Notion (`2026-03-11`)** | **Effective-once** | Persists `create_initiated` before `POST /v1/pages`, records `async_task_id` on `202 Accepted` (`allow_async: true`), polls `/v1/async_tasks/{id}`, and queries the target data source by **`FC-ID`** before recreating on ambiguous crash recovery. |
| **SMTP** | **At-least-once** (client-deduplicable) | Persists a deterministic fingerprint-derived `Message-ID` header alongside `Force-From` (SPF/DKIM/DMARC safe), `Auto-Submitted: auto-generated`, and per-feed `List-Id`. |
| **Telegram (`10.4`)** | **At-least-once** (bounded to 1 batch) | Persists `next_batch_offset` and `sent_message_ids` after each 10-item `sendMediaGroup` batch in Media Mode so `retry_after` or crash recovery resumes from the exact unsent batch. |

### 3. Guarded Outbound Networking, Readability & Per-Resource Proxy

Every outbound request touching user-supplied URLs—feed polling, full-article extraction, anti-hotlink media streaming, favicon discovery, and MCP live previews—passes through a unified **guarded HTTP client**:

- **Connect-Time DNS SSRF Guard**: Custom DNS resolution inspects every resolved IPv4/IPv6 address prior to TCP connect, blocking loopback (`127.0.0.0/8`, `::1`), link-local, cloud metadata (`169.254.169.254`), and private RFC1918/ULA ranges to defeat DNS rebinding.
- **Hop-by-Hop Redirect Re-Validation**: Automatic redirects are disabled; every `30x` hop is re-validated against scheme and IP policies with a bounded redirect cap.
- **Fine-Grained Internal Allowlist**: Permit trusted LAN services such as self-hosted `RSSHub` or a Local Telegram Bot API Server via `SSRF_ALLOWLIST` (CIDRs or hostnames) without disabling SSRF protection globally.
- **Bounded Streaming & Optional Readability**: Strict response body caps (`10 MB` feeds, `5 MB` web pages, `10 MB` photos / `50 MB` media relays) abort oversized streams immediately. Per-feed `fetch_full_content` runs pure-Rust Readability (`dom_smoothie`) behind a dedicated concurrency semaphore and gracefully falls back to the feed summary if blocked.
- **Selective HTTP / SOCKS5 Proxy**: Configure a global proxy via `OUTBOUND_PROXY` or WebUI and toggle `use_proxy` independently per feed and per delivery channel.

### 4. Three Curated Delivery Channels

- **SMTP Newsletters**: Renders clean, responsive inline-CSS single-column reader emails via `minijinja` (`multipart/alternative` with `html2text` plain-text fallback), staying well below Gmail's `102 KB` clipping threshold.
- **Telegram Bot API 10.4 (3 Route-Selectable Modes)**:
  - **Rich Message (`rich_message`)**: Normalizes HTML via [`telegram-rich`](https://github.com/ElleryHe/telegram-rich) into Bot API 10.4 `sendRichMessage` Rich HTML (supporting `<table compact>`, `<blockquote expandable>`, `<tg-button-row>`, code language tags, and lifted media blocks) and fits within official limits (`32,768` chars / `500` blocks / `16` nesting depth / `50` media / `20` columns) at clean block boundaries. Automatically probes Bot API capabilities and degrades gracefully to Compact Card on older Local Bot API Servers.
  - **Compact Card (`compact_card`)**: Concise `sendMessage` digest (`4,096`-char safe truncation) with bold title, source label, summary, canonical link, and route-level `link_preview` toggle.
  - **Media Mode (`media_mode`)**: Extracts `<enclosure>`, `media:content`, and inline media with a fixed 3-line caption (*author/feed*, *Unicode-segmented first sentence*, *canonical URL*), hotlink-protected zero-disk multipart fallback, and checkpointed 10-item `sendMediaGroup` batching.
- **Notion (`Notion-Version: 2026-03-11`)**:
  - **Zero-Friction Database Creation or Existing Data Source Binding**: Auto-creates a 7-column database (`Title`, `Link`, `Source`, `Published`, `State`, `Tag`, `FC-ID`) under `initial_data_source.properties`, or inspects and optionally patches an existing `data_source_id`.
  - **Markdown v2 Normalizer**: Converts sanitized HTML via `htmd`, dynamically expands outer code fences (`4+` backticks) when code blocks contain inner triple backticks, enforces block-boundary truncation (`5,000` blocks) with a canonical link footer, and writes pages atomically with `markdown_version: "v2"` and `allow_async: true` for long-form articles.

### 5. Native Streamable HTTP MCP (`/mcp`) & Defense-in-Depth Guardrails

FeedCourier mounts an `rmcp`-powered Streamable HTTP endpoint at `/mcp` protected by SHA-256 hashed Bearer tokens in two permission scopes:

| Scope | Exposed MCP Tools | Security Guarantee |
| :--- | :--- | :--- |
| **`readonly`** | `list_feeds_status`, `inspect_failed_deliveries`, `preview_feed_latest` | Read-only health diagnostics, outbox inspection, and SSRF-guarded dry-run previews. |
| **`admin`** | All `readonly` tools + `manage_feed`, `configure_route`, `reset_and_poll_feed`, `retry_dead_tasks` | Full feed, filter, route, circuit-breaker, and dead-letter orchestration. |

- **Black-Boxed Credentials & Locked Recipients**: Channel secrets (ChaCha20-Poly1305 AEAD encrypted at rest) and recipient targets (email addresses, `chat_id`, `data_source_id`) can **only** be created or modified in the WebUI. MCP tools receive only numeric channel IDs and redacted display names, preventing prompt injection from exfiltrating data to attacker-controlled endpoints.
- **Untrusted External Content Framing**: Feed titles, summaries, and article previews returned to AI agents are truncated and wrapped in explicit untrusted-content markers to mitigate indirect prompt injection from malicious feeds.

---

## Quick Start

### Option 1: Docker Run

```bash
docker run -d \
  --name feedcourier \
  --restart unless-stopped \
  -p 8080:8080 \
  -v "$(pwd)/data:/data" \
  ghcr.io/elleryhe/feedcourier:latest
```

Open `http://127.0.0.1:8080` to set your administrator password on first launch (or pre-seed `ADMIN_PASSWORD`). FeedCourier automatically generates `/data/master.key` (`0600`) and initializes `/data/feedcourier.db` in SQLite WAL mode.

### Option 2: Docker Compose

```yaml
services:
  feedcourier:
    image: ghcr.io/elleryhe/feedcourier:latest
    container_name: feedcourier
    restart: unless-stopped
    ports:
      - "8080:8080"
    volumes:
      - ./data:/data
    environment:
      DATA_DIR: /data
      LISTEN_ADDR: 0.0.0.0:8080
      RETENTION_HOURS: "24"
      # Optional presets (can also be configured in WebUI):
      # ADMIN_PASSWORD: "change-me-strong-passphrase"
      # MASTER_KEY: "<64-char-hex-or-base64-32-byte-key>"
      # OUTBOUND_PROXY: "socks5h://127.0.0.1:1080"
      # SSRF_ALLOWLIST: "10.0.0.5/32,rsshub.internal"
```

```bash
docker compose up -d
```

### Option 3: Build from Source

Prerequisites: **Rust 1.88+ (2024 Edition)**, **Node.js 24+**, **`pnpm`**, and **`cmake`** (for `aws-lc-rs`).

```bash
# 1. Clone FeedCourier and sibling telegram-rich crate side by side
git clone https://github.com/ElleryHe/FeedCourier.git
git clone --branch v0.0.1 https://github.com/ElleryHe/telegram-rich.git

cd FeedCourier

# 2. Build embedded React 19 WebUI assets into web/dist
corepack enable
pnpm --dir web install --frozen-lockfile
pnpm --dir web build

# 3. Compile single static-asset release binary
cargo build --release --locked

# 4. Launch gateway
DATA_DIR=./data LISTEN_ADDR=127.0.0.1:8080 ./target/release/feedcourier
```

You can also build the standalone multi-stage container image directly from the `FeedCourier` directory (the `Dockerfile` fetches `telegram-rich` `v0.0.1` automatically):

```bash
docker build -t feedcourier:local .
```

---

## Environment Variables

| Variable | Default | Description |
| :--- | :--- | :--- |
| `DATA_DIR` | `./data` (`/data` in Docker) | Persistent directory storing `feedcourier.db` (SQLite WAL), `master.key`, and online `VACUUM INTO` backups. |
| `LISTEN_ADDR` | `0.0.0.0:8080` | Socket address binding the embedded WebUI, `/api/v1/*` REST API, and `/mcp` endpoint. |
| `MASTER_KEY` | *(auto-generated)* | Explicit 32-byte ChaCha20-Poly1305 master key (`64`-char hex or Base64). When omitted, `<DATA_DIR>/master.key` is generated with `0600` permissions. |
| `ADMIN_PASSWORD` | *(interactive setup)* | Sets or rotates the single-admin WebUI password on startup (stored as an Argon2id PHC hash). |
| `OUTBOUND_PROXY` | *(unset)* | Initial global outbound proxy URL (`http://`, `https://`, `socks5://`, `socks5h://`). Falls back to `ALL_PROXY`, `HTTPS_PROXY`, or `HTTP_PROXY` if unset. |
| `SSRF_ALLOWLIST` | *(empty)* | Comma-separated internal CIDRs or hostnames permitted past the private-IP SSRF guard (e.g. `10.0.0.5/32,rsshub.internal`). |
| `RETENTION_HOURS` | `24` | Hours after discovery to retain transient HTML/media payloads before zero-refcount `NOT EXISTS` GC purges closed/unrouted entries. |
| `DB_READ_MAX_CONNECTIONS` | `8` | Maximum concurrent connections in the read-only SQLite WAL connection pool (minimum `1`). |
| `RUST_LOG` | `feedcourier=info,tower_http=info` | Structured `tracing` log filter directive. |

---

## Connecting an MCP Client

1. Sign in to the FeedCourier WebUI and navigate to **Settings → Native MCP Server (`/mcp`)**.
2. Issue a **`readonly`** (inspection & preview) or **`admin`** (full feed & route orchestration) Bearer token and copy the one-time secret.
3. Add FeedCourier to any Streamable HTTP MCP client (Claude Desktop, Cursor, Windsurf, etc.):

```json
{
  "mcpServers": {
    "feedcourier": {
      "type": "streamable-http",
      "url": "http://127.0.0.1:8080/mcp",
      "headers": {
        "Authorization": "Bearer fc_mcp_your_token_here"
      }
    }
  }
}
```

---

## License

- **FeedCourier** is licensed under the **GNU Affero General Public License v3.0 only ([`AGPL-3.0-only`](./LICENSE))**.
  - In compliance with **AGPLv3 Section 13** (Remote Network Interaction), both the embedded WebUI footer (`/api/v1/auth/status`) and the `/mcp` `ServerInfo` metadata natively publish the build version, `AGPL-3.0-only` license identifier, and source repository URL. If you deploy a modified version accessible over a network, you must make the Corresponding Source of your modified version available to its users under the same license.
- The standalone Telegram Bot API 10.4 rich-message toolkit **[`telegram-rich`](https://github.com/ElleryHe/telegram-rich)** is independently maintained and dual-licensed under **`MIT OR Apache-2.0`**.
- Dependency license compliance across the entire crate graph is continuously audited in CI via [`cargo-deny`](./deny.toml).
