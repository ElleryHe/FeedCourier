---
description: "Deploy FeedCourier with Docker Run, Docker Compose, prebuilt Linux and macOS binaries, or from source, and initialize the WebUI administrator password."
---

# Quick Start

FeedCourier runs as a standalone binary or a multi-architecture Docker image (`linux/amd64`, `linux/arm64`) with the web console and SQLite database migrations bundled inside.

---

## Option 1: Docker Run

Multi-architecture images are published to **[Docker Hub (`elleryhe/feedcourier`)](https://hub.docker.com/r/elleryhe/feedcourier)**:

```bash
docker run -d \
  --name feedcourier \
  --restart unless-stopped \
  -p 8080:8080 \
  -v "$(pwd)/data:/data" \
  elleryhe/feedcourier:latest
  # Or pin a specific release version:
  # elleryhe/feedcourier:0.1.4
```

Once the container starts:

1. Open **`http://127.0.0.1:8080`** in your browser.
2. On first launch, the WebUI prompts you to set an administrator password (unless you configured `ADMIN_PASSWORD` in the environment).
3. FeedCourier generates `/data/master.key` (`0600` file permissions) and initializes `/data/feedcourier.db` in SQLite `WAL` mode.

---

## Option 2: Docker Compose

Create a `docker-compose.yml` file:

```yaml
services:
  feedcourier:
    image: elleryhe/feedcourier:latest # or elleryhe/feedcourier:0.1.4
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

Verify the health endpoint:

```bash
curl -s http://127.0.0.1:8080/api/v1/health
```

```json
{
  "status": "ok",
  "app": "FeedCourier",
  "version": "0.1.4",
  "license": "AGPL-3.0",
  "repo_url": "https://github.com/ElleryHe/FeedCourier"
}
```

---

## Option 3: Prebuilt Binaries

Release archives and `SHA256SUMS.txt` are published on **[GitHub Releases](https://github.com/ElleryHe/FeedCourier/releases)** for four targets:

| Platform | Target Triple | Archive Name |
| :--- | :--- | :--- |
| **Linux (x86_64)** | `x86_64-unknown-linux-gnu` | `feedcourier-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| **Linux (ARM64)** | `aarch64-unknown-linux-gnu` | `feedcourier-<version>-aarch64-unknown-linux-gnu.tar.gz` |
| **macOS (Apple Silicon)** | `aarch64-apple-darwin` | `feedcourier-<version>-aarch64-apple-darwin.tar.gz` |
| **macOS (Intel)** | `x86_64-apple-darwin` | `feedcourier-<version>-x86_64-apple-darwin.tar.gz` |

Download, verify, and run:

```bash
# Example for Linux x86_64 (replace VERSION as appropriate)
VERSION="0.1.4"
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
- **`cmake`** and a C compiler (used by `aws-lc-rs` for `rustls` TLS)

### Build Steps

```bash
# 1. Clone the repository
git clone https://github.com/ElleryHe/FeedCourier.git
cd FeedCourier

# 2. Build the WebUI assets into web/dist
corepack enable
pnpm --dir web install --frozen-lockfile
pnpm --dir web build

# 3. Compile the release binary (bundles web/dist into the executable)
cargo build --release --locked

# 4. Run the service
DATA_DIR=./data LISTEN_ADDR=127.0.0.1:8080 ./target/release/feedcourier
```

::: info Building a Local Container Image
You can also build a container image locally using the repository `Dockerfile`:
```bash
docker build -t feedcourier:local .
```
:::

---

## First-Time Setup Steps

After signing in to the WebUI at `http://127.0.0.1:8080`:

1. **Create a Delivery Channel** — Go to **Channels** and configure [SMTP Email](/channels/smtp), [Telegram](/channels/telegram), or [Notion](/channels/notion). Click **Test** to send a verification message or page.
2. **Add Feeds or Import OPML** — Go to **Feeds** to add an RSS, Atom, or JSON Feed URL (with **Silent Init** enabled by default so existing historical articles are not sent all at once), or import an OPML 2.0 file.
3. **Create a Route** — Go to **Routes** and link specific feeds or tags (such as `rust` or `security`) to your target channel, optionally adding keyword or regex filters.
4. **(Optional) Connect an MCP Client** — Go to **Settings → Native MCP Server (`/mcp`)** to generate a `readonly` or `admin` Bearer token for Claude Desktop, Cursor, or Windsurf. See [Client Setup & Security](/mcp/setup).
