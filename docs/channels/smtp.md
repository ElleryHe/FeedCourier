---
description: "Deliver RSS, Atom, and JSON Feed articles via SMTP Email with STARTTLS or implicit TLS, HTML and plain-text formatting, and List-ID inbox filter headers."
---

# SMTP Email

The SMTP channel delivers each matched feed entry as an individual email with a clean single-column HTML layout, a plain-text fallback, and standard mailing-list headers for inbox filtering.

---

## Preparing SMTP Credentials

Most email providers require an **App Password** (rather than your main account password) when sending mail over SMTP:

| Provider | SMTP Host (`host`) | Port (`port`) | Security (`security`) | Notes |
| :--- | :--- | :---: | :---: | :--- |
| **Gmail / Google Workspace** | `smtp.gmail.com` | `587` or `465` | `start_tls` or `tls` | Enable 2-Step Verification in your Google Account and generate a 16-character **App Password**. |
| **Fastmail** | `smtp.fastmail.com` | `465` | `tls` | Create an App Password with SMTP access under **Settings → Privacy & Security**. |
| **Apple iCloud Mail** | `smtp.mail.me.com` | `587` | `start_tls` | Generate an app-specific password at `account.apple.com`; set `from_address` to your iCloud email or alias. |
| **Corporate / Self-Hosted Mail** | Your server hostname | `465` or `587` | `tls` or `start_tls` | Use a dedicated client password; `from_address` must match an authorized sender address. |

---

## WebUI Configuration Fields

In the WebUI, go to **Channels → New Channel → SMTP** and fill in the following fields:

| Field | Required | Default | Description |
| :--- | :---: | :--- | :--- |
| `host` | Yes | — | SMTP server hostname (for example, `smtp.gmail.com` or `smtp.fastmail.com`). |
| `port` | Yes | `587` | SMTP server port (`587` for STARTTLS, `465` for implicit TLS/SMTPS, `25`/`1025` for local relays). |
| `security` | Yes | `start_tls` | Encryption mode:<br>• `start_tls`: Upgrade connection via STARTTLS (standard for port `587`)<br>• `tls`: Implicit TLS / SMTPS (standard for port `465`)<br>• `plain`: Unencrypted SMTP (only for local or internal relays) |
| `username` | No | — | SMTP login username (usually your full email address). |
| `password` | No | — | SMTP password or App Password (encrypted at rest; leave blank when editing to keep the existing password). |
| `from_address` | Yes | — | Sender email address (`Force-From`). Must be authorized by your SMTP account. |
| `to_addresses` | Yes | — | One or more recipient email addresses. |
| `reply_to` | No | — | Optional `Reply-To` email address. |

After saving, click **Test** on the channel card to send a test email and verify connectivity.

---

## Sender Alignment (`Force-From`) & Subject Format

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

## What Delivered Emails Look Like

- **Single-Column HTML + Plain Text**: Each email includes a clean, mobile-friendly single-column HTML view alongside a 78-column plain-text version (`multipart/alternative`), with the feed title, article title, author, publication date, and a link to the original article.
- **Protection Against Email Clipping**: Very long articles are automatically shortened before exceeding email client size limits (such as Gmail's 102 KB clipping threshold), with a **"Continue reading original article →"** link at the bottom.
- **Easy Inbox Filtering (`List-Id`)**: Every email includes a standard `List-Id` header per feed, so you can create inbox rules or filters in Gmail, Fastmail, Apple Mail, or Thunderbird to sort specific feeds into folders automatically.

::: tip Recommended Send Rate for Personal Mailboxes
When sending through personal Gmail, Fastmail, or iCloud accounts, keeping `min_interval_secs` between `15` and `30` seconds and setting `hourly_quota` to `60`–`100` helps stay well within provider sending limits.
:::

---

## Technical Details

For details on MIME assembly, HTML byte budgets, deterministic `Message-ID` deduplication, and SMTP error classification, see [Channel Protocols & Delivery](/reference/channel-protocols).
