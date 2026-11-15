# Configuration & Proxy Routing

FeedCourier follows a **zero-config-first** philosophy: only `DATA_DIR` and `LISTEN_ADDR` are needed for standard deployments, and both have sensible defaults. Runtime network settings (outbound proxy and internal SSRF allowlist) can be seeded via environment variables on first boot and hot-reloaded at any time from the WebUI (`Settings`).

---

## Environment Variables Reference

All environment variables are resolved at startup by `AppConfig::from_env()` in `src/config.rs`:

| Variable | Default | Description |
| :--- | :--- | :--- |
| `DATA_DIR` | `./data` (`/data` in Docker) | Persistent directory storing `feedcourier.db` (SQLite WAL), `master.key`, and online `VACUUM INTO` backups. |
| `LISTEN_ADDR` | `0.0.0.0:8080` | Socket address (`IP:PORT`) binding the embedded WebUI, `/api/v1/*` REST API, and `/mcp` Streamable HTTP endpoint. |
| `MASTER_KEY` | *(auto-generated)* | Explicit 32-byte ChaCha20-Poly1305 master key encoded as `64`-character hex or standard Base64. When omitted, `<DATA_DIR>/master.key` is generated automatically with `0600` file permissions. |
| `ADMIN_PASSWORD` | *(interactive setup)* | Sets or rotates the single-admin WebUI password on startup. Hashed with `Argon2id` and persisted in `system_settings`. |
| `OUTBOUND_PROXY` | *(unset)* | Initial global outbound proxy URL (`http://`, `https://`, `socks5://`, `socks5h://`). Falls back to `ALL_PROXY`, `HTTPS_PROXY`, or `HTTP_PROXY` if unset. |
| `SSRF_ALLOWLIST` | *(empty)* | Comma-separated internal CIDRs or hostnames permitted through the connect-time SSRF guard (e.g., `10.0.0.5/32,rsshub.internal,*.lan`). |
| `RETENTION_HOURS` | `24` | Hours after `discovered_at` to retain sanitized HTML and media payloads before the zero-refcount `NOT EXISTS` GC purges closed/unrouted entries. |
| `DB_READ_MAX_CONNECTIONS` | `8` | Maximum concurrent connections in the read-only SQLite WAL pool (`query_only = ON`, minimum `1`). Writes always serialize through a dedicated `max_connections = 1` pool. |
| `RUST_LOG` | `feedcourier=info,tower_http=info` | Structured `tracing` log filter directive (supports `debug`, `warn`, `error`, or per-module targets like `feedcourier::scheduler=debug`). |

::: warning Rotating `ADMIN_PASSWORD` vs. Persisted Network Settings
- If **`ADMIN_PASSWORD`** is set in the environment, FeedCourier hashes and overwrites the admin password in `system_settings` on **every startup**—making it easy to recover access if you forget your password.
- **`OUTBOUND_PROXY`** and **`SSRF_ALLOWLIST`** act as **initial seeds**: they populate `system_settings` only if no value has been saved in SQLite yet. Once initialized, updates made in the WebUI (`PUT /api/v1/settings`) take precedence and persist across restarts.
:::

---

## Selective Outbound Proxy Routing (`use_proxy`)

In networks where certain upstream feeds, Telegram (`api.telegram.org`), or Notion (`api.notion.com`) require an outbound proxy while domestic feeds or local SMTP relays should connect directly, FeedCourier provides **per-resource proxy toggling**.

```mermaid
flowchart LR
    P["Global Outbound Proxy\n(http://, https://, socks5://, socks5h://)"]
    D["Direct Connection\n(No Proxy)"]

    F1["Feed A (use_proxy = true)"] --> P
    F2["Feed B (use_proxy = false)"] --> D
    C1["Telegram Channel (use_proxy = true)"] --> P
    C2["Internal SMTP Relay (use_proxy = false)"] --> D
```

