# MCP Tools Reference

FeedCourier exposes **7 structured MCP tools** (`src/mcp/tools.rs`) across two permission scopes (`readonly` and `admin`). All tool responses return structured JSON, with untrusted external strings wrapped in `<untrusted_feed_content>` tags.

---

## Summary Matrix

| Tool Name | Minimum Scope | Mutates State? | Outbound Network? | Purpose |
| :--- | :--- | :--- | :--- | :--- |
| [`list_feeds_status`](#1-list-feeds-status) | `readonly` | No | No | Gateway overview metrics, redacted channel IDs, active routes, and feed health. |
| [`inspect_failed_deliveries`](#2-inspect-failed-deliveries) | `readonly` | No | No | Inspect outbox tasks in `dead_letter`, `retrying`, `deferred`, `queued`, or `all` states. |
| [`preview_feed_latest`](#3-preview-feed-latest) | `readonly` | No | Yes *(SSRF-guarded)* | Live-fetch up to 10 items from a URL or `feed_id` to test Readability and content filters. |
| [`manage_feed`](#4-manage-feed) | `admin` | Yes | Yes *(on `add`)* | Add, update, pause, resume, or delete a feed subscription. |
| [`configure_route`](#5-configure-route) | `admin` | Yes | No | Create, update, or delete a delivery route targeting an existing trusted `channel_id`. |
| [`reset_and_poll_feed`](#6-reset-and-poll-feed) | `admin` | Yes | Yes *(SSRF-guarded)* | Reset circuit-breaker / failure counters on a feed and trigger an immediate poll. |
| [`retry_dead_tasks`](#7-retry-dead-tasks) | `admin` | Yes | No *(wakes worker)* | Requeue dead-letter or failed delivery tasks by `task_ids` or `all_dead_letter: true`. |

---

## Read-Only Inspection & Preview Tools (`readonly` + `admin`)

### 1. `list_feeds_status`

Returns gateway-wide counters (`overview`), secret-free channel summaries (`id`, `name`, `kind`, `enabled`, `cooldown_until`), configured routes, and subscribed feeds.

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

---

### 2. `inspect_failed_deliveries`

Retrieves outbox tasks from `delivery_tasks` along with their `attempt_count`, decoded `checkpoint` JSON, `last_status_code`, and `diagnostic_log`.

#### Input Parameters
| Parameter | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `status` | `string?` | Optional | Filter by `"dead_letter"` (default), `"retrying"`, `"deferred"`, `"queued"`, or `"all"`. |
| `limit` | `u32?` | Optional | Maximum rows to return (default `20`, clamped to `1..=100`). |

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

---

### 3. `preview_feed_latest`

Fetches and parses a feed live through `ProtectedHttpClient` without writing to SQLite. Returns up to `10` latest entries (`PREVIEW_MAX_ITEMS = 10`), runs `dom_smoothie` Readability on up to the top `3` items when `fetch_full_content: true`, and evaluates optional content filter rules.

#### Input Parameters
| Parameter | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `url` | `string?` | Conditional | Direct `http://` or `https://` feed URL to preview (provide `url` or `feed_id`). |
| `feed_id` | `i64?` | Conditional | Existing feed ID (inherits saved `url`, `fetch_full_content`, and `use_proxy` unless overridden). |
| `fetch_full_content` | `bool?` | Optional | Override on-demand Readability extraction during preview. |
| `use_proxy` | `bool?` | Optional | Override outbound proxy usage during preview. |
| `include_keywords` | `string[]?` | Optional | Case-insensitive keywords where at least one must match. |
| `exclude_keywords` | `string[]?` | Optional | Case-insensitive keywords that reject matching items. |
| `include_regex` | `string?` | Optional | Bounded regular expression required to match. |
| `exclude_regex` | `string?` | Optional | Bounded regular expression that rejects matching items. |

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

---

## Orchestration & Operational Tools (`admin` Scope Only)

::: warning Requires `admin` Token Scope
Calling any of the four tools below with a `readonly` token returns a structured `"insufficient_scope"` error without executing the operation.
:::

### 4. `manage_feed`

Creates, updates, pauses, resumes, or deletes a feed subscription. On `"add"`, FeedCourier performs an immediate initial poll (using `silent_init: true` by default so historical entries are fingerprinted without flooding channels). If the initial poll is blocked by the SSRF policy, the newly created feed row is automatically rolled back.

#### Input Parameters
| Parameter | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `action` | `string` | **Yes** | `"add"`, `"update"`, `"pause"`, `"resume"`, or `"delete"`. |
| `feed_id` | `i64?` | For non-`add` | Target feed ID (required for `"update"`, `"pause"`, `"resume"`, `"delete"`). |
| `url` | `string?` | For `add` | Feed URL (`http://` or `https://`). |
| `title` | `string?` | Optional | Custom feed title override. |
| `tags` | `string[]?` | Optional | Custom tags for route matching and Notion `Tag` property sync. |
| `poll_interval_secs` | `i64?` | Optional | Polling interval in seconds (default `1800`, must be `> 0`). |
| `fetch_full_content` | `bool?` | Optional | Enable per-feed `dom_smoothie` Readability full-article extraction (`false` by default). |
| `use_proxy` | `bool?` | Optional | Route feed/article fetches through the configured global proxy (`false` by default). |
| `silent_init` | `bool?` | Optional | On `"add"`, absorb initial historical entries silently (`true` by default). |
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

---

### 5. `configure_route`

Creates, updates, or deletes a delivery route binding feeds/tags and content filters to an existing WebUI-managed `channel_id`.

#### Input Parameters
| Parameter | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `action` | `string` | **Yes** | `"create"`, `"update"`, or `"delete"`. |
| `route_id` | `i64?` | For `update`/`delete` | Target route ID. |
| `name` | `string?` | For `create` | Human-readable route name. |
| `channel_id` | `i64?` | For `create` | Integer ID of an existing trusted channel created in the WebUI. |
| `feed_ids` | `i64[]?` | Optional | Explicit feed IDs matched by this route (`[]` with empty `match_tags` matches all feeds). |
| `match_tags` | `string[]?` | Optional | Feed tags matched case-insensitively by this route. |
| `include_keywords` | `string[]?` | Optional | Case-insensitive keywords where at least one must match. |
| `exclude_keywords` | `string[]?` | Optional | Case-insensitive keywords that reject matching entries. |
| `include_regex` | `string?` | Optional | Bounded regular expression required to match (`""` clears on `"update"`). |
| `exclude_regex` | `string?` | Optional | Bounded regular expression that rejects matching entries (`""` clears on `"update"`). |
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

---

### 6. `reset_and_poll_feed`

Clears `consecutive_failures`, resets a `circuit_open` or `backoff` feed back to `active`, triggers an immediate incremental poll, and wakes the outbox dispatcher if new tasks are enqueued.

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

---

### 7. `retry_dead_tasks`

Requeues specific failed/dead-letter tasks (`task_ids`) or all `dead_letter` tasks whose entry payloads are still retained (`all_dead_letter: true`), resetting their status to `queued` and waking the outbox worker immediately.

#### Input Parameters
| Parameter | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `task_ids` | `i64[]?` | Conditional | Specific task IDs to requeue immediately. |
| `all_dead_letter` | `bool?` | Conditional | Set `true` to requeue all `dead_letter` tasks with retained entry payloads. |

#### Example Call
```json
{
  "name": "retry_dead_tasks",
  "arguments": {
    "all_dead_letter": true
  }
}
```
