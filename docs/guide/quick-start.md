# Quick Start

FeedCourier ships as a self-contained single binary and a multi-architecture Docker image (`linux/amd64`, `linux/arm64`) with the React 19 WebUI and SQLite migrations embedded inside.

---

## Option 1: Docker Run (Fastest)

Multi-arch images are published to **[Docker Hub (`elleryhe/feedcourier`)](https://hub.docker.com/r/elleryhe/feedcourier)** on every tagged release:

```bash
docker run -d \
  --name feedcourier \
  --restart unless-stopped \
  -p 8080:8080 \
  -v "$(pwd)/data:/data" \
  elleryhe/feedcourier:latest
  # Or pin an exact semantic version:
  # elleryhe/feedcourier:0.1.0
```

Once the container starts:

1. Open **`http://127.0.0.1:8080`** in your browser.
2. On first launch, the WebUI prompts you to set your administrator password (unless you pre-seeded `ADMIN_PASSWORD`).
3. FeedCourier automatically generates `/data/master.key` (`0600` permissions) and initializes `/data/feedcourier.db` in SQLite `WAL` mode.

---

## Option 2: Docker Compose (Recommended for Production)

Create a `docker-compose.yml` file:

```yaml
services:
  feedcourier:
    image: elleryhe/feedcourier:latest # or elleryhe/feedcourier:0.1.0
    container_name: feedcourier
    restart: unless-stopped
    ports:
      - "8080:8080"
    volumes:
      - ./data:/data
    environment:
      DATA_DIR: /data
      LISTEN_ADDR: 0.0.0.0:8080
      RETENTION_HOURS: "24"
      # Optional presets (can also be configured in WebUI):
      # ADMIN_PASSWORD: "change-me-strong-passphrase"
      # MASTER_KEY: "<64-char-hex-or-base64-32-byte-key>"
      # OUTBOUND_PROXY: "socks5h://127.0.0.1:1080"
      # SSRF_ALLOWLIST: "10.0.0.5/32,rsshub.internal"
```

Start the service in the background:

```bash
docker compose up -d
```

Verify the gateway health check:

```bash
curl -s http://127.0.0.1:8080/api/v1/health
```

```json
{
  "status": "ok",
  "app": "FeedCourier",
  "version": "0.1.0",
  "license": "AGPL-3.0-only",
  "repo_url": "https://github.com/ElleryHe/FeedCourier"
}
```

---

## Option 3: Prebuilt Binaries

Standalone release archives with `SHA256SUMS.txt` are published on **[GitHub Releases](https://github.com/ElleryHe/FeedCourier/releases)** for four targets:

| Platform | Target Triple | Archive Name |
| :--- | :--- | :--- |
| **Linux (x86_64)** | `x86_64-unknown-linux-gnu` | `feedcourier-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| **Linux (ARM64)** | `aarch64-unknown-linux-gnu` | `feedcourier-<version>-aarch64-unknown-linux-gnu.tar.gz` |
| **macOS (Apple Silicon)** | `aarch64-apple-darwin` | `feedcourier-<version>-aarch64-apple-darwin.tar.gz` |
| **macOS (Intel)** | `x86_64-apple-darwin` | `feedcourier-<version>-x86_64-apple-darwin.tar.gz` |

Download, verify, and run:

```bash
# Example for Linux x86_64 (replace VERSION as appropriate)
VERSION="0.1.0"
TARGET="x86_64-unknown-linux-gnu"

curl -LO "https://github.com/ElleryHe/FeedCourier/releases/download/v${VERSION}/feedcourier-${VERSION}-${TARGET}.tar.gz"
curl -LO "https://github.com/ElleryHe/FeedCourier/releases/download/v${VERSION}/SHA256SUMS.txt"

# Verify checksum
grep "feedcourier-${VERSION}-${TARGET}.tar.gz" SHA256SUMS.txt | sha256sum -c -

# Extract and launch
tar -xzf "feedcourier-${VERSION}-${TARGET}.tar.gz"
mkdir -p ./data
DATA_DIR=./data LISTEN_ADDR=127.0.0.1:8080 ./feedcourier
```

---

## Option 4: Build from Source

### Prerequisites

- **Rust 1.94+** (`2024` Edition)
- **Node.js 24+** and **`pnpm` 10+** (via `corepack enable`)
- **`cmake`** and a C compiler (required by `aws-lc-rs` for `rustls` TLS cryptography)

### Build Steps

```bash
# 1. Clone the repository
git clone https://github.com/ElleryHe/FeedCourier.git
cd FeedCourier

# 2. Build the embedded React 19 WebUI assets into web/dist
corepack enable
pnpm --dir web install --frozen-lockfile
pnpm --dir web build

# 3. Compile the single static-asset release binary
#    (rust-embed bundles web/dist directly into the executable)
cargo build --release --locked

# 4. Run the gateway
DATA_DIR=./data LISTEN_ADDR=127.0.0.1:8080 ./target/release/feedcourier
```

::: info Building a Local Container Image
You can also build the multi-stage container image locally using the repository `Dockerfile`:
```bash
docker build -t feedcourier:local .
```
:::

---

## First-Time Onboarding Checklist

After signing into the WebUI at `http://127.0.0.1:8080`:

1. **Create a Trusted Delivery Channel** — Go to **Channels** and configure [SMTP](/channels/smtp), [Telegram](/channels/telegram), or [Notion](/channels/notion). Click **Test** to send an immediate verification message or page.
2. **Add Feeds or Import OPML** — Go to **Feeds** to add an RSS/Atom/JSON Feed URL (with **Silent Init** enabled by default so historical articles don't flood your channel), or import an OPML 2.0 file.
3. **Bind a Route** — Go to **Routes** and link specific feeds or tags (e.g., `rust`, `security`) to your target channel, optionally configuring keyword/regex filters and Telegram presentation modes.
4. **(Optional) Connect an AI Assistant** — Go to **Settings → Native MCP Server (`/mcp`)** to issue a `readonly` or `admin` Bearer token for Claude Desktop, Cursor, or Windsurf. See [MCP Client Setup](/mcp/setup).
