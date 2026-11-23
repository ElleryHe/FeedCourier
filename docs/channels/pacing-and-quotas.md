---
description: "Configure Channel Account pacing, per-delivery random jitter, shared hourly/daily quotas, per-route IANA timezones, delivery time windows, cascading filters, global queue pause, and debug push."
---

# Send Rate, Quotas, Windows & Filters

When a feed publishes multiple articles at once—or when several feeds update at the same time—sending all of them immediately can trigger downstream rate limits (such as SMTP temporary deferrals or HTTP `429 Too Many Requests` on Telegram and Notion).

FeedCourier uses a **two-layer Delivery Channel model** (**Channel Account** + multiple **Destinations**) and queues each matched article as an individual delivery task, spacing out messages using shared account-level intervals, per-delivery random jitter, rolling hourly/daily quotas, and route-level schedules.

---

## 1. Two-Layer Channels & Per-Article Queueing

Every delivery target is organized into two layers:

- **Channel Account** (configured once under **Channels**): stores encrypted credentials, outbound proxy toggle (`use_proxy`, for HTTP-based Telegram and Notion accounts; SMTP uses native TCP/TLS direct connections), pacing (`min_interval_secs`, `jitter_max_secs`), rolling quotas (`hourly_quota`, `daily_quota`), and downstream rate-limit cooldowns (`cooldown_until`).
- **Destinations** (one or more attached to each account): stores target-specific routing settings (Telegram `chat_id` + `message_thread_id`, SMTP `to_addresses` + `reply_to`, or Notion `data_source_id` + `column_mapping`). Each destination can be enabled, disabled, or tested independently.

Instead of combining multiple articles into a digest, FeedCourier creates one delivery task for each new article and each matching route (pointing to a specific **Destination**):

$$\text{1 Delivery Task} = \text{1 Article} \times \text{1 Matched Route}$$

If the same article matches routes targeting Email, Telegram, and Notion, each account processes its own task independently. A failure or quota delay on one account does not block delivery to the others.

---

## 2. Minimum Interval & Per-Delivery Random Jitter

When creating or editing a **Channel Account** in the WebUI (**Channels**), you can configure how quickly consecutive messages are sent:

| Field | Default | Description |
| :--- | :--- | :--- |
| `min_interval_secs` | `15` | Minimum number of seconds to wait after a completed delivery before sending the next message on this account (and any sibling accounts sharing the same credentials). |
| `jitter_max_secs` | `10` | Maximum random delay (`0` to `jitter_max_secs` seconds) added on top of `min_interval_secs`. |

With the default `15` second minimum interval and `10` second random jitter, consecutive deliveries are spaced **15 to 25 seconds apart**. When a feed publishes 12 items at once, they are delivered smoothly over several minutes rather than in a single burst.

::: info How Random Jitter Works
- **Sampled Independently Per Delivered Task**: Every time a delivery succeeds, FeedCourier draws a fresh uniform random delay in `0..=jitter_max_secs` and adds `min_interval_secs` to compute the earliest allowed send time for the next task. A batch of tasks **never** shares a single jitter value.
- **In-Memory Only Across Restarts**: The randomized jitter timestamp is kept in memory only. After a process restart, the first delivery honors `min_interval_secs` based on the persisted `last_completed_at` timestamp in SQLite (`delivery_quota_ledger`); subsequent deliveries resume full `min_interval_secs + jitter` spacing.
:::

---

## 3. Shared Pacing, Quotas & Cooldowns Across Destinations

Because upstream providers enforce rate limits per Bot Token, SMTP account, or Notion Integration—not per chat or recipient list—FeedCourier enforces all rate limits at the **Channel Account** and **credential group** (`credential_group_key`) level:

