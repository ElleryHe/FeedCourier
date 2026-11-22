---
description: "Deliver RSS, Atom, and JSON Feed articles via SMTP Email with per-recipient double opt-in verification, RFC 8058 one-click unsubscribe (/u/{token}), PUBLIC_URL reverse-proxy examples, CAN-SPAM footers, and hard bounce protection."
---

# SMTP Email

The SMTP channel delivers each matched feed entry as an individual email with a responsive single-column HTML layout, a plain-text fallback, localized timestamps, per-recipient double opt-in consent tracking, and RFC 8058 one-click unsubscribe headers.

Under FeedCourier's two-layer channel model, you configure your **SMTP Account** once (server, credentials, sender address, optional CAN-SPAM sender identity, pacing, and quotas) and attach one or more **Destinations** (each with its own recipient list and optional `Reply-To`). SMTP uses native TCP/TLS direct connections; global HTTP/SOCKS outbound proxies (`OUTBOUND_PROXY` / `use_proxy`) do not apply to SMTP accounts.

---

## 1. Preparing SMTP Credentials

Most email providers require an **App Password** (rather than your main account password) when sending mail over SMTP:

| Provider | SMTP Host (`host`) | Port (`port`) | Security (`security`) | Notes |
| :--- | :--- | :---: | :---: | :--- |
| **Gmail / Google Workspace** | `smtp.gmail.com` | `587` or `465` | `start_tls` or `tls` | Enable 2-Step Verification in your Google Account and generate a 16-character **App Password**. |
| **Fastmail** | `smtp.fastmail.com` | `465` | `tls` | Create an App Password with SMTP access under **Settings → Privacy & Security**. |
| **Apple iCloud Mail** | `smtp.mail.me.com` | `587` | `start_tls` | Generate an app-specific password at `account.apple.com`; set `from_address` to your iCloud email or alias. |
| **Corporate / Self-Hosted Mail** | Your server hostname | `465` or `587` | `tls` or `start_tls` | Use a dedicated client password; `from_address` must match an authorized sender address. |

---

## 2. WebUI Configuration: Account + Destinations

Go to **Channels → New Channel → SMTP** to configure the account and its first destination (you can add more destinations to the same account at any time).

### SMTP Account Fields

| Field | Required | Default | Description |
| :--- | :---: | :--- | :--- |
| `name` | No | *Auto-filled* | Account display name. If left blank, automatically populated from **`from_address`**. |
| `host` | Yes | — | SMTP server hostname (for example, `smtp.gmail.com` or `smtp.fastmail.com`). |
| `port` | Yes | `587` | SMTP server port (`587` for STARTTLS, `465` for implicit TLS/SMTPS, `25`/`1025` for local relays). |
| `security` | Yes | `start_tls` | Encryption mode:<br>• `start_tls`: Upgrade connection via STARTTLS (standard for port `587`)<br>• `tls`: Implicit TLS / SMTPS (standard for port `465`)<br>• `plain`: Unencrypted SMTP (only for local or internal relays) |
| `username` | No | — | SMTP login username (usually your full email address). |
| `password` | No | — | SMTP password or App Password (encrypted at rest; leave blank when editing to keep the existing password, or send `"password": null` via `PATCH /api/v1/channels/{id}` to explicitly clear an optional password). |
| `from_address` | Yes | — | Sender email address (`Force-From`). Must be authorized by your SMTP account. |
| `sender_identity` | No | — | Optional CAN-SPAM / legal sender identity or contact address rendered in the footer of every email (for example, `Acme Corp · 123 Main St, San Francisco, CA`). |

### SMTP Destination Fields (Add Multiple per Account)

