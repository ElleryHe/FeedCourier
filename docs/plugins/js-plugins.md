---
description: "Install and configure sandboxed in-process JS Plugins in FeedCourier (v0.3.1+) in one click from the WebUI Marketplace — including official ntfy and Bark push channels with zero sidecar containers."
---

# JS Plugins (`ntfy`, `Bark` & Custom `.js`)

Starting in **`v0.3.1`**, FeedCourier embeds a **pure-Rust in-process JavaScript sandbox (`boa_engine`)** that runs single-file `plugin.js` extensions directly inside the FeedCourier binary — **zero Docker Compose sidecars, zero extra ports, and zero external Node.js or Python runtimes**.

Administrators can install official push channels such as **ntfy** and **Bark** with **one click from the WebUI Plugin Marketplace**, inspect every line of installed script source, or upload custom single-file `.js` plugins.

> [!IMPORTANT]
> JS plugins receive rendered article content (title, summary, sanitized HTML / plain text, and URL) and execute inside an isolated `boa_engine` runtime with no filesystem, shell, or environment-variable access. Only install plugins from trusted repositories or review their source and `allowed_hosts` carefully before confirming installation.

---

## Sandbox & Network Security Boundary

- **Pure-Rust In-Process Sandbox (`boa_engine`)**: Plugins execute per invocation in a fresh sandboxed JS context without access to the local filesystem, environment variables, OS processes, or raw sockets.
- **Explicit `allowed_hosts` + Explicit-Admin `format: "uri"` Policy**: `fc.fetch()` is strictly restricted to:
  1. Public hostnames explicitly declared in the plugin's `meta.allowed_hosts` (for example, `ntfy.sh` or `api.day.app`), which are displayed as security badges on Marketplace cards and in the installation confirmation modal and traverse standard connect-time DNS SSRF protection (`ProtectedHttpClient`).
  2. Exact `(scheme, host, port)` origins from **non-empty `format: "uri"` fields explicitly submitted by an administrator** (such as a self-hosted LAN/Docker `server_url` for `ntfy` or `Bark`), which connect directly without relaxing global `SSRF_ALLOWLIST` for feeds. Schema `default` values are **never** automatically trusted or persisted into the private/LAN `admin_uri_origins` allowlist (default public hosts like `ntfy.sh` or `api.day.app` must be listed in `meta.allowed_hosts`). Additionally, a plugin's `validate()` `normalized_config` cannot introduce a new `format: "uri"` field or alter the `(scheme, host, port)` origin of an existing `format: "uri"` field.
- **Cloud Metadata Hard-Block, Proxy Resolution Boundary & `use_proxy`**:
  - Cloud metadata literal IPs (`169.254.169.254`, `169.254.170.2`, `100.100.100.200`, `fd00:ec2::254`, including IPv4-mapped IPv6/NAT64 forms), unspecified addresses (`0.0.0.0`, `::`), metadata hostnames (`metadata.google.internal`, `metadata.goog`, `instance-data`), and HTTP `3xx` redirects are **unconditionally blocked** on every `fc.fetch()` call (direct or proxied).
  - Direct connections and local-DNS proxies (`socks5://`) also resolve DNS and block metadata IPs before connecting; remote-DNS proxies (`http://`, `https://`, `socks5h://`) enforce hostname and literal-IP checks client-side because DNS resolution occurs on the proxy server.
  - Plugin Channel accounts (`ChannelKind::Plugin`) support the account-level **Use Outbound Proxy** (`use_proxy`) toggle (fail-closed when no global `OUTBOUND_PROXY` is configured).
- **Concurrency Cap, Timeouts & Automatic Circuit Breaker**:
  - JS execution is capped globally at **4 simultaneous sandbox invocations**. Each hook enforces a wall-clock deadline and checks cancellation at every `fc.*` host call (preventing post-timeout HTTP calls), with per-request `fc.fetch()` timeouts clamped to the remaining hook budget.
  - If a JS plugin hits **3 consecutive execution timeouts**, an automatic circuit breaker disables the plugin globally and records the reason in `plugins.disabled_reason`, which is displayed as a red **Auto-disabled** alert banner on the Plugins page until an administrator investigates and re-enables it.
  - Note: `boa_engine` 0.22 does not provide an external preemption interrupt for pure-CPU nested loops, so a malicious pure-CPU loop can occupy 1 of the 4 sandbox slots until Boa's own per-loop iteration limit completes.
- **SHA-256 Integrity Verification & Encrypted Credentials**: Marketplace downloads are verified against the SHA-256 checksum recorded in `index.json` before preview or installation. Secret fields declared in `account_schema` / `destination_schema` (`format: "password"` or `x-secret: true`) are encrypted at rest with ChaCha20-Poly1305 (`MasterKey`) and masked in all API responses.

---

## One-Click Installation from the WebUI Marketplace

