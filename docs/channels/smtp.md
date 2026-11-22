---
description: "Deliver RSS, Atom, and JSON Feed articles via SMTP Email using one SMTP Account with multiple recipient Destinations, responsive Outlook-safe HTML formatting, localized timestamps, and List-ID headers."
---

# SMTP Email

The SMTP channel delivers each matched feed entry as an individual email with a responsive single-column HTML layout, a plain-text fallback, localized timestamps, and standard mailing-list headers for inbox filtering.

Under FeedCourier's two-layer channel model, you configure your **SMTP Account** once (server, credentials, sender address, proxy, pacing, and quotas) and attach one or more **Destinations** (each with its own recipient list and optional `Reply-To`).

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
| `password` | No | — | SMTP password or App Password (encrypted at rest; leave blank when editing to keep the existing password). |
| `from_address` | Yes | — | Sender email address (`Force-From`). Must be authorized by your SMTP account. |

### SMTP Destination Fields (Add Multiple per Account)

| Field | Required | Default | Description |
| :--- | :---: | :--- | :--- |
| `name` | No | *Auto-filled* | Destination label. If left blank, automatically populated from the **first recipient address** in `to_addresses`. |
| `to_addresses` | Yes | — | One or more recipient email addresses for this destination group. |
| `reply_to` | No | — | Optional `Reply-To` email address. |
| `enabled` | No | `true` | Independently enable or pause deliveries to this recipient group without affecting sibling destinations. |

### Account vs. Destination Testing & Shared Limits

- **Test Account**: Verifies SMTP server connectivity and authentication (`EHLO`/`AUTH`) without sending an email.
- **Test Destination**: Sends a live verification email (in your current WebUI language) to that destination's `to_addresses`.
- **Shared Rate Limits & Quotas**: All destinations attached to the same SMTP account share `min_interval_secs`, per-delivery random jitter, `hourly_quota`, `daily_quota`, `use_proxy`, and temporary throttling cooldowns. See [Send Rate, Quotas, Windows & Filters](/channels/pacing-and-quotas).

---

## 3. Sender Alignment (`Force-From`) & Subject Format

Modern email providers enforce **SPF, DKIM, and DMARC** policies. If an automated service tries to spoof the original blog author's domain in the `From:` address, the email will be rejected or routed to spam.

FeedCourier always uses **sender alignment (`Force-From`)**:
- **Fixed Sender Address**: The email address in the `From:` header is always your configured `from_address` (e.g., `courier@yourdomain.com`), ensuring it matches your authenticated SMTP account.
- **Feed Name as Display Name & Subject Prefix**: The sender display name is set to the feed title (e.g., `"Rust Blog via FeedCourier"`), and the email subject is prefixed with `[<Feed Title>]`:

```http
From: "Rust Blog via FeedCourier" <courier@yourdomain.com>
To: reader@example.com
Subject: [Rust Blog] Announcing Rust 1.94.0
List-Id: "Rust Blog" <feed-12.feedcourier.yourdomain.com>
Auto-Submitted: auto-generated
```

---

## 4. What Delivered Emails Look Like

- **Responsive Single-Column HTML + Plain Text**: Each email includes a `640px` single-column HTML card alongside a 78-column plain-text version (`multipart/alternative`), showing the feed title, article title, author, localized publication date (using the **route timezone**, e.g. `2026-11-08 11:00 (UTC+08:00)`), and a link to the original article.
- **Responsive Images, Figures, Tables & Code Blocks**: Upstream feed images carrying large fixed attributes (such as `<img width="1200" height="630">`) are automatically rewritten to fit the `592px` inner card width (`width="592"` for Outlook compatibility while preserving explicitly narrower images, fixed `height` stripped, and inline `max-width:100%;height:auto`), paired with clean `<figure>`/`<figcaption>` spacing, `max-width:100%` wrapping/scrolling on wide `<table>` and `<pre>` blocks, and `<head>` CSS fallbacks for Gmail, Apple Mail, and Outlook.com.
- **Protection Against Email Clipping**: Very long articles are automatically shortened before exceeding email client size limits (such as Gmail's 102 KB clipping threshold), with a **"Continue reading original article →"** link at the bottom.
- **Easy Inbox Filtering (`List-Id`)**: Every email includes a standard `List-Id` header per feed, so you can create inbox rules or filters in Gmail, Fastmail, Apple Mail, or Thunderbird to sort specific feeds into folders automatically.

::: tip Recommended Send Rate for Personal Mailboxes
When sending through personal Gmail, Fastmail, or iCloud accounts, keeping `min_interval_secs` between `15` and `30` seconds and setting `hourly_quota` to `60`–`100` on the SMTP Account helps stay well within provider sending limits across all recipient destinations.
:::

---

## Technical Details

For details on MIME assembly, HTML attribute rewriting, byte budgets, deterministic `Message-ID` deduplication, and SMTP error classification, see [Channel Protocols & Delivery](/reference/channel-protocols).