| Field | Required | Default | Description |
| :--- | :---: | :--- | :--- |
| `name` | No | *Auto-filled* | Destination label. If left blank, automatically populated from the **masked first recipient address** in `to_addresses` (e.g. `a***e@example.com`, keeping plaintext emails out of unencrypted metadata columns). |
| `to_addresses` | Yes | — | One or more recipient email addresses for this destination group. Newly added addresses start as `unverified` and require explicit verification before receiving deliveries (see below). |
| `reply_to` | No | — | Optional `Reply-To` email address. |
| `enabled` | No | `true` | Independently enable or pause deliveries to this recipient group without affecting sibling destinations. |

### Account vs. Destination Testing, Referential Delete Guard & Shared Limits

- **Test Account (`POST /api/v1/channels/{id}/test` without `destination_id`)**: Verifies SMTP server connectivity and authentication and sends a self-test email **only to `from_address`** (never emailing destination recipients).
- **Test Destination (`POST /api/v1/destinations/{id}/test`) & Feed Preview Debug Push**: Strictly deliver **only to `verified` recipients** via isolated single-`To:` envelopes with RFC 8058 compliance headers (in your selected language). If a destination has **`0` verified recipients**, both endpoints return an explicit error instead of sending unsolicited mail.
- **Direct TCP/TLS & Shared Rate Limits**: SMTP uses native TCP/TLS direct connections (HTTP/SOCKS outbound proxies / `OUTBOUND_PROXY` do not apply to SMTP, and the WebUI hides the proxy toggle for SMTP accounts). All destinations attached to the same SMTP account share `min_interval_secs`, per-delivery random jitter, `hourly_quota`, `daily_quota`, and temporary throttling cooldowns. For cross-account rate-limit grouping, SMTP `credential_group_key` includes a SHA-256 digest of the password (never storing plaintext), so separate API keys sharing a generic username (such as SendGrid's `apikey` username with different keys) remain isolated. See [Send Rate, Quotas, Windows & Filters](/channels/pacing-and-quotas).
- **Account & Destination Delete Guard (`409 Conflict`)**: Deleting a channel account or destination that is still referenced by one or more routes returns **`409 Conflict`** listing `referencing_routes`; confirm cascading deletion (or pass `?cascade=true` on `DELETE /api/v1/channels/{id}` / `DELETE /api/v1/destinations/{id}`) to remove it along with its referencing routes.

---

## 3. SMTP Compliance & Per-Recipient Delivery (`v0.3.0`)

Starting in **`v0.3.0`**, FeedCourier tracks consent, verification, unsubscribe, and hard-bounce state **per recipient address** in SQLite (`smtp_recipients`, storing only keyed HMAC-SHA256 digests so plaintext emails remain inside encrypted destination configs).

### Per-Recipient Envelopes & Quota Accounting

- **One Email per Verified Recipient**: Outbox deliveries send an individual SMTP message to each **`verified`** recipient (`To:` contains only a single address) with a unique deterministic `Message-ID` (`<fc-{fp[..24]}-r{route_id}-{hmac[..8]}@{from_domain}>`), individualized `/u/{token}` unsubscribe links, and a **1-second** delay between recipients (renewing the task lease automatically).
- **Crash-Safe Resume**: Each accepted recipient HMAC is checkpointed in `delivery_tasks.checkpoint_json` (`sent_recipients`), so retrying after a transient `4xx` error never double-sends to recipients who already received the article.

::: warning Quota Semantics vs. Provider Message Limits
One delivered article task still counts as **`1`** entry in FeedCourier's rolling hourly/daily quota ledger (`delivery_quota_ledger`), while the **actual number of SMTP messages transmitted equals the number of verified recipients (`N ×`)**. When using providers with strict per-message quotas or billing (Gmail, SES, Postmark, SendGrid), size your `hourly_quota` and `daily_quota` on the SMTP account with recipient count in mind.
:::

### Manual-Trigger Double Opt-In Verification

1. **No Unsolicited Auto-Sending**: Adding a new email address to `to_addresses` creates an **`unverified`** recipient row and does **not** automatically send any verification email.
2. **Administrator-Triggered Verification**: Expand the destination in the WebUI (showing a **`verified / total`** badge) and click **Send Verification** (`POST /api/v1/destinations/{id}/recipients/{rid}/send-verification`). FeedCourier sends the verification email immediately (bypassing the outbox queue and channel quotas):
   - Includes a **6-digit verification code** (stored exclusively as a SHA-256 hash, **30-minute TTL**, maximum **5 wrong attempts**).
   - When **`PUBLIC_URL`** is configured, also includes a one-click confirmation page link **`/v/{token}`**.
   - Built-in abuse throttling: minimum **60-second** resend cooldown and maximum **5 verification emails per hour** per recipient.
3. **Two Ways to Complete Verification**:
   - **Manual Code Entry**: The administrator enters the 6-digit code in the WebUI (`POST /api/v1/destinations/{id}/recipients/{rid}/verify` with `{ "code": "123456" }`), recording `consent_method = "code"`.
   - **Confirmation Link (`/v/{token}`)**: The recipient opens the link and clicks **Confirm Subscription** (`GET /v/{token}` is strictly read-only so corporate mail scanners like Safe Links or Proofpoint never auto-confirm on prefetch; `POST /v/{token}` verifies the recipient and records `consent_method = "link"`).
4. **Automatic Wakeup**: As soon as a recipient becomes `verified`, any tasks deferred waiting for verification are immediately released and queued for delivery.

### `PUBLIC_URL` & RFC 8058 One-Click Unsubscribe (`/u/{token}`)

Configure **`PUBLIC_URL`** (via environment variable or **Settings → Public Base URL**) to an absolute `https://` origin (no query string or fragment; plain `http://` triggers a warning):

- **When `PUBLIC_URL` Is Configured**:
  - Every delivered email includes RFC 8058 headers:
    ```http
    List-Id: <dest-5.yourdomain.com>
    Auto-Submitted: auto-generated
    List-Unsubscribe: <https://feeds.example.com/u/TOKEN>, <mailto:courier@yourdomain.com?subject=unsubscribe>
    List-Unsubscribe-Post: List-Unsubscribe=One-Click
    ```
  - Mail clients (Gmail, Apple Mail, Yahoo) display a native **Unsubscribe** button that issues an unauthenticated, idempotent `POST /u/{token}` with `List-Unsubscribe=One-Click` (`revoked_method = "one_click"`).
  - Clicking the footer **Unsubscribe** link loads `GET /u/{token}` — a strictly read-only localized confirmation page showing only a masked email (`a***e@example.com`). Clicking **Confirm Unsubscribe** submits `POST /u/{token}` (`revoked_method = "link"`).
- **When `PUBLIC_URL` Is Unset**:
  - FeedCourier shows an amber banner on SMTP channels, emits only `<mailto:from_address?subject=unsubscribe>` in `List-Unsubscribe`, prints a *"Reply to this email to unsubscribe"* footer notice, and uses manual 6-digit codes for double opt-in verification.
- **CAN-SPAM Footer**: Every delivered email includes the subscription reason (*"You are receiving this email because you are subscribed to …"*), the unsubscribe control, and the optional `sender_identity` text configured on the SMTP account.

### Reverse Proxy: Exposing Only `/u/` and `/v/` Publicly

You do **not** need to expose FeedCourier's WebUI, `/api/v1/*`, or `/mcp` to the public internet to support RFC 8058 one-click unsubscribe and verification links. Put Nginx or Caddy in front of `PUBLIC_URL` and proxy **only `/u/` and `/v/`** while keeping management interfaces private on LAN/VPN.

::: warning Configure `TRUST_PROXY=true` Behind a Reverse Proxy
By default (`TRUST_PROXY=false`), FeedCourier rate-limits `/u/` and `/v/` using only the direct TCP peer address (`ConnectInfo<SocketAddr>`) and ignores client-supplied proxy headers so callers cannot bypass limits by spoofing `X-Forwarded-For`. When deploying behind Nginx or Caddy, set **`TRUST_PROXY=true`** in FeedCourier's environment and ensure your proxy overwrites `X-Real-IP` (`$remote_addr`) or appends the client IP to `X-Forwarded-For`. With `TRUST_PROXY=true`, FeedCourier reads `X-Real-IP` or the **rightmost** proxy-appended `X-Forwarded-For` entry—resisting client-spoofed leftmost entries. Confirmation pages use relative `action=""` form targets so subpath `PUBLIC_URL` mounts work cleanly, and bare `/u/` or `/v/` paths return a public `404` rather than the WebUI SPA.
:::

#### Nginx Example

```nginx
# Also set TRUST_PROXY=true on the FeedCourier container/process
server {
    listen 443 ssl http2;
    server_name feeds.example.com;

    # TLS certificates omitted for brevity

    # Public SMTP compliance endpoints ONLY (RFC 8058 unsubscribe + double opt-in)
    location ~ ^/(u|v)/ {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }

    # Keep WebUI, /api/v1/*, and /mcp private (do not expose publicly)
    location / {
        return 404;
    }
}
```

#### Caddy Example

```text
# Also set TRUST_PROXY=true on the FeedCourier container/process
feeds.example.com {
    @public_smtp path /u/* /v/*
    handle @public_smtp {
        reverse_proxy 127.0.0.1:8080 {
            header_up X-Real-IP {remote_host}
        }
    }

    handle {
        respond 404
    }
}
```

### Hard Bounce Handling, Deferral, Auto-Pause & Auto-Resume

- **Recipient-Level 5xx Hard Bounces vs. Channel/Content 5xx**:
  - **Recipient Hard Bounce**: Only when the SMTP server returns a recipient-level permanent rejection—enhanced status codes `5.1.1`, `5.1.2`, `5.1.3`, `5.1.6`, `5.1.10`, or recipient-unknown `550`/`551`/`553` responses—does FeedCourier mark that specific recipient **`bounced`** (`revoked_method = "hard_bounce"`, with any email addresses masked in `bounce_diagnostic` and `diagnostic_log`) and continue delivering to remaining verified recipients.
    - If other recipients in the same destination succeeded, the task finishes as **`delivered`** with a redacted warning note in `diagnostic_log`.
    - If **all** verified recipients hard-bounce, the task transitions to **`dead_letter`**.
  - **Account / Quota / Auth / Content 5xx (`530`, `535`, `552`, `554`, `5.4.5`, `5.7.*`)**: Errors caused by authentication failures, message size/quota limits, or SPF/DKIM/DMARC/content policy immediately abort delivery as channel-level errors (preserving already-delivered recipients in `checkpoint_json.sent_recipients`) **without** marking verified subscribers as `bounced`.
  - Temporary `4xx` errors continue using normal exponential backoff retries and skip already-delivered recipients via checkpointing.
- **No Deliverable Recipients & `auto_paused` Automatic Recovery**:
  - If `0` recipients are `verified` while at least one is `unverified` or `pending`, tasks enter **`deferred`** (*"Awaiting SMTP recipient verification"*, re-checked in 1 hour, and woken immediately when a recipient verifies).
  - If **all** recipients are `unsubscribed` or `bounced`, FeedCourier automatically pauses the destination with **`enabled = false, auto_paused = true`** and defers remaining tasks so it stops attempting unwanted deliveries.
  - Once any recipient later completes re-verification (via 6-digit code or `/v/{token}`), a destination with **`auto_paused = true`** **automatically resumes** (`enabled = true, auto_paused = false`) and wakes deferred tasks. Manually toggling `enabled` clears `auto_paused = false`, so manually disabled destinations remain paused until an administrator re-enables them.
- **Re-Enabling Revoked/Bounced Addresses**: Administrators can click **Reactivate** (`POST .../reactivate`) on an `unsubscribed` or `bounced` recipient; its status resets to **`unverified`** and it must complete verification again before receiving mail. Reaching 5 failed verification code attempts atomically clears the active code so any prior `/v/{token}` link immediately returns `410 Gone`.
- **Upgrading from `v0.2.x` (Legacy Backfill)**: On first startup after upgrading to `v0.3.0`, whenever an existing database has migration `0002` applied and any business table (`feeds`, `channels`, or `channel_destinations`) is non-empty, `Database::connect` captures a pre-DDL `VACUUM INTO` snapshot at `<DATA_DIR>/pre-v0.3.0-backup.db` before running migration `0003`. Then `backfill_legacy_smtp_recipients` marks every existing SMTP recipient as `status = 'verified', consent_method = 'legacy'` (guarded idempotently by `system_settings.smtp_recipient_model_version = 1`) so existing personal setups keep delivering without disruption.

---

## 4. Sender Alignment (`Force-From`) & Headers

Modern email providers enforce **SPF, DKIM, and DMARC** policies. FeedCourier always uses **sender alignment (`Force-From`)**:
- **Fixed Sender Address**: The email address in the `From:` header is always your configured `from_address` (e.g., `courier@yourdomain.com`), ensuring it matches your authenticated SMTP account.
- **Feed Name as Display Name**: The sender display name is set to `"<Feed Title> via FeedCourier"`, paired with standardized headers for inbox rules and RFC 8058 compliance:

```http
From: "Rust Blog via FeedCourier" <courier@yourdomain.com>
To: reader@example.com
Subject: Announcing Rust 1.94.0
List-Id: <dest-5.yourdomain.com>
Auto-Submitted: auto-generated
List-Unsubscribe: <https://feeds.example.com/u/TOKEN>, <mailto:courier@yourdomain.com?subject=unsubscribe>
List-Unsubscribe-Post: List-Unsubscribe=One-Click
```

---

## 5. What Delivered Emails Look Like

- **Responsive Single-Column HTML + Plain Text**: Each email includes a `640px` single-column HTML card alongside a 78-column plain-text version (`multipart/alternative`), showing the feed title, article title, author, localized publication date (using the **route timezone**, e.g. `2026-11-08 11:00 (UTC+08:00)`), a link to the original article, and the compliance footer.
- **Responsive Images, Figures, Tables & Code Blocks**: Upstream feed images carrying large fixed pixel widths (such as `<img width="1200" height="630">`) are automatically rewritten to fit the `592px` inner card width (`width="592"` for Outlook compatibility while preserving explicitly narrower widths, stripping fixed `height`, and adding inline `max-width:100%;height:auto`). For percentage widths, fractional values remain in inline CSS (e.g., `style="width:33.3%;max-width:100%;height:auto;..."`) while the HTML `width` attribute carries the integer pixel equivalent scaled to `592px` (e.g., `width="197"`), `0%` stays `0`, and positive sub-1% widths clamp to `1%` so images never vanish. Attributes use semicolon-aware HTML5 entity decoding, alongside clean `<figure>`/`<figcaption>` spacing and `max-width:100%` wrapping/scrolling on wide `<table>` and `<pre>` blocks.
- **Protection Against Email Clipping**: Very long articles are automatically shortened before exceeding email client size limits (such as Gmail's 102 KB clipping threshold), with a **"Continue reading original article →"** link at the bottom.
- **Easy Inbox Filtering (`List-Id`)**: Every email includes a standard `List-Id` header so you can filter automated digests cleanly in Gmail, Fastmail, Apple Mail, or Thunderbird.

::: tip Recommended Send Rate for Personal Mailboxes
When sending through personal Gmail, Fastmail, or iCloud accounts, keeping `min_interval_secs` between `15` and `30` seconds and setting `hourly_quota` conservatively relative to your recipient count helps stay well within provider sending limits across all destinations.
:::

---

## Technical Details

For details on MIME assembly, HTML attribute rewriting, HMAC token construction, and SMTP error classification, see [Channel Protocols & Delivery](/reference/channel-protocols) and [Security & Isolation](/reference/security).
