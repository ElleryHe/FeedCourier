---
description: "Configure send intervals, random jitter, hourly and daily delivery quotas, shared credential rate limits, route delivery time windows, global queue pause, delivery ordering, and keyword or regex route filters."
---

# Send Rate, Quotas, Windows & Filters

When a feed publishes multiple articles at once—or when several feeds update at the same time—sending all of them immediately can trigger downstream rate limits (such as SMTP temporary deferrals or HTTP `429 Too Many Requests` on Telegram and Notion).

FeedCourier queues each matched article as an individual delivery task and spaces out outgoing messages using per-channel intervals, random jitter, shared account limits, and optional hourly/daily quotas.

---

## 1. Per-Article Queueing

Instead of combining multiple articles into a daily digest, FeedCourier creates one delivery task for each new article and each matching route:

$$\text{1 Delivery Task} = \text{1 Article} \times \text{1 Matched Route}$$

If the same article matches routes for Email, Telegram, and Notion, each channel processes its own task independently. A failure or quota delay on one channel does not block delivery to the others.

---

## 2. Minimum Interval & Random Jitter

When creating or editing a channel in the WebUI (**Channels**), you can configure how quickly consecutive messages are sent:

| Field | Default | Description |
| :--- | :--- | :--- |
| `min_interval_secs` | `15` | Minimum number of seconds to wait after a delivery before sending the next message on this channel (or shared account). |
| `jitter_max_secs` | `10` | Maximum random delay (`0` to `jitter_max_secs` seconds) added on top of `min_interval_secs`. |

With the default `15` second minimum interval and `10` second random jitter, consecutive deliveries are spaced **15 to 25 seconds apart**. When a feed publishes 12 items at once, they are delivered smoothly over several minutes rather than in a single burst.

---

## 3. Shared Rate Limits Across Channels with the Same Credentials

You may want to create multiple channels in the WebUI that use the same underlying account or token—for example:
- Multiple Telegram channels using the same **Bot Token** to post to different groups or forum topics;
- Multiple Notion channels using the same **Integration Secret** to write to different databases;
- Multiple SMTP channels using the same **SMTP username and server** to send to different recipient lists.

FeedCourier automatically groups channels that share the same underlying credentials:
- They share the same send interval (`min_interval_secs` + `jitter_max_secs`) so they do not send messages at the exact same second.
- If one channel receives a rate-limit response (`429 Retry-After` or temporary SMTP throttling), all channels using that credential pause until the cooldown window finishes.

---

## 4. Hourly & Daily Quotas

Each channel also supports two optional volume caps:

- **`hourly_quota`**: Maximum number of deliveries allowed within a rolling 1-hour window (`3,600` seconds).
- **`daily_quota`**: Maximum number of deliveries allowed within a rolling 24-hour window (`86,400` seconds).

When a channel reaches its hourly or daily quota:
- Remaining tasks are placed in **`deferred`** status rather than being dropped or marked as failed.
- Waiting for a quota window does **not** count against a task's error retry limit.
- Once the rolling window frees capacity, deferred tasks automatically resume in order.

---

## 5. Route Matching & Content Filters

In the WebUI under **Routes** (or via the `configure_route` MCP tool), you control which feeds and which articles are routed to a channel.

### Step 1: Feed & Tag Selection

Each route can filter by specific feeds or tags:
- **`feed_ids`**: Select one or more specific feeds.
- **`match_tags`**: Match feeds carrying any of the specified tags (case-insensitive, e.g., `rust`, `security`).
- If **both** `feed_ids` and `match_tags` are left empty, the route matches **all active feeds**. Otherwise, a feed matches if it is listed in `feed_ids` **or** has at least one tag in `match_tags`.

### Step 2: Keyword & Regular Expression Filters

For matched feeds, the route evaluates filters against the article's **title and plain-text body** (when full-text extraction is enabled on the feed, filters search the extracted full text; otherwise, they search the title and feed summary) in the following order:

