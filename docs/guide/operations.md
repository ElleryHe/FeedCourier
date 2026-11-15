# Operations, Security Keys & Backups

FeedCourier is engineered for **zero-maintenance longevity**: a single persistent directory (`/data`), automatic payload garbage collection, and online atomic snapshots.

---

## Persistent `/data` Directory Layout

When running in Docker (`-v ./data:/data`) or directly on the host (`DATA_DIR=./data`), FeedCourier maintains the following files inside `DATA_DIR`:

```text
/data/
├── feedcourier.db        # Primary SQLite database file
├── feedcourier.db-wal    # Write-Ahead Log (active during runtime)
├── feedcourier.db-shm    # Shared-memory WAL index
└── master.key            # Auto-generated 32-byte AEAD key (0600 permissions, if MASTER_KEY env is unset)
```

::: danger Always Back Up `feedcourier.db` and `master.key` Together
All channel credentials (`channels.encrypted_config`—including SMTP passwords, Telegram Bot Tokens, and Notion Integration Secrets) are encrypted at rest with **ChaCha20-Poly1305** using your 32-byte master key.
If you restore `feedcourier.db` on a new server without the corresponding `master.key` (or `MASTER_KEY` environment variable), encrypted channels cannot be decrypted and must be re-entered.
:::

---

## Master Key Management (`master.key` vs. `MASTER_KEY`)

FeedCourier supports two ways to supply the 32-byte ChaCha20-Poly1305 key (`src/crypto.rs`):

### 1. Automatic Key File Generation (Default)
If `MASTER_KEY` is not set in the environment:
- On first boot, FeedCourier generates 32 cryptographically random bytes, encodes them as 64 hex characters, and writes `<DATA_DIR>/master.key` atomically with `0600` (`rw-------`) Unix permissions.
- Subsequent startups automatically load `<DATA_DIR>/master.key`.

### 2. Explicit Environment Variable (`MASTER_KEY`)
For stateless secret managers (e.g., Docker Swarm Secrets, Kubernetes Secrets, 1Password CLI, or `sops`), pass `MASTER_KEY` directly:

```bash
# Generate a 64-character hex master key
openssl rand -hex 32
```

Both **64-character hexadecimal** and **standard/URL-safe Base64** (decoding to 32 bytes) are accepted.

---

## Zero-Downtime Online Hot Backups (`VACUUM INTO`)

Never copy `feedcourier.db` directly with `cp` or `rsync` while the gateway is actively writing to `feedcourier.db-wal` unless you also copy `-wal` and `-shm` atomically.

Instead, FeedCourier provides a built-in **zero-downtime online backup** powered by SQLite's `VACUUM INTO` command, which produces a standalone, defragmented, transactionally consistent `.db` snapshot while feed polling and outbox deliveries continue uninterrupted.

### Via WebUI
1. Open the FeedCourier WebUI and navigate to **Settings → Maintenance & Database**.
2. Click **Download Snapshot Backup**.
3. Your browser downloads `feedcourier-backup-YYYYMMDD-HHMMSS.db`.

### Via REST API (`curl`)
You can trigger and download a consistent SQLite snapshot using an authenticated session cookie:

```bash
# 1. Authenticate and save the HttpOnly fc_session cookie
curl -s -c cookies.txt \
  -H "Content-Type: application/json" \
  -d '{"password":"your-admin-password"}' \
  http://127.0.0.1:8080/api/v1/auth/login

# 2. Stream a freshly vacuumed SQLite snapshot
curl -s -b cookies.txt -OJ \
  http://127.0.0.1:8080/api/v1/maintenance/backup
```

### Restoring from a Snapshot
1. Stop the FeedCourier container or process.
2. Remove the old `feedcourier.db`, `feedcourier.db-wal`, and `feedcourier.db-shm` files in `/data`.
3. Place your backup file at `/data/feedcourier.db` alongside your original `/data/master.key`.
4. Start FeedCourier.

---

## Automatic Maintenance & Payload Garbage Collection

A background maintenance task runs automatically every **1 hour** (`DEFAULT_MAINTENANCE_TICK = 3600s`) and performs three operations:

1. **Zero-Refcount Payload GC**: Any row in `entries` whose `discovered_at` timestamp is older than `RETENTION_HOURS` (default `24` hours) **and** for which `NOT EXISTS` any non-terminal row in `delivery_tasks` (`queued`, `delivering`, `retrying`, `deferred`) has its large columns (`content_html`, `summary_text`, `first_sentence`, `media_json`) set to `NULL` and `payload_purged_at` recorded. The 64-character `fingerprint_sha256` row remains permanently so the article is never re-delivered.
2. **Expired Session Cleanup**: Purges expired WebUI login rows from `sessions`.
3. **Query Planner Optimization**: Runs `PRAGMA optimize` to keep SQLite B-tree statistics fresh.

::: tip Manual Payload GC Trigger
You can also trigger payload garbage collection immediately from the WebUI (**Settings → Run Payload GC Now**) or via `POST /api/v1/maintenance/gc`.
:::

---

## Reverse Proxy & HTTPS Deployment

When exposing the WebUI or `/mcp` endpoint outside `localhost`, always place FeedCourier behind a TLS-terminating reverse proxy such as **Caddy** or **Nginx**.

### Caddy (`Caddyfile`)

```txt
feedcourier.example.com {
    reverse_proxy 127.0.0.1:8080 {
        # Flush SSE / Streamable HTTP MCP responses immediately
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

        # Required for Streamable HTTP /mcp SSE streams
        proxy_buffering off;
        proxy_cache off;
        proxy_read_timeout 3600s;
    }
}
```