1. Open **Plugins → Marketplace** in the FeedCourier WebUI.
2. Review the plugin card's description, version, and **`Allowed hosts`** domain pills.
3. Click **Install** (or **Update to vX.Y.Z** when a newer version is available):
   - FeedCourier fetches the versioned `plugin.js` from the registry (`POST /api/v1/plugins/install` with `dry_run: true`), verifies its **SHA-256 checksum**, compiles `export const meta` inside the sandbox, and shows the **Verified SHA-256**, **`Allowed Outbound Hosts`**, and version changelog.
   - Select another version from the dropdown if multiple versions are published.
4. Click **Confirm & Install** to persist the plugin into SQLite and activate it immediately.

---

## Official Channel Quick Starts

### 1. ntfy Push (`ntfy`)

Deliver RSS, Atom, and JSON Feed articles to [`ntfy.sh`](https://ntfy.sh) or your self-hosted ntfy server with configurable topics, priorities, tags, and bearer token authentication.

1. Open **Plugins → Marketplace** → click **Install** on **ntfy Push** → **Confirm & Install**.
2. Open **Channels → New Account** → select **ntfy Push**.
3. Configure the **Account** fields:
   - **Server URL** (`server_url`): `https://ntfy.sh` (or your self-hosted URL, e.g. `https://ntfy.example.com` or `http://ntfy:80` — works out of the box without editing `SSRF_ALLOWLIST`).
   - **Access Token** (`access_token`, optional): Bearer token (`tk_...`) if your topic requires authentication.
   - **Use Outbound Proxy** (optional): Enable if FeedCourier needs your global HTTP/SOCKS5 proxy to reach `ntfy.sh`.
4. Configure the **Initial Destination** fields:
   - **Topic** (`topic`): Your ntfy topic name (for example `feedcourier-alerts`).
   - **Priority** (`priority`, optional) & **Tags** (`tags`, optional).
5. Click **Verify Account** or **Send Test**, then attach the destination to any delivery route.

### 2. Bark Push (`bark`)

Deliver clean iOS / macOS push notifications via [`api.day.app`](https://bark.day.app/) or a self-hosted Bark server, formatted with a **Telegram Compact Card-style summary layout** (concise excerpt, feed source attribution, and tap-to-open article URL) and automatic UTF-8 byte-budget truncation for APNs payload limits.

1. Open **Plugins → Marketplace** → click **Install** on **Bark Push** → **Confirm & Install**.
2. Open **Channels → New Account** → select **Bark Push**.
3. Configure the **Account** fields:
   - **Server URL** (`server_url`): `https://api.day.app` (or your self-hosted Bark server URL — works directly for LAN/Docker servers without editing `SSRF_ALLOWLIST`).
   - **Use Outbound Proxy** (optional): Enable if FeedCourier reaches `api.day.app` through your global outbound proxy.
4. Configure the **Initial Destination** fields:
   - **Device Key** (`device_key`): Your Bark device key (stored encrypted with ChaCha20-Poly1305 and masked in API responses).
   - Optional notification options such as **Group**, **Sound**, **Level**, or **Icon**.
5. Click **Send Test** to verify delivery on your Apple device, then bind the destination in **Routes**.

---

## Importing a Custom Single-File `.js` Plugin

For private webhooks, internal gateways, or local development without publishing to the public registry:

1. Open **Plugins** and click **Import Custom Plugin (.js)** in the top-right header.
2. Either click **Choose .js File** to upload a local `plugin.js` / `.mjs` file, or paste the ES module source directly into the monospace editor.
3. Click **Preview & Validate** (`POST /api/v1/plugins/upload` with `dry_run: true`) to compile the script inside `boa_engine` and inspect its extracted `meta`, `script_sha256`, `allowed_hosts`, and core compatibility.
4. If the plugin ID is already installed and you want to upgrade it in place, check **Overwrite if plugin ID is already installed**.
5. Click **Confirm & Install**. Custom-imported plugins carry a **`Custom`** badge alongside their runtime badge.

---

## Lifecycle Operations

- **View Source**: Click **View Source** (`GET /api/v1/plugins/{id}/source`) on any installed JS plugin card or detail page to audit the exact stored `plugin.js` script and its verified SHA-256 digest at any time.
- **One-Click Updates, Version Switching & Custom-Overwrite Guard**: When a newer version appears in the Marketplace index, installed cards show an **Update to vX.Y.Z** button. Upgrading updates the stored script and synced schemas in place while preserving existing channel accounts, destinations, and routes. If the plugin ID is currently installed from a **Custom** upload (`source: "custom"`), Marketplace installation returns **`409 Conflict`** unless explicitly confirmed (`replace_custom: true` via the red confirmation warning in the WebUI).
- **Disable / Pause & Auto-Disable Recovery**: Toggling a channel plugin off defers pending outbox tasks cleanly (`status = 'deferred'`) without consuming retry attempts or dead-lettering them. Plugins tripped by the 3-consecutive-timeout circuit breaker display an **Auto-disabled** alert banner (`disabled_reason`); manually re-enabling the plugin clears `disabled_reason` and resets the timeout counter.
- **Safe Uninstall**: Uninstalling a plugin still referenced by channels or routes returns **`409 Conflict`** listing all referencing channels and routes, requiring explicit `?cascade=true` confirmation.
