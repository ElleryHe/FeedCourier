---
description: "Overview of FeedCourier, a self-hosted RSS, Atom, and JSON Feed delivery service for SMTP Email, Telegram, and Notion with deduplication and MCP support."
---

# Project Overview

**FeedCourier** is a self-hosted subscription delivery service that polls RSS, Atom, and JSON Feed sources and delivers new articles to **Email (SMTP)**, **Telegram**, and **Notion**.

It is designed as a delivery service rather than a local feed reader: reading, searching, and long-term archiving happen in your email client, Telegram chats, or Notion databases. Once articles are delivered to all matched channels and pass the retention window (`24` hours by default), cached article bodies and media metadata are automatically cleaned up while compact SHA-256 content hashes are kept to prevent duplicate deliveries.

---

## Key Capabilities

- **Feed Polling, Filtering & Full-Text Extraction**: Polls RSS, Atom, and JSON Feed subscriptions on configurable intervals with HTTP conditional caching (`ETag` / `Last-Modified`), optional full-article extraction for summary-only feeds, and a WebUI Feeds list with tag filtering, persistent CJK/Latin sorting, and pagination.
- **Two-Layer Multi-Channel Delivery (`Account` + `Destinations`)**: Configure credentials, outbound proxy, pacing, and quotas once per **Channel Account**, then attach multiple independently toggleable and testable **Destinations**:
  - **[SMTP Email](/channels/smtp)**: Sends single-article responsive HTML and plain-text emails (with Outlook-safe image scaling, localized timestamps, sender alignment `Force-From`, and standard `List-Id` headers) to one or more recipient groups under the same SMTP account.
  - **[Telegram](/channels/telegram)**: Connects one Bot account to multiple private chats, groups, forum topics, and channels, with three route-selectable modes: Rich Message (`telegram-rich 0.0.2` native footers, heading demotion, and image collages/slideshows), Compact Card, and Media Mode.
  - **[Notion](/channels/notion)**: Connects one Notion Integration to multiple databases, creating pages with mapped article properties and Markdown v2 body content via one-click database initialization or binding to existing databases.
- **Send Rate Control, Quotas & Route Rules**: Shares minimum intervals, per-delivery random jitter, rolling hourly/daily quotas, and `429` cooldowns across all destinations of a Channel Account; filters incoming entries by feed tags, keywords, or regular expressions; supports per-route delivery language, IANA timezone overrides (for delivery windows and article timestamps), and weekdays; and lets you pause the global queue ([Send Rate, Quotas, Windows & Filters](/channels/pacing-and-quotas)). Backlogs and multiple feeds are delivered in chronological order.
- **WebUI & MCP Integration**: Ships with an embedded web console (featuring channel/route cascading filters, semantic outbox status colors, and username/password sign-in with optional TOTP 2FA), alongside a Streamable HTTP Model Context Protocol endpoint (`/mcp`) for inspecting status, previewing feeds, and managing routes by redacted `destination_id` from AI clients ([Client Setup & Security](/mcp/setup)).

---

## Tech Stack

| Layer | Technologies | Role |
| :--- | :--- | :--- |
| **Backend** | Rust, Axum, Tokio | HTTP server, background feed scheduler, and outbox delivery worker. |
| **Storage** | SQLite (`WAL` mode) | Single-file persistent storage for configuration, feed state, content hashes, and delivery queues. |
| **Feed & HTML Processing** | `feed-rs`, `dom_smoothie`, `ammonia` | RSS / Atom / JSON Feed parsing, Readability full-text extraction, and HTML sanitization. |
| **Channel Adapters** | `lettre`, [`telegram-rich`](https://crates.io/crates/telegram-rich), Notion REST API | SMTP email delivery, Telegram message formatting and media upload, and Notion page synchronization. |
| **MCP Server** | `rmcp` | Streamable HTTP `/mcp` server providing 9 structured tools with `readonly` and `admin` scopes. |
| **Frontend** | React 19, TypeScript, Tailwind CSS v4, Vite | Single-page web console embedded directly into the server binary. |

---

## Documentation Structure

The documentation is organized into two sections:

1. **User Guide** (this section): Covers installation and deployment ([Quick Start](/guide/quick-start)), environment variables and proxy configuration ([Configuration & Proxy](/guide/configuration)), backups ([Backup & Maintenance](/guide/operations)), [Delivery Channels](/channels/pacing-and-quotas), and [MCP Integration](/mcp/setup).
2. **[Architecture Manual](/reference/architecture)**: Covers internal system architecture, [channel protocol and delivery details](/reference/channel-protocols), [security and isolation mechanisms](/reference/security), [database schema](/reference/database-schema), [REST API endpoints](/reference/rest-api), and the [build and release pipeline](/reference/releasing).
