---
description: "Connect Claude Desktop, Cursor, or custom AI clients to FeedCourier's Streamable HTTP /mcp endpoint using scoped readonly or admin Bearer tokens."
---

# Client Setup & Security

FeedCourier provides a built-in **Model Context Protocol (MCP)** server at **`/mcp`** over the **Streamable HTTP** transport on the same port as the WebUI (default `0.0.0.0:8080`).

Any MCP client that supports Streamable HTTP (or `mcp-remote`) can connect directly using a Bearer token issued in the WebUI to check feed status, inspect failed deliveries, preview full-text extraction and filters, or manage feeds and routes.

---

## 1. Issue a Bearer Token in the WebUI

1. Sign in to the FeedCourier WebUI (`http://127.0.0.1:8080`).
2. Open **Settings → Native MCP Server (`/mcp`)**.
3. Enter a name for the token (for example, `Claude Desktop` or `Cursor`) and choose a permission scope:

| Scope | Allowed Tools | Typical Use |
| :--- | :--- | :--- |
| **`readonly`** | `list_feeds_status`, `inspect_failed_deliveries`, `preview_feed_latest` | Status checks, delivery failure diagnostics, and dry-run feed/filter previews without modifying any data. |
| **`admin`** | All `readonly` tools **plus** `manage_feed`, `configure_route`, `reset_and_poll_feed`, `retry_dead_tasks`, `manage_outbox_queue`, `test_push_feed_entry` | Adding, updating, pausing, clearing the cache of, or deleting feeds, configuring route rules (language, time windows), resetting feeds, retrying failed tasks, pausing the global queue, and debug-pushing an entry to a channel. |

4. Click **Issue Token** and copy the generated `fc_mcp_...` token (or the ready-to-paste client JSON configuration).

::: info Token Storage
The plaintext token is shown only once when created. FeedCourier stores only a SHA-256 hash and a short prefix in the database. If you lose a token, revoke it in the WebUI and create a new one.
:::

---

## 2. Configure Your MCP Client

### Cursor / Windsurf / Streamable HTTP Clients

Add the following entry to your client's `mcpServers` configuration file (such as `~/.cursor/mcp.json`):

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

### Claude Desktop (via `mcp-remote` if Custom Headers Require `stdio`)

If your desktop client version requires a local `stdio` command to attach custom `Authorization` headers, you can bridge to `/mcp` with `mcp-remote`:

```json
{
  "mcpServers": {
    "feedcourier": {
      "command": "npx",
      "args": [
        "-y",
        "mcp-remote",
        "http://127.0.0.1:8080/mcp",
        "--header",
        "Authorization: Bearer fc_mcp_your_token_here"
      ]
    }
  }
}
```

### Claude Code CLI

```bash
claude mcp add --transport http feedcourier http://127.0.0.1:8080/mcp \
  --header "Authorization: Bearer fc_mcp_your_token_here"
```

::: tip Remote HTTPS Deployments
If your FeedCourier instance is hosted behind a reverse proxy (see [Reverse Proxy & HTTPS Deployment](/guide/operations#reverse-proxy-https-deployment)), replace `http://127.0.0.1:8080/mcp` with `https://feedcourier.example.com/mcp` and make sure response buffering is disabled (`flush_interval -1` in Caddy or `proxy_buffering off;` in Nginx).
:::

---

## 3. Security Overview

Connecting an AI assistant to an application that processes external web feeds requires clear permission boundaries:

- **Permission Scopes (`readonly` vs. `admin`)**: A `readonly` token can only inspect feed health, view failed delivery logs, and run non-persistent feed previews. Only `admin` tokens can modify feeds, routes, or queued tasks, pause or resume the global delivery queue, or send debug pushes.
- **Channel & Recipient Isolation (WebUI Only)**: Channel credentials (SMTP passwords, Telegram Bot Tokens, Notion Integration Secrets) and delivery destinations (recipient email addresses, Telegram `chat_id`, Notion databases) can **only** be created or modified by an administrator signed in to the WebUI. The MCP server does not provide any tool to create or edit channels, and channel listings in MCP only return numeric channel IDs and display names—never credentials or destination addresses.
- **Debug Push Destination Lock**: `test_push_feed_entry` can only target an existing `channel_id` and never accepts credentials or recipient addresses. It is stateless (no fingerprints, queue entries, or quota use), requires an `admin` token, and its target URLs pass the same SSRF checks as previews.
- **Prompt Injection & Private Network Protection**: External feed titles, summaries, and upstream error messages returned to MCP clients are truncated and wrapped as untrusted content so malicious text inside an article cannot trick an AI assistant into executing unintended actions. In addition, live feed previews (`preview_feed_latest`) and feed additions (`manage_feed`) automatically block requests to private and loopback network addresses unless explicitly permitted in [`SSRF_ALLOWLIST`](/guide/configuration#allowing-trusted-internal-services-ssrf-allowlist).

For technical details on credential encryption, content sanitization, and connection-layer SSRF checks, see [Security & Isolation](/reference/security).

---

## 4. Example Prompts

Once connected, you can ask your AI assistant to help with everyday tasks:

- **Status & Failure Diagnostics (`readonly`)**:
  > *"Check my FeedCourier status. Are any feeds in `backoff` or `circuit_open`? Inspect any dead-letter delivery tasks and explain why they failed."*
- **Previewing Full-Text Extraction & Filters (`readonly`)**:
  > *"Preview `https://blog.rust-lang.org/feed.xml` with `fetch_full_content: true` and test an `include_keywords` filter for `['release', 'security']`. Show me which recent posts match."*
- **Subscribing & Routing (`admin`)**:
  > *"Subscribe to `https://lwn.net/headlines/rss` with tags `['linux', 'kernel']` and `fetch_full_content: true`, then route the `linux` tag to my Notion channel."*
- **Controlling the Queue (`admin`)**:
  > *"Pause the delivery queue for 2 hours, then clear the cache of feed #4 without re-fetching."*
- **Recovering Paused Feeds & Failed Tasks (`admin`)**:
  > *"Reset and poll feed #4 immediately, then retry all dead-letter delivery tasks."*

See [Tools Overview](/mcp/tools) for the parameters and examples of all 9 MCP tools.