- **All Destinations Under One Account**: If one Telegram Bot account sends to 4 groups/topics, one SMTP account sends to 3 recipient groups, or one Notion Integration writes to 2 databases, all of those destinations share the same `min_interval_secs` + per-delivery random jitter, the same `hourly_quota` and `daily_quota`, and the same upstream `429` cooldown window.
- **Separate Accounts Sharing Identical Credentials**: Even if you create two separate Channel Accounts that use the exact same underlying credentials (SMTP host/port/username + password SHA-256 digest, Telegram Bot Token on the same `api_base_url`, or Notion Integration Secret), FeedCourier automatically links them via `credential_group_key` so their pacing, rolling quotas (enforcing the strictest effective limit in the group), and `429` cooldowns are still shared. Because SMTP includes the password digest, distinct API keys sharing a generic username (such as SendGrid's `apikey`) remain isolated.
- **Group-Wide Cooldowns**: If any destination receives a `429 Retry-After` response or temporary SMTP throttling, every destination on that account (and any account sharing the same credentials) pauses together until `cooldown_until` elapses.

---

## 4. Hourly & Daily Quotas

Each **Channel Account** supports two optional rolling volume caps shared across all of its destinations:

- **`hourly_quota`**: Maximum number of deliveries allowed within a rolling 1-hour window (`3,600` seconds).
- **`daily_quota`**: Maximum number of deliveries allowed within a rolling 24-hour window (`86,400` seconds).

When an account reaches its hourly or daily quota:
- Remaining tasks are placed in **`deferred`** status rather than being dropped or marked as failed.
- Waiting for a quota window does **not** count against a task's error retry limit.
- Once the rolling window frees capacity, deferred tasks automatically resume in chronological order.

::: info Persistent Quota Ledger (`delivery_quota_ledger`)
Every confirmed delivery appends a lightweight record (`credential_group_key`, `channel_id`, `completed_at`) to the dedicated `delivery_quota_ledger` table, which intentionally has no foreign keys to feeds, routes, or destinations and is automatically pruned after 24 hours. Because rolling 1-hour / 24-hour quota usage and `last_completed_at` pacing timestamps are read from this ledger rather than `delivery_tasks`, **deleting feeds, routes, or destinations—or running Clear cache—never resets your quota usage or pacing intervals**.
:::

---

## 5. Route Matching, Filters & WebUI Filtering

In the WebUI under **Routes** (or via the [`configure_route`](/mcp/tools#6-configure-route) MCP tool), you control which feeds and articles are routed to a specific **Destination**.

### Organizing Channels & Routes in the WebUI

- **Channels Page Filter**: Filter accounts at the top of the **Channels** page by **Channel Kind** (`All` / `Telegram` / `SMTP` / `Notion`).
- **Routes Page Cascading Filters**: Narrow down large route lists with two cascading dropdowns—first choose **Channel Kind** (`All` / `Telegram` / `SMTP` / `Notion`), then choose **Account** (which lists only the Bots, SMTP senders, or Notion Integrations matching the selected kind).

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
You can test keyword and regex rules against live feed entries without creating delivery tasks by using the **Feed Preview** drawer in the WebUI or the [`preview_feed_latest` MCP tool](/mcp/tools#4-preview-feed-latest). Each preview item shows whether it matched (`matched_filter`) and which rule accepted or rejected it (`filter_reason`).
:::

---

## 6. Route Timezone, Delivery Windows & Language {#route-delivery-time-windows}

Each route can restrict **when** it delivers and customize **how** localized dates and text appear. In the WebUI under **Routes** (or via `configure_route`), configure:

| Option | Description |
| :--- | :--- |
| **`timezone`** | Optional **IANA timezone** override for this route (for example `Asia/Shanghai`, `America/New_York`, or `Europe/London`). When omitted, the route inherits the **system timezone** (default `America/Los_Angeles`, configurable via `TIMEZONE`/`TZ` or **Settings**). |
| **`active_windows`** | Daily windows as `{ "start": "HH:MM", "end": "HH:MM" }` pairs, evaluated in the **route timezone** with DST-safe boundaries. An `end` earlier than `start` wraps past midnight (`22:00`–`06:00`); **only `00:00`–`00:00` (or an empty list `[]`)** means all day—any other equal `start == end` window (such as `08:00`–`08:00`) is rejected with `400 Bad Request`. |
| **`active_weekdays`** | Weekdays as numbers `1` (Monday) to `7` (Sunday). An empty list means every day. For a window that wraps midnight, the weekday is the day the window **starts**. |
| **`locale`** | Delivery language (`en` or `zh-CN`) for localized notification copy. Pre-filled with your current WebUI language on create; routes without it inherit the legacy system locale. |

### What the Route Timezone Governs

A route's effective timezone (`options.timezone` → fallback `system_settings.timezone`) controls three things simultaneously:

1. **Delivery Time Windows & Weekdays**: `active_windows` and `active_weekdays` are evaluated in the route timezone.
2. **SMTP Email Article Timestamps**: The publication timestamp rendered in the email header line uses the route timezone.
3. **Telegram Rich Message `<footer>` Timestamps**: The metadata footer below the article title displays `Source · Author · Localized Publish Time` in the route timezone.

Localized article timestamps are formatted with an explicit UTC offset—for example:

```text
2026-11-08 11:00 (UTC+08:00)
```

::: info DST-Safe Windows, Outside-Window Queuing & Live Route Updates
- **Outside the Window**: Tasks are not dropped when a route is outside its window. They stay in the queue (`queued` / `deferred`) with their next attempt scheduled for the **next window start**, and are then delivered in chronological order, still subject to account intervals and quotas.
- **Daylight Saving Time (DST) Safety**: During spring-forward transitions (non-existent local times such as `02:30`), local window boundaries clamp forward to the first valid instant after the gap. During fall-back transitions (ambiguous repeated local times), window starts use the earliest occurrence and window ends use the latest occurrence so deliveries are never cut short.
- **Updating a Route with Pending Tasks**: Changing a route's target **Destination** atomically repoints pending `queued`, `deferred`, and `retrying` tasks to the new destination, clearing `checkpoint_json` and resetting `attempt_count = 0` (tasks currently mid-flight in `delivering` are left untouched to finish). Changing `active_windows`, `active_weekdays`, or `timezone` immediately resets `next_attempt_at = now` on `queued` and `deferred` tasks and wakes the outbox dispatcher.
:::

---

## 7. Delivery Order & Global Queue Pause {#delivery-order-and-queue-pause}

- **Chronological backlog**: When a feed is polled for the first time (or after **Clear cache**), its backlog is queued oldest → newest by publish time (falling back to the reverse feed order).
- **Merged multi-feed order**: The global outbox claims due tasks by article publish time (falling back to discovery time), oldest first. When several feeds have pending tasks, they are delivered as one merged chronological stream, subject to account intervals and quotas.
- **Global pause**: On the **Tasks** page you can pause the whole queue (now, for 15 min – 6 h, or until a chosen time) and resume it manually. While paused, feeds are still polled and tasks accumulate; on resume everything due is delivered oldest-first. See [Backup & Maintenance](/guide/operations#pausing-resuming-the-delivery-queue).

---

## 8. Debug Push from the Feed Preview {#debug-push-from-the-feed-preview}

To check how an article will look in a destination without subscribing or touching the queue, open the **Feed Preview** for a URL (or for an existing feed) and use the **Debug push** bar: choose a target **Destination**, (for Telegram) the delivery mode and link preview, the delivery language, an optional timezone override, and optional route plugins (such as Link Rewrite), then click **Push latest** or **Push this item** on a specific preview item.

A debug push is **stateless**: it does not create fingerprints, does not enter the queue, ignores route filters, time windows, and quotas, and does not count against account limits. For SMTP destinations, debug push strictly delivers only to **`verified`** recipients via isolated single-`To:` envelopes with compliance headers (returning an error when 0 verified recipients exist). Notion pages created this way use a `test-` prefixed `FC-ID`, so they never collide with real deliveries. Failures are reported in the result message. The MCP equivalent is [`test_push_feed_entry`](/mcp/tools#10-test-push-feed-entry).

---

## Technical Details

For internal details on the outbox state machine, lease recovery, and rate-limit group hashing, see [System Architecture](/reference/architecture) and [Channel Protocols & Delivery](/reference/channel-protocols).
