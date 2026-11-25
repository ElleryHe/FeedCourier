---
description: "FeedCourier Plugin Framework overview and live Plugin Marketplace catalog powered by ElleryHe/FeedCourier-Plugins (Protocol v1 — built-in Rust plugins & in-process sandboxed JS plugins)."
---

# Plugins & Marketplace

FeedCourier provides a **Protocol v1 (`api_version: "1"`) Plugin Framework** supporting both **built-in Rust plugins** (`v0.3.0+`) and **in-process sandboxed JavaScript plugins (`boa_engine`, `v0.3.1+`)** for custom delivery channels (`kind: "channel"`), delivery-time processors (`kind: "processor"`, `on_render`), and persistent post-delivery callbacks (`on_delivered`) — all inside the single FeedCourier binary with **zero sidecar containers**.

```mermaid
flowchart LR
    Entry[("Stored Entry\n(Immutable canonical_url & HTML)")] --> Hook["on_render Hook Chain\n(RouteOptions.plugin_order)"]
    Hook --> Overrides["RenderOverrides\n(Sanitized HTML / primary_link / warnings)"]
    Overrides --> Channels["Channel Adapters\n(SMTP / Telegram / Notion / JS Plugin Channel)"]
    Channels --> Delivered["Persistent plugin_events\n(on_delivered At-Least-Once)"]
```

---

## Plugin Architecture Overview

- **Two Runtime Models Sharing Protocol v1**:
  - **Built-in (`runtime: "builtin"`)**: First-party Rust plugins compiled directly into the single FeedCourier binary (such as [Link Rewrite (YOURLS)](/plugins/link-rewrite)).
  - **In-Process JS Sandbox (`runtime: "js"`, `v0.3.1+`)**: Single-file `plugin.js` ES modules executed inside a pure-Rust `boa_engine` sandbox — installed in one click from the WebUI Marketplace (with SHA-256 verification) or imported directly via `.js` upload. JS plugins can implement custom **delivery channels** (`deliver`, such as **ntfy** and **Bark**), **render processors** (`onRender`, with `ammonia` HTML re-sanitization), or **post-delivery subscribers** (`onDelivered`).
- **Strict Sandbox & Network Isolation**: JS plugins have zero filesystem, shell, or environment access. Outbound HTTP calls through `fc.fetch()` are constrained strictly to declared `allowed_hosts` plus explicit administrator-configured URI fields, enforced by connect-time DNS SSRF protection and supporting channel `use_proxy` for public hosts.
- **Per-Route Configuration & Execution Ordering (`RouteOptions.plugins` & `plugin_order`)**: Enable processor plugins globally, configure per-route options, and control execution sequence with up/down ordering arrows in the route editor. Each plugin runs under an isolated 15s wall-clock timeout; non-strict failures record warnings without blocking delivery.
- **Persistent `on_delivered` Events (`plugin_events`)**: Subscriptions to `on_delivered` are enqueued inside the same SQLite transaction that commits task delivery and dispatched asynchronously with an idempotency key and exponential backoff (up to 5 attempts).
- **Safe Lifecycle & Compatibility Checks**:
  - Marketplace and uploaded manifests are checked against `api_version` and `min_core` before installation.
  - Uninstalling an in-use plugin or deleting a referenced provider instance returns **`409 Conflict`** listing referencing channels and routes (`?cascade=true` supported for plugins).

---

## Plugin Marketplace

The catalog below is loaded at build time from the official **[`ElleryHe/FeedCourier-Plugins`](https://github.com/ElleryHe/FeedCourier-Plugins)** registry (`index.json`), with an automatic fallback to the compiled snapshot in `src/plugins/market_snapshot.json` for offline builds.

<PluginMarketplace locale="en" />

### Runtime Marketplace Loading in FeedCourier

When you open **Plugins → Marketplace** in the WebUI (`GET /api/v1/plugins/market`):

- FeedCourier fetches `https://raw.githubusercontent.com/ElleryHe/FeedCourier-Plugins/main/index.json` through the SSRF-protected HTTP client (up to `1 MiB`, `8s` timeout) and caches the result in memory for **6 hours**.
- Set [`PLUGIN_INDEX_URL`](/guide/configuration#environment-variables-reference) to point to a private mirror or self-hosted registry.
- In air-gapped networks where the remote URL is unreachable, FeedCourier automatically falls back to its built-in snapshot (`source: "snapshot"`).
- Built-in (`kind: "builtin"`) plugins can be activated in one click; JS (`runtime: "js"`) listings display their declared **`Allowed hosts`** domain pills and open the **One-Click Install / Update** preview modal with SHA-256 verification and version selector; incompatible entries display their required core version (`min_core`).

---

## Next Steps

- **[JS Plugins (ntfy & Bark)](/plugins/js-plugins)**: One-click WebUI Marketplace setup for **ntfy** and **Bark**, custom `.js` uploads, source inspection, and sandbox network security.
- **[Link Rewrite (YOURLS)](/plugins/link-rewrite)**: Set up self-hosted YOURLS short-link rewriting, authentication modes, `SSRF_ALLOWLIST`, caching, and channel-specific behavior.
- **[Plugin Development](/plugins/development)**: Plugin API v1 (`plugin.js`) specification, `globalThis.fc` host utilities, sandbox limits, unit testing with `node --test`, and submitting to [`ElleryHe/FeedCourier-Plugins`](https://github.com/ElleryHe/FeedCourier-Plugins).
