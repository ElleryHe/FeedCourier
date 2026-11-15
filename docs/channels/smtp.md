# SMTP Newsletters Channel

FeedCourier's SMTP adapter (`src/channels/smtp.rs`) carries forward `rss2email`'s single-article email philosophy while solving modern email deliverability, SPF/DKIM/DMARC alignment, and Gmail's `102 KB` clipping limit.

---

## Key Capabilities

1. **Responsive Inline-CSS Single-Column Reader Template**: Rendered via `minijinja` with zero external stylesheet dependencies, paired with a 78-column plain-text fallback (`html2text`) inside a standard `multipart/alternative` MIME envelope.
2. **Strict `95 KiB` HTML Budget (Anti-Clipping)**: Automatically truncates oversized HTML bodies at clean tag/character boundaries below `MAX_EMAIL_HTML_BYTES = 95 KiB`, appending a *"Continue reading original article →"* callout so Gmail and Fastmail never clip the footer or break layout styling.
3. **`Force-From` Deliverability Discipline**: Always sends from your authenticated SPF/DKIM/DMARC-aligned `from_address` while dynamically formatting the display name as `"<Feed Title> via FeedCourier"`.
4. **Deterministic `Message-ID` & `List-Id` Headers**: Generates a reproducible RFC 5322 `Message-ID` from the entry's SHA-256 fingerprint and a per-feed `List-Id` header so email clients can deduplicate retries and auto-filter newsletters into folders.
5. **Pooled Async Transports**: Reuses persistent `lettre` `AsyncSmtpTransport` connections across deliveries.

---

## Configuration Fields

SMTP channels are configured in the WebUI under **Channels → New Channel → SMTP** (`SmtpChannelConfig`):

| Field | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `host` | `string` | Yes | SMTP server hostname (e.g., `smtp.fastmail.com`, `smtp.gmail.com`, `email-smtp.us-east-1.amazonaws.com`). |
| `port` | `u16` | Yes | SMTP port (`587` for STARTTLS, `465` for implicit TLS/SMTPS, `25`/`1025` for local relay). |
| `security` | `string` | Yes | Transport encryption mode: `"start_tls"` (default), `"tls"`, or `"plain"`. |
| `username` | `string?` | Optional | SMTP authentication username (encrypted at rest). |
| `password` | `string?` | Optional | SMTP authentication password or App Password (encrypted at rest via ChaCha20-Poly1305). |
| `from_address` | `string` | Yes | Fixed sender email address (`Force-From`) authorized by your SMTP provider's SPF/DKIM records. |
| `to_addresses` | `string[]` | Yes | One or more recipient email addresses locked inside this trusted channel. |
| `reply_to` | `string?` | Optional | Optional `Reply-To` address. |

---

## How `Force-From` Protects Your Sender Reputation

Traditional tools that spoof the feed author's domain in the `From:` header fail modern DMARC policies and get rejected by Gmail, iCloud, and Proton Mail.

FeedCourier enforces `Force-From` on every outgoing message:

```http
From: "Rust Blog" <courier@yourdomain.com>
To: reader@example.com
Subject: [Rust Blog] Announcing Rust 1.94.0
Message-ID: <fc-entry-8f3c9a2e1b4d7051c8a9e2b3@yourdomain.com>
List-Id: "Rust Blog" <feed-12.yourdomain.com>
Auto-Submitted: auto-generated
X-Mailer: FeedCourier
Content-Type: multipart/alternative; boundary="..."
```

- **`From:`** uses your verified `from_address` (`courier@yourdomain.com`) with the sanitized feed title as the display name (`"Rust Blog"`).
- **`Message-ID:`** is derived deterministically from `(feed_id, fingerprint_sha256, route_id, domain)` and persisted in `delivery_tasks.checkpoint_json` (`{"kind": "smtp", "message_id": "<...>"}`) before transmission. If a network drop causes an at-least-once retry, the retried email carries the **exact same `Message-ID`**, allowing standards-compliant mail servers and clients to deduplicate it automatically.
- **`List-Id:`** uses `<feed-{id}.{domain}>`, making it trivial to create inbox rules or Sieve filters per feed.
- **`Auto-Submitted: auto-generated`** prevents vacation auto-responders from replying to automated feed deliveries.

---

## Error Handling & Rate-Limit Cooldowns

| SMTP Response Code | Classification | Outbox Behavior |
| :--- | :--- | :--- |
| `250 OK` | `Delivered` | Task marked `delivered`, `checkpoint_json` records `message_id`. |
| `421`, `450`, `451`, `452` (4xx Transient) | `Retry` + Group Cooldown | Task transitions to `retrying` with exponential backoff; all channels sharing the SMTP `credential_group_key` enter cooldown. |
| `535`, `550`, `553`, `554` (5xx Permanent) | `PermanentFailure` | Task moves immediately to `dead_letter` with the full SMTP diagnostic string recorded in `diagnostic_log`. |

::: tip Recommended Pacing for Consumer Mailboxes
When sending through personal Gmail, Fastmail, or iCloud SMTP accounts, keep `min_interval_secs` at `15`–`30` seconds and set `hourly_quota` to `60`–`100` to stay comfortably within provider anti-abuse thresholds.
:::
