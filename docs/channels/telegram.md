---
description: "Push RSS, Atom, and JSON Feed updates to Telegram private chats, groups, supergroup forum topics, and channels using Rich, Compact, or Media modes."
---

# Telegram

The Telegram channel delivers articles to **private chats**, **groups**, **supergroup forum topics**, and **broadcast channels**. Each route pointing to a Telegram channel can independently choose between three presentation modes: **Rich Message**, **Compact Card**, and **Media Mode**, and its own delivery language.

---

## 1. Getting Credentials & Configuring the Channel

1. **Create a Bot**: Message [@BotFather](https://t.me/BotFather) on Telegram and send `/newbot` to receive a **`bot_token`** (for example, `123456789:AAH...`).
2. **Add the Bot to Your Target Chat**:
   - **Private Chat**: Open a chat with your new bot and send `/start`.
   - **Group or Forum Supergroup**: Add the bot as a member of the group.
   - **Broadcast Channel**: Add the bot as a channel administrator with the **Post Messages** permission.
3. **Find the `chat_id` (and Optional `message_thread_id`)**:
   - **Public Channel or Group**: Use the `@username` (for example, `@mychannel`).
   - **Private Channel or Supergroup**: Use the numeric ID starting with `-100` (for example, `-1001234567890`).
   - **Forum Topic**: If your supergroup has **Topics** enabled and you want messages sent into a specific topic thread, note the topic's integer ID (`message_thread_id`).
4. **Configure in the WebUI**: Go to **Channels → New Channel → Telegram** and fill in:

| Field | Required | Default | Description |
| :--- | :---: | :--- | :--- |
| `bot_token` | Yes | — | Bot Token from `@BotFather` (encrypted at rest; shown masked in the WebUI). |
| `chat_id` | Yes | — | Target user ID, group ID (`-100...`), or public channel username (`@mychannel`). |
| `message_thread_id` | No | — | Optional forum topic thread ID for supergroups with Topics enabled. |
| `api_base_url` | No | `https://api.telegram.org` | Optional custom Bot API server URL (for reverse proxies or self-hosted Local Bot API servers). |

Click **Test** to verify the token, check server capabilities, and send a test message (in your current WebUI language).

---

## 2. Choosing a Route Delivery Mode

When creating or editing a **Route** that targets a Telegram channel, you can choose one of three delivery modes:

### Rich Message Mode (`rich_message` — Default)

Best for blogs, long-form articles, and technical writing:
- **Structured Formatting**: Preserves subheadings, lists, compact tables, expandable blockquotes, syntax-highlighted code blocks, and inline images or videos.
- **Read Original Button**: Appends a **"Read original"** button at the bottom of the message linking to the source article. Very long articles are cleanly shortened at paragraph/block boundaries with a notice above the button.
- **Automatic Fallback on Older Bot API Servers**: If you connect `api_base_url` to an older self-hosted Local Bot API server that does not support Rich Message delivery, FeedCourier automatically falls back to **Compact Card** mode so messages are still delivered without failing.

### Compact Card Mode (`compact_card`)

Best for high-frequency news feeds, release notes, or group chats where you want short notifications:
- Sends a concise message containing the **bold article title**, **feed source and author**, **plain-text summary**, and **original article link**.
- Includes a per-route **Link Preview (`link_preview`)** toggle so you can choose whether Telegram expands webpage preview cards below the message.

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
In the **Feed Preview** modal, the **Debug push** bar sends a real item to a Telegram channel with the chosen mode, link preview, and language, without touching the queue. See [Debug Push](/channels/pacing-and-quotas#debug-push-from-the-feed-preview).
:::

---

## 3. Self-Hosted Local Bot API Server {#self-hosted-local-bot-api-server}

If you point `api_base_url` to a self-hosted Telegram Bot API server running on your private network or Docker bridge (for example, `http://telegram-bot-api.internal:8081`), add its hostname or IP range to [`SSRF_ALLOWLIST`](/guide/configuration#allowing-trusted-internal-services-ssrf-allowlist) under **Settings** so outbound connections to that private address are permitted.

---

## Technical Details

For the target Telegram Bot API protocol version, Rich HTML normalization rules, media relay concurrency limits, and batch checkpoint internals, see [Channel Protocols & Delivery](/reference/channel-protocols).
