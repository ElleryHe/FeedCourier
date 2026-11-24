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
- **Explicit `allowed_hosts` + Admin URI Boundary**: `fc.fetch()` is strictly restricted to:
  1. Public hostnames explicitly declared in the plugin's `meta.allowed_hosts` (for example, `ntfy.sh` or `api.day.app`), which are displayed as security badges on Marketplace cards and in the installation confirmation modal.
  2. Hostnames explicitly configured by the administrator in `format: "uri"` account/destination fields (such as a self-hosted `server_url`).
- **Dual Network Allowlist, Metadata Hard-Block & `use_proxy`**:
  - Cloud metadata/unspecified targets (`169.254.169.254`, `100.100.100.200`, `fd00:ec2::254`, `0.0.0.0`, `::`, `metadata.google.internal`) and HTTP `3xx` redirects are **unconditionally blocked** on every `fc.fetch()` call.
  - Exact `(scheme, host, port)` origins from administrator-configured `format: "uri"` fields (such as a self-hosted LAN/Docker `server_url` for `ntfy` or `Bark`) connect directly without relaxing global `SSRF_ALLOWLIST` for feeds.
  - Plugin-declared `meta.allowed_hosts` route through FeedCourier's connect-time DNS SSRF guard (`ProtectedHttpClient`).
  - Plugin Channel accounts (`ChannelKind::Plugin`) support the account-level **Use Outbound Proxy** (`use_proxy`) toggle (fail-closed when no global `OUTBOUND_PROXY` is configured).
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
- **One-Click Updates & Version Switching**: When a newer version appears in the Marketplace index, installed cards show an **Update to vX.Y.Z** button. Upgrading updates the stored script and synced schemas in place while preserving existing channel accounts, destinations, and routes.
- **Disable / Pause**: Toggling a channel plugin off defers pending outbox tasks cleanly (`status = 'deferred'`) without consuming retry attempts or dead-lettering them.
- **Safe Uninstall**: Uninstalling a plugin still referenced by channels or routes returns **`409 Conflict`** listing all referencing channels and routes, requiring explicit `?cascade=true` confirmation.