| Filter Rule | Behavior |
| :--- | :--- |
| **`exclude_keywords`** | Case-insensitive keyword list. If **any** excluded keyword appears in the title or text, the article is skipped. |
| **`exclude_regex`** | Regular expression. If the title or text matches this pattern, the article is skipped. |
| **`include_keywords`** | Case-insensitive keyword list. When non-empty, **at least one** keyword must appear in the title or text. |
| **`include_regex`** | Regular expression. When set, the title or text must match this pattern. |

::: tip Previewing Filters Before Saving
You can test keyword and regex rules against live feed entries without creating delivery tasks by using the **Feed Preview** drawer in the WebUI or the [`preview_feed_latest` MCP tool](/mcp/tools#3-preview-feed-latest). Each preview item shows whether it matched (`matched_filter`) and which rule accepted or rejected it (`filter_reason`).
:::

---

## 6. Route Delivery Time Windows {#route-delivery-time-windows}

Each route can restrict **when** it delivers, so notifications do not arrive at night or on days you do not want them. In the WebUI under **Routes** (or via `configure_route`), set:

| Option | Description |
| :--- | :--- |
| **`active_windows`** | Daily windows as `{ "start": "HH:MM", "end": "HH:MM" }` pairs, evaluated in the **system timezone**. An `end` earlier than `start` wraps past midnight (`22:00`–`06:00`); `start` equal to `end` means the whole day; an empty list means always. |
| **`active_weekdays`** | Weekdays as numbers `1` (Monday) to `7` (Sunday). An empty list means every day. For a window that wraps midnight, the weekday is the day the window **starts**. |

The system timezone defaults to `America/Los_Angeles` (from the `TIMEZONE` / `TZ` environment variable on first start) and can be changed in **Settings**. See [Configuration](/guide/configuration).

::: info Outside the Window Tasks Are Still Queued
Tasks are not dropped when the route is outside its window. They stay in the queue (`queued` / `deferred`) with their next attempt scheduled for the **next window start**, and are then delivered in chronological order, still subject to channel intervals and quotas.
:::

### Route Delivery Language

Each route has its own delivery language (`en` or `zh-CN`) used for the localized parts of pushed messages (such as the Telegram Media Mode link label and SMTP/Notion template text). The WebUI pre-fills it with your current interface language when you create a route. The interface language itself is only a browser preference and does not change what is delivered. Routes created before this option existed fall back to the legacy system-wide language.

---

## 7. Delivery Order & Global Queue Pause {#delivery-order-and-queue-pause}

- **Chronological backlog**: When a feed is polled for the first time (or after **Clear cache**), its backlog is queued oldest → newest by publish time (falling back to the reverse feed order).
- **Merged multi-feed order**: The global outbox claims due tasks by article publish time (falling back to discovery time), oldest first. When several feeds have pending tasks, they are delivered as one merged chronological stream, subject to channel intervals and quotas.
- **Global pause**: On the **Tasks** page you can pause the whole queue (now, for 15 min – 6 h, or until a chosen time) and resume it manually. While paused, feeds are still polled and tasks accumulate; on resume everything due is delivered oldest-first. See [Backup & Maintenance](/guide/operations#pausing-resuming-the-delivery-queue).

---

## 8. Debug Push from the Feed Preview {#debug-push-from-the-feed-preview}

To check how an article will look in a channel without subscribing or touching the queue, open the **Feed Preview** for a URL (or for an existing feed) and use the **Debug push** bar: choose a channel, (for Telegram) the delivery mode and link preview, and the delivery language, then click **Push latest** or **Push this item** on a specific preview item.

A debug push is **stateless**: it does not create fingerprints, does not enter the queue, ignores route filters, time windows, and quotas, and does not count against channel limits. Notion pages created this way use a `test-` prefixed `FC-ID`, so they never collide with real deliveries. Failures are reported in the result message. The MCP equivalent is [`test_push_feed_entry`](/mcp/tools#9-test-push-feed-entry).

---

## Technical Details

For internal details on the outbox state machine, lease recovery, and rate-limit group hashing, see [System Architecture](/reference/architecture) and [Channel Protocols & Delivery](/reference/channel-protocols).
