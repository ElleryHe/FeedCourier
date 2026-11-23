---
description: "Push RSS, Atom, and JSON Feed updates to multiple Telegram private chats, groups, forum topics, and channels from a single Bot Account using Rich, Compact, or Media modes."
---

# Telegram

The Telegram channel delivers articles to **private chats**, **groups**, **supergroup forum topics**, and **broadcast channels**. Under FeedCourier's two-layer channel model, you configure each **Telegram Bot Account** once and attach as many **Destinations** (chats, groups, topics, or channels) as you need. Each route targeting a Telegram destination independently selects its presentation mode (**Rich Message**, **Compact Card**, or **Media Mode**), delivery language, and timezone.

---

## 1. Getting Credentials & Configuring Account + Destinations

1. **Create a Bot**: Message [@BotFather](https://t.me/BotFather) on Telegram and send `/newbot` to receive a **`bot_token`** (for example, `123456789:AAH...`).
2. **Add the Bot to Your Target Chat(s)**:
   - **Private Chat**: Open a chat with your bot and send `/start`.
   - **Group or Forum Supergroup**: Add the bot as a member of the group.
   - **Broadcast Channel**: Add the bot as a channel administrator with the **Post Messages** permission.
3. **Find Each Destination's `chat_id` (and Optional `message_thread_id`)**:
   - **Public Channel or Group**: Use the `@username` (for example, `@mychannel`).
   - **Private Channel or Supergroup**: Use the numeric ID starting with `-100` (for example, `-1001234567890`).
   - **Forum Topic**: If your supergroup has **Topics** enabled and you want messages sent into a specific topic thread, note the topic's integer ID (`message_thread_id`).
4. **Configure in the WebUI**: Go to **Channels → New Channel → Telegram**.

### Telegram Account (Bot) Fields

| Field | Required | Default | Description |
| :--- | :---: | :--- | :--- |
| `name` | No | *Auto-fetched* | Account display name. If left blank, automatically fetched via Telegram `getMe` (`@username`). |
| `bot_token` | Yes | — | Bot Token from `@BotFather` (encrypted at rest; shown masked in the WebUI). |
| `api_base_url` | No | `https://api.telegram.org` | Optional custom Bot API server URL (for reverse proxies or self-hosted Local Bot API servers). Only `http://` or `https://` URLs without userinfo, query string, or fragment are permitted, and all requests enforce connect-time `SsrfPolicy` validation. |

### Telegram Destination Fields (Add Multiple per Bot)

| Field | Required | Default | Description |
| :--- | :---: | :--- | :--- |
| `name` | No | *Auto-fetched* | Destination label. If left blank, automatically fetched via Telegram `getChat` (group, channel, or user title). |
| `chat_id` | Yes | — | Target user ID, group ID (`-100...`), or public channel username (`@mychannel`). |
| `message_thread_id` | No | — | Optional forum topic thread ID for supergroups with Topics enabled. |
| `enabled` | No | `true` | Independently enable or pause this chat/topic without pausing other destinations on the same bot. |

### Account vs. Destination Testing & Shared Limits

- **Test Account**: Calls `getMe` and probes `sendRichMessage` capability on your Bot API server without sending a chat message.
- **Test Destination**: Sends a verification message (in your current WebUI language) to that specific chat or topic.
- **Shared Rate Limits & Quotas**: All destinations under the same Bot Account share `min_interval_secs`, per-delivery random jitter, `hourly_quota`, `daily_quota`, `use_proxy`, and Telegram `429 retry_after` cooldowns. See [Send Rate, Quotas, Windows & Filters](/channels/pacing-and-quotas).

---

## 2. Choosing a Route Delivery Mode

When creating or editing a **Route** that targets a Telegram destination, you can choose one of three delivery modes:

### Rich Message Mode (`rich_message` — Default)

Best for blogs, long-form articles, and technical writing (powered by [`telegram-rich 0.0.2`](https://crates.io/crates/telegram-rich)):
- **Clean Mobile-Friendly Hierarchy**: Renders the article title as a calm, plain-foreground **`<h3>`** (no oversized `<h1>`/`<h2>` or distracting full-title link blue), places **`Source · Author · Compact Publish Time`** (in the route's timezone, e.g. `2026-11-08 11:00`) in a native Telegram **`<footer>`** directly underneath, and separates header and body with a horizontal divider (`<hr>`).
- **Automatic Body Heading Demotion**: Original headings inside the article body are demoted by three levels (`h1→h4`, `h2→h5`, `h3+→h6`) so section headings stay visually subordinate to the article title.
- **Automatic Media Collages & Slideshows**: Two or more consecutive inline images in the article body are automatically grouped into a Telegram **collage** (or a **slideshow** when exceeding collage limits), alongside preserved lists, compact tables, expandable blockquotes, and syntax-highlighted code blocks.
- **Read Original Button**: Appends a **"Read original"** button at the bottom of the message linking to the source article. Very long articles are cleanly shortened at paragraph/block boundaries with a notice above the button.
- **Automatic Fallback on Older Bot API Servers**: If you connect `api_base_url` to an older self-hosted Local Bot API server that does not support Rich Message delivery, FeedCourier automatically falls back to **Compact Card** mode so messages are still delivered without failing.

### Compact Card Mode (`compact_card`)

Best for high-frequency news feeds, release notes, or group chats where you want short notifications:
- **Editorial Card Layout**: Renders the **bold article title** at top, wraps the plain-text summary inside a native Telegram **`<blockquote>`** (automatically upgrading to a collapsible **`<blockquote expandable>`** when longer than `300` UTF-16 code units, cleanly word/sentence-boundary truncated at `<= 800` units, and omitting the quote block when the summary duplicates the title), places italic **`Source · Author · YYYY-MM-DD HH:MM`** metadata at the bottom, and attaches a native inline URL button (**"Read original"** / **"阅读原文"**).
- **Noise-Free Summary Text**: Automatically suppresses false-positive Telegram auto-highlighting on `/commands`, `@scoped/packages`, and code/doc filenames (such as `README.md`, `README.fr.md`, `sku.go`) while preserving real `http(s)://` URLs.
- Includes a per-route **Link Preview (`link_preview`)** toggle so you can choose whether Telegram expands webpage preview cards below the message (always targeting the original canonical URL even when Link Rewrite shortens the button URL).

### Media Mode (`media_mode`)

Best for photography feeds, comics, illustrations, social updates, podcasts, and video feeds:
- **Media Extraction**: Collects images, videos, and audio attachments from the feed item and article body.
- **3-Line Caption**: Keeps captions concise with three lines:
  1. **Line 1**: Author name (or feed title if no author is listed).
  2. **Line 2**: The first complete sentence of the article text.
  3. **Line 3**: Hyperlink labeled **`Link`** (`链接` in Simplified Chinese, following the route's delivery language) pointing to the original article.
  *(If an entry has no media attachments, Media Mode sends the same 3-line HTML message.)*
- **Multi-Item Batching**: When an article contains more than 10 images or videos, FeedCourier automatically splits them into media-group batches of up to **10 items** (attaching the 3-line caption to the first item). If a rate limit occurs mid-way through a multi-batch post, retries resume from the remaining batches without resending earlier ones.
- **Inline Video Streaming & Hotlink Protection Relay**: Images and audio try direct URL delivery first and automatically fall back to in-memory upload (`<= 10 MiB` for photos, `<= 50 MiB` for audio/video) if the origin site enforces hotlink (`Referer`) checks. Videos and unlabeled `<enclosure>` attachments are streamed directly in memory so FeedCourier can inspect container magic bytes, extract MP4 dimensions (including portrait rotation) and duration, and upload them with `supports_streaming: true` for inline playback inside Telegram without writing temporary files to disk.

::: tip Try a Mode Before Saving
In the **Feed Preview** modal, the **Debug push** bar sends a real item to a Telegram destination with the chosen mode, link preview, language, and timezone without touching the queue. See [Debug Push](/channels/pacing-and-quotas#debug-push-from-the-feed-preview).
:::

---

## 3. Self-Hosted Local Bot API Server & SSRF Validation {#self-hosted-local-bot-api-server}

**All** Telegram Bot API requests—both to the official `https://api.telegram.org` endpoint and to any custom `api_base_url` (including self-hosted Local Bot API servers)—enforce connect-time `SsrfPolicy` DNS IP validation (sharing the hot-reloadable `SSRF_ALLOWLIST`) with automatic HTTP redirects disabled. In addition:

- `api_base_url` only allows `http://` or `https://` schemes and rejects embedded userinfo (`user:pass@`), query strings (`?…`), or fragments (`#…`).
- Account creation, updates, and capability probes blocked by SSRF policy return HTTP `403` (`ssrf_blocked`).

::: warning Upgrade Note for Existing Local Bot API Servers
Existing internal or private-network Local Bot API endpoints configured prior to `v0.2.0` are **not** automatically allowlisted. If your `api_base_url` points to localhost, a Docker bridge service, or an RFC 1918 private network (for example, `http://telegram-bot-api.internal:8081`), you must explicitly add its hostname or IP/CIDR to [`SSRF_ALLOWLIST`](/guide/configuration#allowing-trusted-internal-services-ssrf-allowlist) via environment variable or under **Settings** so outbound connections are permitted.
:::

---

## Technical Details

For the target Telegram Bot API protocol version, `telegram-rich 0.0.2` normalization rules, media relay concurrency limits, and batch checkpoint internals, see [Channel Protocols & Delivery](/reference/channel-protocols).
