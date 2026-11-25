---
description: "Reference for FeedCourier's 10 MCP tools covering feed status inspection, secret-free plugin discovery (list_plugins), article preview, failed delivery diagnostics, feed management, route and plugin configuration, global queue control, and debug push."
---

# Tools Overview

FeedCourier exposes **10 structured MCP tools** across two permission scopes (`readonly` and `admin`). All tools return structured JSON responses.

---

## Tool Summary

| Tool Name | Minimum Scope | Modifies State? | Purpose |
| :--- | :--- | :--- | :--- |
| [`list_feeds_status`](#1-list-feeds-status) | `readonly` | No | View system overview counters, redacted channel accounts and destinations (ID, name, kind, enabled only), active routes (including `plugins`), and feed health. |
| [`list_plugins`](#2-list-plugins) | `readonly` | No | List installed plugins and their provider instances (IDs, names, hooks, enabled status only — strictly zero secrets or API URLs). |
| [`inspect_failed_deliveries`](#3-inspect-failed-deliveries) | `readonly` | No | Inspect delivery tasks in `dead_letter`, `retrying`, `deferred`, `queued`, or `all` states. |
| [`preview_feed_latest`](#4-preview-feed-latest) | `readonly` | No | Fetch up to 10 recent entries from a URL or `feed_id` to test full-text extraction and content filters. |
| [`manage_feed`](#5-manage-feed) | `admin` | Yes | Add, update, pause, resume, clear cache, or delete a feed subscription. |
| [`configure_route`](#6-configure-route) | `admin` | Yes | Create, update, or delete a delivery route linked to an existing trusted `destination_id` (or legacy single-destination `channel_id`), including per-route `plugins` bindings. |
| [`reset_and_poll_feed`](#7-reset-and-poll-feed) | `admin` | Yes | Reset failure counters on a feed and trigger an immediate poll. |
| [`retry_dead_tasks`](#8-retry-dead-tasks) | `admin` | Yes | Requeue failed delivery tasks by `task_ids` or `all_dead_letter: true`. |
| [`manage_outbox_queue`](#9-manage-outbox-queue) | `admin` | Yes | Check, pause, or resume the global delivery queue (optionally with a scheduled resume). |
| [`test_push_feed_entry`](#10-test-push-feed-entry) | `admin` | No (stateless) | Debug push of one feed entry to a trusted `destination_id` (or legacy single-destination `channel_id`) without fingerprints, queue, or quota. |

::: info WebUI-Only Security Boundaries
Channel account/destination creation, plugin provider credentials (such as YOURLS API URLs and tokens), and SMTP recipient verification/unsubscription are **WebUI-only** and cannot be mutated over `/mcp`.
:::

---

## Read-Only Inspection & Preview Tools (`readonly` + `admin`)

### 1. `list_feeds_status` {#1-list-feeds-status}

Returns system-wide counters (`overview`), redacted channel accounts (`channels`, each with `id`, `name`, `kind`, `enabled`, `cooldown_until`, and nested `destinations`), a top-level redacted `destinations` list (`id`, `channel_id`, `name`, `kind`, `enabled` — never exposing secrets, email addresses, `chat_id`, or `data_source_id`), configured routes (with `channel_id`, `destination_id`, `locale`, `timezone`, `active_windows`, `active_weekdays`, and `plugins`), and subscribed feeds. The response also includes the global outbox state `queue: { paused, resume_at }` and the system `timezone`.

#### Input Parameters
| Parameter | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `health_status` | `string?` | Optional | Filter feeds by `"active"`, `"backoff"`, `"circuit_open"`, or `"paused"`. |
| `tag` | `string?` | Optional | Case-insensitive tag filter matching feeds carrying this tag. |

#### Example Call
```json
{
  "name": "list_feeds_status",
  "arguments": {
    "health_status": "backoff",
    "tag": "rust"
  }
}
```

#### Example Prompt
> *"Show me all feeds tagged `rust`, and check if any feeds are currently in `backoff` or `circuit_open`."*

---

### 2. `list_plugins` {#2-list-plugins}

Returns a secret-free catalog of all installed plugins and their configured provider instances so AI assistants can discover valid `plugin_id` and `provider_id` integers for [`configure_route`](#6-configure-route) without ever seeing provider URLs, signature tokens, usernames, or passwords.

#### Input Parameters
Takes an empty object `{}` (no parameters).

#### Example Response
```json
{
  "plugins": [
    {
      "id": "link-rewrite",
      "name": "Link Rewrite (YOURLS)",
      "version": "0.3.1",
      "api_version": "1",
      "kind": "processor",
      "runtime": "builtin",
      "enabled": true,
      "hooks": ["on_render"],
      "instances": [
        {
          "id": 1,
          "name": "Personal sho.rt",
          "enabled": true
        }
      ]
    }
  ]
}
```

#### Example Prompt
> *"List the installed FeedCourier plugins and see which link-rewrite provider instances are currently enabled."*

---

### 3. `inspect_failed_deliveries` {#3-inspect-failed-deliveries}

Retrieves delivery tasks along with their route, channel account, destination (`destination_id`, `destination_name`), attempt count, last status code, and diagnostic error log.

#### Input Parameters
| Parameter | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `status` | `string?` | Optional | Filter by `"dead_letter"` (default), `"retrying"`, `"deferred"`, `"queued"`, or `"all"`. |
| `limit` | `u32?` | Optional | Maximum number of tasks to return (default `20`, range `1..=100`). |

#### Example Call
```json
{
  "name": "inspect_failed_deliveries",
  "arguments": {
    "status": "dead_letter",
    "limit": 20
  }
}
```

#### Example Prompt
> *"Inspect the latest dead-letter delivery tasks and summarize why they failed."*

---

### 4. `preview_feed_latest` {#4-preview-feed-latest}

Fetches and parses a feed live without writing to the database. Returns up to `10` latest entries, optionally runs full-text extraction on up to the top `3` entries when `fetch_full_content: true`, and tests optional keyword or regex filters.

#### Input Parameters
| Parameter | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `url` | `string?` | Conditional | Direct `http://` or `https://` feed URL to preview (provide `url` or `feed_id`). |
| `feed_id` | `i64?` | Conditional | Existing feed ID (uses its saved `url`, `fetch_full_content`, and `use_proxy` settings unless overridden). |
| `fetch_full_content` | `bool?` | Optional | Test full-article extraction during the preview. |
| `use_proxy` | `bool?` | Optional | Use the configured outbound proxy during the preview. |
| `include_keywords` | `string[]?` | Optional | Case-insensitive keywords where at least one must match. |
| `exclude_keywords` | `string[]?` | Optional | Case-insensitive keywords that exclude matching items. |
| `include_regex` | `string?` | Optional | Regular expression required to match. |
| `exclude_regex` | `string?` | Optional | Regular expression that excludes matching items. |

#### Example Call
```json
{
  "name": "preview_feed_latest",
  "arguments": {
    "url": "https://blog.rust-lang.org/feed.xml",
    "fetch_full_content": true,
    "include_keywords": ["Rust", "Announcing"],
    "exclude_keywords": ["Call for Testing"]
  }
}
```

#### Example Prompt
> *"Preview `https://blog.rust-lang.org/feed.xml` with full-content extraction enabled, and check which recent articles match the keyword `Announcing`."*

---

## Configuration & Operational Tools (`admin` Scope Only)

::: warning Requires `admin` Token Scope
Calling any of the six tools below with a `readonly` token returns an `"insufficient_scope"` error without making changes.
:::

### 5. `manage_feed` {#5-manage-feed}

Adds, updates, pauses, resumes, clears the cache of, or deletes a feed subscription. On `"add"`, FeedCourier performs an initial poll right away (with `silent_init: true` enabled by default so existing historical articles are recorded without flooding your channels). If the initial request is blocked by private network protection, the new feed record is rolled back automatically.

#### Input Parameters
| Parameter | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `action` | `string` | **Yes** | `"add"`, `"update"`, `"pause"`, `"resume"`, `"clear_cache"`, or `"delete"`. |
| `feed_id` | `i64?` | For non-`add` | Target feed ID (required for `"update"`, `"pause"`, `"resume"`, `"clear_cache"`, `"delete"`). |
| `url` | `string?` | For `add` | Feed URL (`http://` or `https://`). |
| `title` | `string?` | Optional | Custom feed title override. |
| `tags` | `string[]?` | Optional | Custom tags for route matching and Notion `Tag` property sync. |
| `poll_interval_secs` | `i64?` | Optional | Polling interval in seconds (default `1800`, must be `> 0`). |
| `fetch_full_content` | `bool?` | Optional | Enable full-article extraction for this feed (`false` by default). |
| `use_proxy` | `bool?` | Optional | Route feed and article requests through the global outbound proxy (`false` by default). |
| `silent_init` | `bool?` | Optional | On `"add"`, record existing historical articles silently (`true` by default). |
| `push_latest_one` | `bool?` | Optional | On `"add"` when `silent_init` is `true`, still enqueue the single newest entry (`false` by default). |
| `poll_now` | `bool?` | Optional | On `"clear_cache"`, poll the feed immediately after clearing (`true` by default). |

#### Example Call (`add`)
```json
{
  "name": "manage_feed",
  "arguments": {
    "action": "add",
    "url": "https://tokio.rs/blog/index.xml",
    "tags": ["rust", "async"],
    "poll_interval_secs": 1800,
    "fetch_full_content": true,
    "silent_init": true,
    "push_latest_one": true
  }
}
```

#### Example Call (`clear_cache`)
```json
{
  "name": "manage_feed",
  "arguments": {
    "action": "clear_cache",
    "feed_id": 4,
    "poll_now": true
  }
}
```

`clear_cache` deletes all entries, fingerprints, and delivery tasks of the feed, clears its `ETag` / `Last-Modified` and failure state, and disables silent init, so the next poll queues the full backlog oldest-first. Already delivered articles can be delivered again.

#### Example Prompt
> *"Add `https://tokio.rs/blog/index.xml` with tags `rust` and `async`, enable full-text extraction, and push the latest 1 article."*

---

### 6. `configure_route` {#6-configure-route}

Creates, updates, or deletes a delivery route that connects feeds/tags and content filters to an existing trusted `destination_id` created in the WebUI (discoverable via `list_feeds_status`). For backward compatibility, `channel_id` is still accepted if that channel account owns **exactly one** destination; if the account has multiple destinations, an error is returned asking for `destination_id`.

#### Input Parameters
| Parameter | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `action` | `string` | **Yes** | `"create"`, `"update"`, or `"delete"`. |
| `route_id` | `i64?` | For `update`/`delete` | Target route ID. |
| `name` | `string?` | For `create` | Route name. |
| `destination_id` | `i64?` | For `create` (primary) | Integer ID of an existing trusted destination created in the WebUI (required for `"create"` unless a single-destination `channel_id` is passed). |
| `channel_id` | `i64?` | Legacy fallback | Integer ID of a Channel Account; automatically resolves only when that account owns exactly 1 destination. |
| `feed_ids` | `i64[]?` | Optional | Explicit feed IDs matched by this route (`[]` with empty `match_tags` matches all feeds). |
| `match_tags` | `string[]?` | Optional | Feed tags matched case-insensitively by this route. |
| `include_keywords` | `string[]?` | Optional | Case-insensitive keywords where at least one must match. |
| `exclude_keywords` | `string[]?` | Optional | Case-insensitive keywords that exclude matching entries. |
| `include_regex` | `string?` | Optional | Regular expression required to match (`""` clears on `"update"`). |
| `exclude_regex` | `string?` | Optional | Regular expression that excludes matching entries (`""` clears on `"update"`). |
| `telegram_mode` | `string?` | Optional | Telegram mode: `"rich_message"` (default), `"compact_card"`, or `"media_mode"`. |
| `link_preview` | `bool?` | Optional | Enable link preview in compact/text Telegram messages (`true` by default). |
| `locale` | `string?` | Optional | Delivery language of this route: `"en"` or `"zh-CN"`. |
| `timezone` | `string?` | Optional | Optional per-route IANA timezone override (e.g., `"Asia/Shanghai"`; `""` clears on `"update"` to inherit the system timezone). Governs delivery windows, SMTP article timestamps, and Telegram rich-message footer timestamps. |
| `active_windows` | `{start, end}[]?` | Optional | Daily delivery windows in the **route timezone** (inheriting system timezone) with DST-safe boundaries, e.g. `[{"start":"08:00","end":"22:00"}]`. `end` earlier than `start` wraps midnight; only `00:00`–`00:00` (or `[]`) means all day—other `start == end` windows are rejected with a validation error. |
| `active_weekdays` | `u8[]?` | Optional | Allowed weekdays `1` (Mon) – `7` (Sun); `[]` means every day. |
| `plugins` | `Record<string, object>?` | Optional | Per-plugin route options keyed by plugin ID (for example, `{"link-rewrite": {"provider_id": 1}}`). Validated against installed plugins and existing provider instances before saving; pass `{}` or `{"link-rewrite": {"provider_id": null}}` to disable. Discover valid IDs with [`list_plugins`](#2-list-plugins). |
| `enabled` | `bool?` | Optional | Whether this route is active (`true` by default). |

#### Example Call (`create`)
```json
{
  "name": "configure_route",
  "arguments": {
    "action": "create",
    "name": "Rust Feeds -> Telegram Rich",
    "destination_id": 5,
    "match_tags": ["rust"],
    "telegram_mode": "rich_message",
    "link_preview": true,
    "locale": "en",
    "timezone": "America/New_York",
    "active_windows": [{ "start": "08:00", "end": "22:00" }],
    "active_weekdays": [1, 2, 3, 4, 5],
    "plugins": {
      "link-rewrite": { "provider_id": 1 }
    }
  }
}
```

Tasks outside a route's window or weekdays are still queued and are delivered at the next window start. On `"update"`, changing the target destination atomically repoints pending `queued`/`deferred`/`retrying` tasks (clearing checkpoints and resetting `attempt_count = 0`; active `delivering` tasks are untouched), and changing `active_windows`, `active_weekdays`, or `timezone` wakes `queued`/`deferred` tasks immediately.

#### Example Prompt
> *"Create a route sending all feeds with the `rust` tag to Telegram destination #5 in `rich_message` mode, using `America/New_York` timezone on weekdays between 08:00 and 22:00."*

---

### 7. `reset_and_poll_feed` {#7-reset-and-poll-feed}

Resets a feed's consecutive failure counter, restores a `circuit_open` or `backoff` feed back to `active`, and runs an immediate poll.

#### Input Parameters
| Parameter | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `feed_id` | `i64` | **Yes** | ID of the feed to reset and poll immediately. |

#### Example Call
```json
{
  "name": "reset_and_poll_feed",
  "arguments": {
    "feed_id": 4
  }
}
```

#### Example Prompt
> *"Reset feed #4 and poll it right now."*

---

### 8. `retry_dead_tasks` {#8-retry-dead-tasks}

Requeues specific failed tasks (`task_ids`) or all `dead_letter` tasks whose article bodies are still within the retention window (`all_dead_letter: true`), setting their status back to `queued` for immediate delivery.

#### Input Parameters
| Parameter | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `task_ids` | `i64[]?` | Conditional | Specific task IDs to requeue immediately. |
| `all_dead_letter` | `bool?` | Conditional | Set `true` to requeue all `dead_letter` tasks with retained article content. |

#### Example Call
```json
{
  "name": "retry_dead_tasks",
  "arguments": {
    "all_dead_letter": true
  }
}
```

#### Example Prompt
> *"Retry all dead-letter delivery tasks."*

---

### 9. `manage_outbox_queue` {#9-manage-outbox-queue}

Reads or changes the global delivery queue state. While paused, feeds are still polled and tasks accumulate; on resume all due tasks across feeds are delivered oldest-first by article publish time (falling back to discovery time).

#### Input Parameters
| Parameter | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `action` | `string` | **Yes** | `"status"`, `"pause"`, or `"resume"`. |
| `resume_in_minutes` | `u32?` | Optional | With `"pause"`: automatically resume after this many minutes. |
| `resume_at` | `string?` | Optional | With `"pause"`: automatically resume at this RFC 3339 timestamp. |

Without `resume_in_minutes` or `resume_at`, a pause lasts until `"resume"` is called.

#### Example Call
```json
{
  "name": "manage_outbox_queue",
  "arguments": {
    "action": "pause",
    "resume_in_minutes": 60
  }
}
```

#### Example Prompt
> *"Pause the delivery queue for one hour."*

---

### 10. `test_push_feed_entry` {#10-test-push-feed-entry}

Debug push: fetches a feed entry live and delivers it to a trusted destination once. It is **stateless**: it creates no fingerprints, does not enter the queue, and does not use route filters, time windows, or quotas. Notion pages use a `test-` prefixed `FC-ID`. Downstream failures are returned as a diagnostic result instead of an error.

#### Input Parameters
| Parameter | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `destination_id` | `i64?` | **Primary** | Trusted WebUI-configured delivery destination ID (required unless a single-destination `channel_id` is passed). |
| `channel_id` | `i64?` | Legacy fallback | Channel account ID; accepted only when that account owns exactly 1 destination. |
| `feed_id` | `i64?` | Conditional | Existing feed to read from (provide `feed_id` or `url`). |
| `url` | `string?` | Conditional | Direct feed URL. |
| `fingerprint_sha256` | `string?` | Optional | Entry to push; omit to push the latest item. |
| `fetch_full_content` | `bool?` | Optional | Extract the full article before pushing. |
| `use_proxy` | `bool?` | Optional | Use the configured outbound proxy when fetching. |
| `telegram_mode` | `string?` | Optional | `"rich_message"`, `"compact_card"`, or `"media_mode"` (Telegram destinations). |
| `link_preview` | `bool?` | Optional | Telegram link preview toggle. |
| `locale` | `string?` | Optional | Delivery language: `"en"` or `"zh-CN"`. |
| `timezone` | `string?` | Optional | Optional IANA timezone for article timestamps (inherits system timezone when omitted). |

#### Example Call
```json
{
  "name": "test_push_feed_entry",
  "arguments": {
    "destination_id": 5,
    "feed_id": 4,
    "telegram_mode": "media_mode",
    "locale": "en",
    "timezone": "Asia/Shanghai"
  }
}
```

#### Example Prompt
> *"Push the latest item of feed #4 to Telegram destination #5 in media mode as a test."*
