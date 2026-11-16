---
description: "Back up and restore the FeedCourier SQLite WAL database and master.key, configure payload retention cleanup, and set up reverse proxies and monitoring."
---

# Backup & Maintenance

FeedCourier stores all persistent state in a single directory (`/data`), runs periodic cleanup for delivered article bodies, and supports online SQLite backups while the service is running.

---

## Persistent `/data` Directory Layout

When running in Docker (`-v ./data:/data`) or on the host (`DATA_DIR=./data`), FeedCourier maintains the following files inside `DATA_DIR`:

```text
/data/
├── feedcourier.db        # Primary SQLite database file
├── feedcourier.db-wal    # Write-Ahead Log (active during runtime)
├── feedcourier.db-shm    # Shared-memory WAL index
└── master.key            # Auto-generated 32-byte encryption key (0600 permissions, if MASTER_KEY env is unset)
```

::: danger Always Back Up `feedcourier.db` and `master.key` Together
Channel credentials (SMTP passwords, Telegram Bot Tokens, and Notion Integration Secrets) are encrypted using your 32-byte master key.
If you restore `feedcourier.db` on a new server without the matching `master.key` (or `MASTER_KEY` environment variable), saved channel credentials cannot be decrypted and will need to be re-entered.
:::

---

## Master Key Management (`master.key` vs. `MASTER_KEY`)

You can supply the 32-byte encryption key in either of two ways:

### 1. Automatic Key File Generation (Default)
If `MASTER_KEY` is not set in the environment:
- On first startup, FeedCourier generates 32 random bytes, encodes them as 64 hexadecimal characters, and writes `<DATA_DIR>/master.key` with `0600` (`rw-------`) Unix permissions.
- Subsequent startups automatically load `<DATA_DIR>/master.key`.

### 2. Environment Variable (`MASTER_KEY`)
When using external secret managers (such as Docker Secrets, Kubernetes Secrets, or 1Password CLI), pass `MASTER_KEY` via environment variable:

```bash
# Generate a 64-character hex master key
openssl rand -hex 32
```

Both **64-character hexadecimal** and **Base64** (decoding to 32 bytes) are accepted.

---

## Online Backups (`VACUUM INTO`)

Avoid copying `feedcourier.db` directly with `cp` or `rsync` while the process is running, as active changes may still reside in `feedcourier.db-wal`.

Instead, use the built-in online backup (`VACUUM INTO`), which creates a self-contained, consistent `.db` snapshot while feed polling and deliveries continue running.

### Via WebUI
1. Open the WebUI and go to **Settings → Maintenance & Database**.
2. Click **Download Snapshot Backup**.
3. Your browser downloads `feedcourier-backup-YYYYMMDD-HHMMSS.db`.

### Via REST API (`curl`)
You can also download a database snapshot from the command line using an authenticated session cookie:

```bash
# 1. Sign in and save the HttpOnly fc_session cookie
curl -s -c cookies.txt \
  -H "Content-Type: application/json" \
  -d '{"password":"your-admin-password"}' \
  http://127.0.0.1:8080/api/v1/auth/login

# 2. Download a consistent SQLite snapshot
curl -s -b cookies.txt -OJ \
  http://127.0.0.1:8080/api/v1/maintenance/backup
```

### Restoring from a Snapshot
1. Stop the FeedCourier container or process.
2. Remove the existing `feedcourier.db`, `feedcourier.db-wal`, and `feedcourier.db-shm` files in `/data`.
3. Copy your backup file to `/data/feedcourier.db` alongside your original `/data/master.key`.
4. Start FeedCourier.

---

## Automatic Maintenance & Content Cleanup

A background task runs every **1 hour** and performs three maintenance tasks:

1. **Delivered Content Cleanup**: Once an article is older than `RETENTION_HOURS` (default `24` hours) and has no pending or retrying delivery tasks, its cached HTML body, text summary, and media attachment list are cleared. The compact SHA-256 content hash is kept permanently so the article will not be delivered again.
2. **Expired Session Cleanup**: Removes expired WebUI login sessions.
3. **Database Statistics Update**: Runs `PRAGMA optimize` to keep SQLite query planner statistics up to date.

::: tip Manual Content Cleanup
You can trigger content cleanup at any time in the WebUI (**Settings → Run Payload GC Now**) or via `POST /api/v1/maintenance/gc`.
:::

---

## Reverse Proxy & HTTPS Deployment {#reverse-proxy-https-deployment}

When exposing the WebUI or `/mcp` endpoint beyond `localhost`, place FeedCourier behind a reverse proxy with HTTPS such as **Caddy** or **Nginx**.

### Caddy (`Caddyfile`)

```txt
feedcourier.example.com {
    reverse_proxy 127.0.0.1:8080 {
        # Flush Streamable HTTP /mcp responses immediately
        flush_interval -1
    }
}
```

### Nginx

```nginx
server {
    listen 443 ssl http2;
    server_name feedcourier.example.com;

    # TLS certificates ...

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        # Required for Streamable HTTP /mcp responses
        proxy_buffering off;
        proxy_cache off;
        proxy_read_timeout 3600s;
    }
}
```