### Supported Proxy Schemes

- `http://[user:pass@]host:port` — Standard HTTP/CONNECT proxy.
- `https://[user:pass@]host:port` — TLS-wrapped HTTP proxy.
- `socks5://[user:pass@]host:port` — SOCKS5 proxy.
- `socks5h://[user:pass@]host:port` — SOCKS5 proxy with remote DNS resolution (recommended to prevent DNS leaks or local DNS poisoning).

### Environment Fallback Order

When resolving the initial proxy URL at boot, FeedCourier checks:

1. `OUTBOUND_PROXY`
2. `ALL_PROXY`
3. `HTTPS_PROXY`
4. `HTTP_PROXY`

### Enabling Proxy Per Feed or Per Channel

Configuring a global proxy URL does **not** force all traffic through the proxy automatically. Instead, you toggle `use_proxy` independently on:

- **Individual Feeds** (`feeds.use_proxy`): Applies to feed XML/JSON polling, on-demand `dom_smoothie` Readability extraction, favicon discovery, and Telegram Media Mode anti-hotlink fallback fetches for that feed.
- **Individual Channels** (`channels.use_proxy`): Applies to outbound deliveries and connectivity tests for that specific Telegram, Notion, or SMTP channel.
- **Live Previews** (`/api/v1/feeds/preview` & MCP `preview_feed_latest`): Accepts an explicit `use_proxy` boolean per preview call.

---

## Connect-Time SSRF Guard & `SSRF_ALLOWLIST`

Every outbound HTTP request to a user-controlled URL (feed polling, Readability extraction, media relay, favicon discovery, and MCP `preview_feed_latest`) passes through `ProtectedHttpClient`.

By default, FeedCourier **blocks** connections to:
- Loopback addresses (`127.0.0.0/8`, `::1/128`, `localhost`, `*.localhost`)
- Private RFC 1918 IPv4 ranges (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`)
- IPv6 Unique Local (`fc00::/7`) and Link-Local (`fe80::/10`) addresses
- IPv4 Link-Local & Cloud Instance Metadata endpoints (`169.254.0.0/16`, including `169.254.169.254`)
- Carrier-Grade NAT (`100.64.0.0/10`), broadcast, multicast, and unspecified (`0.0.0.0`, `::`) ranges

### Allowing Trusted Internal Services (`SSRF_ALLOWLIST`)

If you run self-hosted companion services on your LAN or Docker bridge network—such as **[RSSHub](https://docs.rsshub.app/)**, **FreshRSS RSS-Bridge**, or a **[Local Telegram Bot API Server](/channels/telegram#self-hosted-local-bot-api-server)**—add their specific CIDR blocks or hostnames to `SSRF_ALLOWLIST` rather than disabling SSRF protection globally.

Supported allowlist entry formats:

| Format | Example | Behavior |
| :--- | :--- | :--- |
| **Exact IPv4 / IPv6 CIDR** | `10.0.0.5/32`, `172.18.0.0/16`, `fd00::1/128` | Permits connections whose resolved IP falls within the CIDR prefix. |
| **Single IP Literal** | `192.168.1.50` | Treated as a `/32` (IPv4) or `/128` (IPv6) host allowlist entry. |
| **Exact Hostname** | `rsshub.internal`, `telegram-bot-api.local` | Case-insensitively permits the hostname and its resolved private IPs. |
| **Wildcard Subdomain** | `*.lan`, `*.internal.example.com` | Permits any subdomain under the suffix (does not match the bare apex unless listed separately). |

#### Example: Allowing a Docker Compose RSSHub Service

```yaml
services:
  feedcourier:
    image: elleryhe/feedcourier:latest
    environment:
      SSRF_ALLOWLIST: "rsshub,172.20.0.0/16"
```

You can also edit the allowlist live in the WebUI under **Settings → Outbound Network & SSRF Guard**. Changes take effect immediately without restarting the process.
