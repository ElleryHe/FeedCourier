---
layout: home

hero:
  name: "FeedCourier"
  text: "Pure-Rust Streaming Feed Delivery Gateway"
  tagline: "Route RSS, Atom, and JSON Feeds in real time to SMTP Newsletters, Telegram Bot API 10.4, and Notion 2026-03-11 Data Sources—with zero disk bloat, lease-based outbox pacing, and a native Streamable HTTP MCP server."
  image:
    light: /logo-light.svg
    dark: /logo-dark.svg
    alt: FeedCourier
  actions:
    - theme: brand
      text: Quick Start
      link: /guide/quick-start
    - theme: alt
      text: Architecture & Philosophy
      link: /guide/introduction
    - theme: alt
      text: GitHub
      link: https://github.com/ElleryHe/FeedCourier

features:
  - title: Ephemeral Payloads, Permanent Fingerprints
    details: Stores compact 64-char SHA-256 anti-drift fingerprints forever in SQLite WAL while automatically purging HTML and media metadata via a zero-refcount NOT EXISTS garbage collector once delivery closes.
  - title: Three Curated Delivery Channels
    details: Purpose-built adapters for responsive inline-CSS SMTP newsletters (Force-From), Telegram Bot API 10.4 (Rich Message via telegram-rich, Compact Card, and Media Mode), and Notion 2026-03-11 Data Sources with Markdown v2.
  - title: Anti-Spam Pacing & Crash-Safe Outbox
    details: Every matched article enqueues an isolated task (1 Task = 1 Entry × 1 Route) drained with minimum intervals, random jitter, shared credential locks, hourly/daily quotas, and checkpoint_json crash recovery.
  - title: Connect-Time DNS SSRF Guard
    details: All user-supplied URLs pass through a unified HTTP client that inspects resolved IPs before TCP connect, re-validates every 30x redirect hop, caps stream sizes, and supports per-resource HTTP/SOCKS5 proxies.
  - title: Native Streamable HTTP MCP (/mcp)
    details: Built-in rmcp server with readonly and admin Bearer scopes. AI agents can inspect health, preview feeds, and orchestrate routes while credentials stay encrypted and recipients remain locked in WebUI.
  - title: Single Binary, <20 MB Idle RSS
    details: Compiles the Rust async engine, SQLite WAL migrations, and React 19 management console into one standalone executable or multi-arch Docker image with online VACUUM INTO hot backups.
---

## End-to-End Delivery Pipeline

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

## Explore the Documentation

| Section | What You'll Learn |
| :--- | :--- |
| **[Getting Started & Operations](/guide/introduction)** | Core design boundaries, [Docker & binary deployment](/guide/quick-start), [environment variables & proxy routing](/guide/configuration), and [`/data` backups](/guide/operations). |
| **[Delivery Channels](/channels/pacing-and-quotas)** | [Pacing, quotas & content filters](/channels/pacing-and-quotas), [SMTP `Force-From` newsletters](/channels/smtp), [Telegram Bot API 10.4 modes](/channels/telegram), and [Notion `2026-03-11` integration](/channels/notion). |
| **[Native MCP Server](/mcp/setup)** | [Streamable HTTP client setup](/mcp/setup), [all 7 MCP tools](/mcp/tools), and [defense-in-depth prompt-injection guardrails](/mcp/security). |
| **[Architecture & Reference](/reference/architecture)** | [4-tier dedup & lease outbox internals](/reference/architecture), [SQLite WAL schema](/reference/database-schema), [`/api/v1/*` REST API](/reference/rest-api), and [automated release pipelines](/reference/releasing). |
