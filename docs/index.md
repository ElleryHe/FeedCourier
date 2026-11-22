---
layout: home
title: "FeedCourier"
titleTemplate: ":title — Self-Hosted RSS, Atom & JSON Feed Delivery Service"
description: "Self-hosted RSS, Atom, and JSON Feed subscription delivery service built with Rust, SQLite, and React. Delivers articles to SMTP Email, Telegram, and Notion, with an embedded WebUI and MCP server."

hero:
  name: "FeedCourier"
  text: "RSS / Atom / JSON Feed Subscription Delivery Service"
  tagline: "Built with Rust, SQLite, and React. Polls feeds, extracts full text when needed, filters by route rules, and delivers articles to SMTP Email, Telegram, and Notion, with an embedded web console and /mcp server."
  image:
    light: /logo-light.svg
    dark: /logo-dark.svg
    alt: FeedCourier
  actions:
    - theme: brand
      text: Quick Start
      link: /guide/quick-start
    - theme: alt
      text: User Guide
      link: /guide/introduction
    - theme: alt
      text: Architecture
      link: /reference/architecture

features:
  - title: Built-In Plugin Framework & Link Rewrite
    details: Protocol v1 delivery-time hooks (on_render) with a built-in YOURLS Link Rewrite plugin, encrypted multi-provider instances, 90-day SQLite short-link cache, per-route selection, and a live marketplace catalog powered by ElleryHe/FeedCourier-Plugins.
    link: /plugins/
    linkText: Explore Plugins & Marketplace
  - title: Compliant SMTP Email Delivery
    details: Sends individual per-recipient emails with manual-trigger double opt-in verification (6-digit code + optional /v/{token}), RFC 8058 one-click unsubscribe (/u/{token}), synchronous 5xx hard-bounce protection, Force-From alignment, and responsive Outlook-safe templates.
    link: /channels/smtp
    linkText: Set Up SMTP Email
  - title: Send Rate Pacing & Per-Route Timezones
    details: Enqueues an independent task per matched article and route (1 Task = 1 Entry × 1 Route), sharing minimum intervals, per-delivery random jitter, hourly/daily quotas, and 429 cooldowns across all destinations of a channel account, with per-route IANA timezone overrides.
    link: /channels/pacing-and-quotas
    linkText: Configure Pacing & Quotas
  - title: Telegram Delivery Modes
    details: Connect one Bot account to multiple chats, groups, topics, or channels. Supports Rich Message (native footers, heading demotion, image collages/slideshows), Compact Card, and Media Mode per route, with in-memory anti-hotlink relay and Local Bot API fallback.
    link: /channels/telegram
    linkText: Set Up Telegram
  - title: Notion Database Sync
    details: Reuse one Notion Integration across multiple databases—initializing standard 8-column properties (including optional Short URL) or binding existing data sources with automatic 404 data_source_id writeback, Markdown v2 conversion, and asynchronous long-page creation.
    link: /channels/notion
    linkText: Set Up Notion
  - title: Embedded Web Console & 10 MCP Tools
    details: Ships a React web console and a Streamable HTTP /mcp endpoint (10 tools including secret-free list_plugins) in a single binary with ChaCha20-Poly1305 encrypted credentials, destination lock, TOTP 2FA, and connect-time DNS SSRF protection.
    link: /mcp/setup
    linkText: Connect MCP Clients
---

## Delivery Pipeline Overview

```mermaid
flowchart LR
    subgraph Ingest["Fetch & Parse Pipeline"]
        F["RSS / Atom / JSON Feed"] --> HC["SSRF-Protected HTTP Client"]
        HC --> R["Optional Readability Extraction"]
        R --> D["SHA-256 Dedup & HTML Sanitization"]
    end

    subgraph Storage["SQLite Storage (/data)"]
        D --> E[("Article Fingerprints & Content")]
        E --> T[("Delivery Task Queue")]
        GC["Expired Content Cleanup"] -.-> E
    end

    subgraph Targets["Delivery Channels"]
        T --> SMTP["SMTP Email"]
        T --> TG["Telegram"]
        T --> NT["Notion"]
    end

    UI["Embedded Web Console"] <--> Storage
    MCP["Streamable HTTP /mcp"] <--> Storage
```

## Quick Deployment with Docker

```bash
docker run -d \
  --name feedcourier \
  --restart unless-stopped \
  -p 8080:8080 \
  -v "$(pwd)/data:/data" \
  -e TZ=America/Los_Angeles \
  elleryhe/feedcourier:latest
```

After starting the container, open `http://127.0.0.1:8080` in your browser to set the administrator username and password, configure delivery channels, import OPML subscriptions, or issue `/mcp` tokens. For binary and source builds, see [Quick Start](/guide/quick-start).
