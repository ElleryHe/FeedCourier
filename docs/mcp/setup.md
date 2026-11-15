# Native MCP Client Setup

FeedCourier embeds a native **Model Context Protocol (MCP)** server powered by the official Rust SDK (`rmcp`) directly on its Axum HTTP listener at **`/mcp`** using the **Streamable HTTP** transport.

No sidecar bridges, Node.js wrappers, or stdio tunnels are required—any MCP client that supports Streamable HTTP can connect directly with a static Bearer token.

---

## Step 1: Issue a Scoped Bearer Token in WebUI

1. Sign in to your FeedCourier WebUI (`http://127.0.0.1:8080`).
2. Navigate to **Settings → Native MCP Server (`/mcp`)**.
3. Enter a descriptive token name (e.g., `Claude Desktop` or `Cursor On-Call`) and select one of the two permission scopes:

| Scope | Permitted Tools | Recommended Use Case |
| :--- | :--- | :--- |
| **`readonly`** | `list_feeds_status`, `inspect_failed_deliveries`, `preview_feed_latest` | Daily health checks, dead-letter diagnostics, and dry-run feed/Readability/filter previews without modifying state. |
| **`admin`** | All `readonly` tools **plus** `manage_feed`, `configure_route`, `reset_and_poll_feed`, `retry_dead_tasks` | Full natural-language feed subscription management, filter tuning, route binding, circuit-breaker recovery, and dead-letter requeueing. |

4. Click **Issue Token** and copy the one-time `fc_mcp_...` secret (or the pre-formatted client JSON snippet).

::: info SHA-256 Token Storage
FeedCourier never stores raw MCP tokens in SQLite. Only the **SHA-256 hex digest** (`mcp_tokens.token_hash`) and a short display prefix (`token_prefix`) are persisted. If you lose a token, revoke it in the WebUI and issue a new one.
:::

---

## Step 2: Configure Your MCP Client

### Claude Desktop / Cursor / Windsurf (Streamable HTTP)

Add the following entry to your client's `mcpServers` configuration file:

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

::: tip Remote HTTPS Deployments
If your FeedCourier instance is hosted behind a reverse proxy (see [Reverse Proxy & HTTPS Deployment](/guide/operations#reverse-proxy-https-deployment)), replace `http://127.0.0.1:8080/mcp` with `https://feedcourier.example.com/mcp` and ensure response buffering is disabled (`flush_interval -1` in Caddy or `proxy_buffering off;` in Nginx).
:::

### Claude Code CLI

```bash
claude mcp add --transport http feedcourier http://127.0.0.1:8080/mcp \
  --header "Authorization: Bearer fc_mcp_your_token_here"
```

---

## Example Natural-Language Workflows

Once connected, you can ask your AI assistant to operate FeedCourier directly:

- **Health & Dead-Letter Triage (`readonly`)**:
  > *"Check my FeedCourier status. Are any feeds in `backoff` or `circuit_open`? Inspect any dead-letter delivery tasks and explain why they failed."*
- **Dry-Run Readability & Filter Tuning (`readonly`)**:
  > *"Preview `https://blog.rust-lang.org/feed.xml` with `fetch_full_content: true` and test an `include_keywords` filter for `['release', 'security']`. Show me which recent posts match."*
- **End-to-End Subscription & Routing (`admin`)**:
  > *"Subscribe to `https://lwn.net/headlines/rss` with tags `['linux', 'kernel']` and `fetch_full_content: true`, then route the `linux` tag to my Notion channel in Rich/Markdown mode."*
- **Circuit-Breaker Recovery (`admin`)**:
  > *"Reset and immediately poll feed #4, then retry all dead-letter tasks."*

See [MCP Tools Reference](/mcp/tools) for the complete JSON schemas of all 7 tools, and [MCP Security Boundaries](/mcp/security) for how FeedCourier prevents prompt injection and data exfiltration.
