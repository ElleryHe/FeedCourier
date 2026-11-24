---
description: "Install and configure out-of-process HTTP Bridge Channel Plugins in FeedCourier (v0.3.1+), including the official zero-dependency ntfy reference plugin."
---

# Bridge Channel Plugins (HTTP Bridge)

Starting in **`v0.3.1`**, FeedCourier supports **out-of-process HTTP Bridge Plugins** (`runtime: "bridge"`, Plugin Protocol v1). Operators can add custom delivery channels (such as **ntfy**, Pushover, Slack, Matrix, Gotify, or internal webhooks) without modifying or bloating the core FeedCourier binary.

> [!IMPORTANT]
> FeedCourier delivers rendered article content (title, summary, HTML/plain text, and URL) directly to the bridge sidecar you register. Only register bridge services that you operate and trust. FeedCourier enforces HMAC-SHA256 request authentication, strict `host:port` direct routing, non-redirect policy, and hard timeouts.

---

## Security & Network Boundary

- **HMAC-SHA256 Authentication**: Every request from FeedCourier carries `X-FeedCourier-Timestamp` and `X-FeedCourier-Signature: v1=<hex(HMAC-SHA256(secret, "{ts}.{body}"))>` with a `±300s` replay window.
- **Exact `host:port` Isolation**: FeedCourier connects directly to the exact `(scheme, host, port)` you register using a dedicated HTTP client (no global proxy, `redirect::Policy::none()`, `1 MiB` response cap). You do **not** need to add Docker sidecars to `SSRF_ALLOWLIST`, and unregistered private/LAN hosts remain strictly blocked for RSS feeds and media fetches.
- **Encrypted Credentials**: The bridge shared secret (`plugins.encrypted_bridge_secret`) as well as channel account and destination secrets are encrypted at rest with ChaCha20-Poly1305 (`MasterKey`).

---

## Quick Start: Deploying the Reference `ntfy` Channel Plugin

The official **[`ElleryHe/FeedCourier-Plugins`](https://github.com/ElleryHe/FeedCourier-Plugins)** repository includes a zero-dependency Node.js 24 reference channel plugin under `channels/ntfy/`.

### 1. Run the Bridge Sidecar with Docker Compose

Add the `feedcourier-ntfy` sidecar on the same Docker network as `feedcourier`:

```yaml
services:
  feedcourier:
    image: elleryhe/feedcourier:0.3.1
    ports:
      - "8080:8080"
    volumes:
      - feedcourier_data:/data
    stop_grace_period: 30s
    restart: unless-stopped

  feedcourier-ntfy:
    build: https://github.com/ElleryHe/FeedCourier-Plugins.git#main:channels/ntfy
    environment:
      BRIDGE_SECRET: "replace-with-a-long-random-secret"
      PORT: "8787"
    restart: unless-stopped
```

### 2. Register the Bridge in FeedCourier WebUI

1. Open **Plugins** → click **Add Bridge Plugin**.
2. Enter:
   - **Bridge Base URL**: `http://feedcourier-ntfy:8787`
   - **Shared HMAC Secret**: the exact `BRIDGE_SECRET` configured on the sidecar container.
3. Click **Preview Manifest** (`POST /api/v1/plugins/bridges` with `dry_run: true`) to verify HMAC signing, protocol compatibility (`api_version: "1"` and `min_core`), and declared schemas.
4. Click **Confirm & Register**.

### 3. Create Channel Accounts & Destinations

1. Open **Channels** → **New Account**.
2. Select **ntfy Push** (appears automatically alongside Telegram, SMTP, and Notion once enabled).
3. Fill the schema-generated **Account** fields (`server_url`, optional `access_token`) and **Initial Destination** fields (`topic`, optional `priority` and `tags`). FeedCourier validates both locally against the plugin manifest schemas and remotely via `POST /validate`.
4. Click **Test** on the account or destination, then attach the destination to any delivery route!

---

## Lifecycle Operations

- **Refresh Manifest**: After upgrading a bridge sidecar container, click **Refresh Manifest** (`POST /api/v1/plugins/{id}/refresh-manifest`) to pull updated version numbers, schemas, or capabilities without re-entering the secret.
- **Disable / Pause**: Toggling a channel plugin off defers pending outbox tasks cleanly (`status = 'deferred'`) without losing attempts or dead-lettering them.
- **Safe Deletion**: Deleting a bridge plugin still referenced by channels or routes returns **`409 Conflict`** listing all referencing channels and routes, requiring explicit `?cascade=true` confirmation.
