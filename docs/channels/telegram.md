# Telegram Channel (Bot API 10.4)

FeedCourier's Telegram adapter (`src/channels/telegram.rs`) is built specifically for **Telegram Bot API 10.4**, supporting private chats, supergroup forum topics (`message_thread_id`), broadcast channels, and self-hosted Local Bot API Servers across **three route-selectable presentation modes**.

---

## Channel Configuration

Configure a Telegram channel in the WebUI under **Channels → New Channel → Telegram** (`TelegramChannelConfig`):

| Field | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `bot_token` | `string` | Yes | Telegram Bot Token from `@BotFather` (encrypted at rest via ChaCha20-Poly1305). |
| `chat_id` | `string` | Yes | Target numeric user ID, group/supergroup ID (e.g., `-1001234567890`), or public channel username (`@mychannel`). |
| `message_thread_id` | `i64?` | Optional | Forum topic thread ID when delivering into a supergroup with Topics enabled. |
| `api_base_url` | `string?` | Optional | Custom Bot API server root URL (defaults to `https://api.telegram.org`). |

---

## Three Route-Selectable Presentation Modes

Each **Route** targeting a Telegram channel can independently choose one of three delivery modes (`routes.options_json.telegram_mode`):

### 1. Rich Message Mode (`rich_message` — Default)

Calls Bot API 10.4 `sendRichMessage` powered by the standalone [`telegram-rich`](https://crates.io/crates/telegram-rich) crate ([GitHub](https://github.com/ElleryHe/telegram-rich)):

- **Rich HTML Normalization**: Converts sanitized article HTML into Telegram's official Rich HTML subset, preserving headings, compact tables (`<table compact>`), expandable blockquotes (`<blockquote expandable>`), fenced code blocks with syntax language tags, figures with captions, and lifted standalone media blocks.
- **Limit-Aware Block Truncation (`Document::fit`)**: Automatically fits long articles at clean block boundaries within Telegram's official limits:
  - `32,768` characters
  - `500` blocks
  - `16` nesting depth levels
  - `50` embedded media items
  - `20` table columns
- **Read Original Action Button**: Appends a `<tg-button-row>` button linking directly to the article's canonical URL (plus an explicit truncation notice when shortened).

### 2. Compact Card Mode (`compact_card`)

Calls standard `sendMessage` (`parse_mode: "HTML"`) bounded safely to **`4,096` UTF-16 code units**:

- Assembles a clean, scannable digest card: **Bold Title** + **Feed Source Label** + **Plain-Text Summary** + **Canonical Link**.
- Honors the route-level **`link_preview`** toggle (`LinkPreviewOptions.is_disabled = !link_preview`), allowing you to enable or suppress webpage preview cards per route.

### 3. Media Mode (`media_mode`)

Designed for visual feeds, photography blogs, comics, podcasts, and social/microblog streams:

1. **Full Media Extraction**: Collects media attachments from `<enclosure>`, Media RSS (`<media:content>`), and inline `<img>`, `<video>`, and `<audio>` elements.
2. **Strict 3-Line Minimalist Caption** (bounded to `1,024` UTF-16 code units):
   - **Line 1**: Author name (falls back to Feed Title if the entry has no author).
   - **Line 2**: The **first complete sentence** of the article body, segmented via Unicode UAX #29 (`unicode-segmentation`, handling both CJK `。！？` and English abbreviations cleanly) with trailing sentence punctuation stripped.
   - **Line 3**: Canonical article URL.
3. **URL-First with Zero-Disk Anti-Hotlink Multipart Fallback**:
   - First attempts direct URL delivery so Telegram's servers fetch the media.
   - If the origin server blocks Telegram via hotlink protection (`Referer` check) or returns an error, FeedCourier automatically falls back to streaming the file in memory via `ProtectedHttpClient` (injecting the feed's origin `Referer`, enforcing SSRF checks, and capping at **`10 MiB` for photos** / **`50 MiB` for video/audio/documents** behind a global concurrency semaphore of `2`) and uploads it via `multipart/form-data`—**never touching disk**.
   - Media items exceeding the size cap degrade gracefully to plain links in the message text.
4. **Checkpointed 10-Item Batching (`sendMediaGroup`)**:
   - When an entry contains more than 10 media attachments, FeedCourier splits them into batches of `10` (attaching the 3-line caption only to the first item of the first batch).
   - After each batch succeeds, `CheckpointSink` immediately persists `next_batch_offset` and `sent_message_ids` into `delivery_tasks.checkpoint_json`.
   - If batch #2 hits a `429 retry_after` or the process restarts mid-flight, recovery resumes from `next_batch_offset` without resending batch #1.
5. **No-Media Fallback**: If an entry contains zero media attachments, `media_mode` sends the same 3-line text card via `sendMessage`.

---

## Capability Probing & Self-Hosted Local Bot API Fallback

When you create, update, or test a Telegram channel, FeedCourier probes the target Bot API endpoint (`getMe` + `sendRichMessage` availability probe) and caches a capability snapshot in `channels.capabilities_json`:

```json
{
  "supports_rich_message": true,
  "bot_api_version": "10.4",
  "bot_username": "my_feedcourier_bot",
  "probed_at": 1763174400,
  "fallback_reason": null
}
```

### Automatic Runtime Degradation
If you point `api_base_url` at a self-hosted **Local Telegram Bot API Server** compiled against an older TDLib version (`< 10.2`) that does not yet implement `sendRichMessage`:
- FeedCourier detects the `404 Not Found` / `method not found` response either during initial probing or at delivery time.
- Routes configured for `rich_message` **automatically degrade to `compact_card` (`sendMessage`)**, record `degraded_to_compact: true` in `checkpoint_json`, and update `capabilities_json.fallback_reason` so delivery never stalls.

### Self-Hosted Local Bot API Server & `SSRF_ALLOWLIST`
If your custom `api_base_url` resolves to a private LAN or Docker IP (e.g., `http://telegram-bot-api.internal:8081`), add its hostname or CIDR to [`SSRF_ALLOWLIST`](/guide/configuration#allowing-trusted-internal-services-ssrf-allowlist) so outbound SSRF checks permit the connection.
