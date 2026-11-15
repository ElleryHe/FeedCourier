# Project Overview

**FeedCourier** is a self-hosted subscription delivery service that polls RSS, Atom, and JSON Feed sources and delivers new articles to **Email (SMTP)**, **Telegram**, and **Notion**.

It is designed as a delivery service rather than a local feed reader: reading, searching, and long-term archiving happen in your email client, Telegram chats, or Notion databases. Once articles are delivered to all matched channels and pass the retention window (`24` hours by default), cached article bodies and media metadata are automatically cleaned up while compact SHA-256 content hashes are kept to prevent duplicate deliveries.

---

## Key Capabilities

- **Feed Polling & Full-Text Extraction**: Polls RSS, Atom, and JSON Feed subscriptions on configurable intervals with HTTP conditional caching (`ETag` / `Last-Modified`) and optional full-article extraction for summary-only feeds.
- **Multi-Channel Delivery**:
  - **[SMTP Email](/channels/smtp)**: Sends single-article HTML and plain-text emails with sender alignment (`Force-From`) and standard mailing-list headers.
  - **[Telegram](/channels/telegram)**: Supports private chats, groups, forum topics, and channels with three route-selectable modes: Rich Message, Compact Card, and Media Mode.
  - **[Notion](/channels/notion)**: Creates pages in a Notion database with mapped article properties and Markdown-converted body content, supporting one-click database initialization or binding to an existing database.
- **Send Rate Control, Quotas & Route Filters**: Spaces out bursts using per-channel minimum intervals, random jitter, and optional hourly/daily quotas, and filters incoming entries by feed tags, keywords, or regular expressions ([Send Rate, Quotas & Filters](/channels/pacing-and-quotas)).
- **WebUI & MCP Integration**: Ships with an embedded web console for managing channels, feeds, routes, and settings, alongside a Streamable HTTP Model Context Protocol endpoint (`/mcp`) for inspecting status, previewing feeds, and managing routes from AI clients ([Client Setup & Security](/mcp/setup)).

---

## Tech Stack

| Layer | Technologies | Role |
| :--- | :--- | :--- |
| **Backend** | Rust, Axum, Tokio | HTTP server, background feed scheduler, and outbox delivery worker. |
| **Storage** | SQLite (`WAL` mode) | Single-file persistent storage for configuration, feed state, content hashes, and delivery queues. |
| **Feed & HTML Processing** | `feed-rs`, `dom_smoothie`, `ammonia` | RSS / Atom / JSON Feed parsing, Readability full-text extraction, and HTML sanitization. |
| **Channel Adapters** | `lettre`, [`telegram-rich`](https://crates.io/crates/telegram-rich), Notion REST API | SMTP email delivery, Telegram message formatting and media upload, and Notion page synchronization. |
| **MCP Server** | `rmcp` | Streamable HTTP `/mcp` server providing 7 structured tools with `readonly` and `admin` scopes. |
| **Frontend** | React 19, TypeScript, Tailwind CSS v4, Vite | Single-page web console embedded directly into the server binary. |

---

## Documentation Structure

The documentation is organized into two sections:

1. **User Guide** (this section): Covers installation and deployment ([Quick Start](/guide/quick-start)), environment variables and proxy configuration ([Configuration & Proxy](/guide/configuration)), backups ([Backup & Maintenance](/guide/operations)), [Delivery Channels](/channels/pacing-and-quotas), and [MCP Integration](/mcp/setup).
2. **[Architecture Manual](/reference/architecture)**: Covers internal system architecture, [channel protocol and delivery details](/reference/channel-protocols), [security and isolation mechanisms](/reference/security), [database schema](/reference/database-schema), [REST API endpoints](/reference/rest-api), and the [build and release pipeline](/reference/releasing).
