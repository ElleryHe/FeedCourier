---
description: "Reference for FeedCourier's 7 MCP tools covering feed status inspection, article preview, failed delivery diagnostics, feed management, and route configuration."
---

# Tools Overview

FeedCourier exposes **7 structured MCP tools** across two permission scopes (`readonly` and `admin`). All tools return structured JSON responses.

---

## Tool Summary

| Tool Name | Minimum Scope | Modifies State? | Purpose |
| :--- | :--- | :--- | :--- |
| [`list_feeds_status`](#1-list-feeds-status) | `readonly` | No | View system overview counters, channel summaries (ID and name only), active routes, and feed health. |
| [`inspect_failed_deliveries`](#2-inspect-failed-deliveries) | `readonly` | No | Inspect delivery tasks in `dead_letter`, `retrying`, `deferred`, `queued`, or `all` states. |
| [`preview_feed_latest`](#3-preview-feed-latest) | `readonly` | No | Fetch up to 10 recent entries from a URL or `feed_id` to test full-text extraction and content filters. |
| [`manage_feed`](#4-manage-feed) | `admin` | Yes | Add, update, pause, resume, or delete a feed subscription. |
| [`configure_route`](#5-configure-route) | `admin` | Yes | Create, update, or delete a delivery route linked to an existing `channel_id`. |
| [`reset_and_poll_feed`](#6-reset-and-poll-feed) | `admin` | Yes | Reset failure counters on a feed and trigger an immediate poll. |
| [`retry_dead_tasks`](#7-retry-dead-tasks) | `admin` | Yes | Requeue failed delivery tasks by `task_ids` or `all_dead_letter: true`. |

---

## Read-Only Inspection & Preview Tools (`readonly` + `admin`)

### 1. `list_feeds_status` {#1-list-feeds-status}

Returns system-wide counters (`overview`), channel summaries (`id`, `name`, `kind`, `enabled`, `cooldown_until`—without secrets or recipient addresses), configured routes, and subscribed feeds.

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

### 2. `inspect_failed_deliveries` {#2-inspect-failed-deliveries}

Retrieves delivery tasks along with their attempt count, last status code, and diagnostic error log.

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

### 3. `preview_feed_latest` {#3-preview-feed-latest}

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
Calling any of the four tools below with a `readonly` token returns an `"insufficient_scope"` error without making changes.
:::

### 4. `manage_feed` {#4-manage-feed}

Adds, updates, pauses, resumes, or deletes a feed subscription. On `"add"`, FeedCourier performs an initial poll right away (with `silent_init: true` enabled by default so existing historical articles are recorded without flooding your channels). If the initial request is blocked by private network protection, the new feed record is rolled back automatically.

#### Input Parameters
| Parameter | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `action` | `string` | **Yes** | `"add"`, `"update"`, `"pause"`, `"resume"`, or `"delete"`. |
| `feed_id` | `i64?` | For non-`add` | Target feed ID (required for `"update"`, `"pause"`, `"resume"`, `"delete"`). |
| `url` | `string?` | For `add` | Feed URL (`http://` or `https://`). |
| `title` | `string?` | Optional | Custom feed title override. |
| `tags` | `string[]?` | Optional | Custom tags for route matching and Notion `Tag` property sync. |
| `poll_interval_secs` | `i64?` | Optional | Polling interval in seconds (default `1800`, must be `> 0`). |
| `fetch_full_content` | `bool?` | Optional | Enable full-article extraction for this feed (`false` by default). |
| `use_proxy` | `bool?` | Optional | Route feed and article requests through the global outbound proxy (`false` by default). |
| `silent_init` | `bool?` | Optional | On `"add"`, record existing historical articles silently (`true` by default). |
| `push_latest_one` | `bool?` | Optional | On `"add"` when `silent_init` is `true`, still enqueue the single newest entry (`false` by default). |

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

#### Example Prompt
> *"Add `https://tokio.rs/blog/index.xml` with tags `rust` and `async`, enable full-text extraction, and push the latest 1 article."*

---

### 5. `configure_route` {#5-configure-route}

Creates, updates, or deletes a delivery route that connects feeds/tags and content filters to an existing `channel_id` created in the WebUI.

#### Input Parameters
| Parameter | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `action` | `string` | **Yes** | `"create"`, `"update"`, or `"delete"`. |
| `route_id` | `i64?` | For `update`/`delete` | Target route ID. |
| `name` | `string?` | For `create` | Route name. |
| `channel_id` | `i64?` | For `create` | Integer ID of an existing delivery channel created in the WebUI. |
| `feed_ids` | `i64[]?` | Optional | Explicit feed IDs matched by this route (`[]` with empty `match_tags` matches all feeds). |
| `match_tags` | `string[]?` | Optional | Feed tags matched case-insensitively by this route. |
| `include_keywords` | `string[]?` | Optional | Case-insensitive keywords where at least one must match. |
| `exclude_keywords` | `string[]?` | Optional | Case-insensitive keywords that exclude matching entries. |
| `include_regex` | `string?` | Optional | Regular expression required to match (`""` clears on `"update"`). |
| `exclude_regex` | `string?` | Optional | Regular expression that excludes matching entries (`""` clears on `"update"`). |
| `telegram_mode` | `string?` | Optional | Telegram mode: `"rich_message"` (default), `"compact_card"`, or `"media_mode"`. |
| `link_preview` | `bool?` | Optional | Enable link preview in compact/text Telegram messages (`true` by default). |
| `enabled` | `bool?` | Optional | Whether this route is active (`true` by default). |

#### Example Call (`create`)
```json
{
  "name": "configure_route",
  "arguments": {
    "action": "create",
    "name": "Rust Feeds -> Telegram Rich",
    "channel_id": 2,
    "match_tags": ["rust"],
    "telegram_mode": "rich_message",
    "link_preview": true
  }
}
```

#### Example Prompt
> *"Create a route sending all feeds with the `rust` tag to Telegram channel #2 in `rich_message` mode."*

---

### 6. `reset_and_poll_feed` {#6-reset-and-poll-feed}

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

### 7. `retry_dead_tasks` {#7-retry-dead-tasks}

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
