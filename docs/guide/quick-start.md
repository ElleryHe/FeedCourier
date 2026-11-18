---
description: "Deploy FeedCourier with Docker Run, Docker Compose, prebuilt Linux and macOS binaries, or from source, and initialize the WebUI administrator account, and enable optional TOTP two-factor authentication."
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
  -e TZ=America/Los_Angeles \
  elleryhe/feedcourier:latest
  # Or pin a specific release version:
  # elleryhe/feedcourier:0.1.6
```

Once the container starts:

1. Open **`http://127.0.0.1:8080`** in your browser.
2. On first launch, the WebUI prompts you to choose an administrator username (default `Courier`) and password (unless you configured `ADMIN_USERNAME` / `ADMIN_PASSWORD` in the environment).
3. FeedCourier generates `/data/master.key` (`0600` file permissions) and initializes `/data/feedcourier.db` in SQLite `WAL` mode.

---

## Option 2: Docker Compose

Create a `docker-compose.yml` file:

```yaml
services:
  feedcourier:
    image: elleryhe/feedcourier:latest # or elleryhe/feedcourier:0.1.6
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
      TZ: America/Los_Angeles # system timezone for route delivery time windows
      # Optional presets (can also be configured in WebUI):
      # ADMIN_USERNAME: "Courier"
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
  "version": "0.1.6",
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
VERSION="0.1.6"
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
3. **Create a Route** — Go to **Routes** and link specific feeds or tags (such as `rust` or `security`) to your target channel, optionally adding keyword or regex filters. Each route also has its own **delivery language** (defaults to your current WebUI language) and optional **delivery time windows** and **weekdays** (evaluated in the system timezone).
4. **(Optional) Connect an MCP Client** — Go to **Settings → Native MCP Server (`/mcp`)** to generate a `readonly` or `admin` Bearer token for Claude Desktop, Cursor, or Windsurf. See [Client Setup & Security](/mcp/setup).

---

## Sign-In & Two-Factor Authentication

FeedCourier has a single administrator account. Sign in with the **username** and **password** you chose on first launch (the username defaults to `Courier`). Password managers can autofill both fields.

To add a second factor, go to **Settings → Account & Security**:

1. Click **Enable 2FA** and confirm your current password.
2. Scan the QR code with any TOTP authenticator app (or type the displayed secret manually) and enter the 6-digit code to activate it.
3. Save the **10 recovery codes** (`XXXX-XXXX-XXXX`) shown once. Each code works a single time in place of an authenticator code.

Afterwards, signing in is a two-step flow: username and password first, then a 6-digit code (submitted automatically on the sixth digit) or a recovery code. You can regenerate recovery codes (which invalidates the old ones) or disable 2FA from the same page. Changing the password with 2FA enabled requires a current code, and revokes all other sessions.

If you lose both your authenticator and your recovery codes, see [Lost Authenticator (`RESET_2FA`)](/guide/operations#lost-authenticator-reset-2fa).

::: info Interface Language vs. Delivery Language
The WebUI language switcher (sidebar bottom-left, or the top bar on mobile) is a browser-only preference. The language used for pushed messages is set per route.
:::
