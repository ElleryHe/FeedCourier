/**
 * Canonical REST API Contract (`/api/v1`) for FeedCourier.
 *
 * Aligned 1-to-1 with `src/channels/mod.rs` and `src/db/models.rs`:
 * - `ChannelTargetCreate` deserializes directly into `crate::channels::ChannelConfig`
 * - `ChannelTargetView` matches `ChannelConfig::redacted_view()`
 * - `ChannelCapabilities` matches `crate::channels::TelegramCapabilities`
 * - `RouteOptions` matches `crate::channels::RouteOptions`
 * - `DeliveryCheckpoint` matches `crate::channels::DeliveryCheckpoint`
 * - Timestamps are ISO 8601 / RFC 3339 UTC strings (`unix_to_rfc3339`), with UI helpers
 *   also accepting Unix epoch seconds (`number`) defensively.
 */

// ---------------------------------------------------------------------------
// Common & Error Envelope
// ---------------------------------------------------------------------------

/** Standard JSON error body returned by Axum `AppError` on non-2xx responses. */
export interface ApiErrorBody {
  error: string;
  code?: string;
  details?: string;
}

export interface ActionAckResponse {
  ok: boolean;
}

// ---------------------------------------------------------------------------
// 1. Auth & Compliance Metadata (`/api/v1/auth/*`)
// ---------------------------------------------------------------------------

export const DEFAULT_REPO_URL = "https://github.com/ElleryHe/FeedCourier";
export const DEFAULT_LICENSE = "AGPL-3.0-only";
export const DEFAULT_VERSION = "0.1.0";

/** `GET /api/v1/auth/status` */
export interface AuthStatusResponse {
  /** True when no admin password hash exists yet (neither `ADMIN_PASSWORD` env nor DB). */
  setup_required: boolean;
  /** True when the current HttpOnly session cookie is valid. */
  authenticated: boolean;
  /** Build version (e.g. `"0.1.0"`). */
  version: string;
  /** Source repository URL (AGPLv3 Section 13 compliance). */
  repo_url: string;
  /** SPDX license identifier (`"AGPL-3.0-only"`). */
  license: string;
}

/** `POST /api/v1/auth/setup` & `POST /api/v1/auth/login` */
export interface AuthPasswordRequest {
  password: string;
}

// ---------------------------------------------------------------------------
// 2. Overview & Maintenance (`/api/v1/overview`, `/api/v1/maintenance/*`)
// ---------------------------------------------------------------------------

export type FeedHealthStatus = "active" | "backoff" | "circuit_open" | "paused";

export type ChannelKind = "smtp" | "telegram" | "notion";

export type TaskStatus =
  | "queued"
  | "delivering"
  | "delivered"
  | "retrying"
  | "deferred"
  | "dead_letter";

export interface FeedHealthCounts {
  active: number;
  backoff: number;
  circuit_open: number;
  paused: number;
}

export interface EntryCounts {
  /** Total permanent SHA-256 dedup fingerprints stored in `entries`. */
  total_fingerprints: number;
  /** Entries whose transient HTML/media payload has NOT yet been purged by GC. */
  active_payloads: number;
}

export interface TaskStatusCounts {
  queued: number;
  delivering: number;
  retrying: number;
  deferred: number;
  dead_letter: number;
  delivered: number;
}

export interface ChannelCooldownSummary {
  channel_id: number;
  channel_name: string;
  kind: ChannelKind;
  /** ISO 8601 UTC timestamp when rate-limit / downstream cooldown expires. */
  cooldown_until: string;
}

/** `GET /api/v1/overview` */
export interface OverviewResponse {
  feeds: FeedHealthCounts;
  entries: EntryCounts;
  tasks: TaskStatusCounts;
  active_cooldowns: ChannelCooldownSummary[];
}

/** `POST /api/v1/maintenance/gc` */
export interface GcResponse {
  purged_entries: number;
}

// Note: `GET /api/v1/maintenance/backup` streams binary `application/octet-stream`
// produced via SQLite `VACUUM INTO`.

// ---------------------------------------------------------------------------
// 3. Feeds & OPML (`/api/v1/feeds*`)
// ---------------------------------------------------------------------------

export interface FeedDto {
  id: number;
  url: string;
  title: string;
  site_url: string | null;
  favicon_url: string | null;
  tags: string[];
  poll_interval_secs: number;
  fetch_full_content: boolean;
  use_proxy: boolean;
  silent_init: boolean;
  health_status: FeedHealthStatus;
  consecutive_failures: number;
  next_poll_at: string | null;
  last_error: string | null;
  created_at: string;
  updated_at: string;
}

/** `POST /api/v1/feeds` */
export interface CreateFeedRequest {
  url: string;
  title?: string | null;
  tags?: string[];
  poll_interval_secs?: number;
  fetch_full_content?: boolean;
  use_proxy?: boolean;
  silent_init?: boolean;
  /**
   * Optional convenience flag (`feedcourier.md` §四.2): when `silent_init` is true,
   * still enqueue the single newest entry to verify end-to-end delivery.
   */
  push_latest_one?: boolean;
}

/** `PATCH /api/v1/feeds/:id` */
export interface UpdateFeedRequest {
  url?: string;
  title?: string | null;
  tags?: string[];
  poll_interval_secs?: number;
  fetch_full_content?: boolean;
  use_proxy?: boolean;
  silent_init?: boolean;
  /** Allows pausing (`"paused"`) or resuming (`"active"`) polling. */
  health_status?: "active" | "paused";
}

/** `POST /api/v1/feeds/:id/poll` (resets circuit breaker & polls immediately) */
export interface PollFeedResponse {
  feed: FeedDto;
  new_entries: number;
  enqueued_tasks: number;
  not_modified: boolean;
}

/** Reusable content filter rule set (shared by Feed Preview and Routes). */
export interface ContentFilterRules {
  include_keywords: string[];
  exclude_keywords: string[];
  include_regex: string | null;
  exclude_regex: string | null;
}

/** `POST /api/v1/feeds/preview` */
export interface FeedPreviewRequest {
  url: string;
  fetch_full_content: boolean;
  use_proxy: boolean;
  filter?: ContentFilterRules;
}

export interface FeedPreviewItem {
  fingerprint_sha256: string;
  title: string;
  canonical_url: string | null;
  author: string | null;
  published_at: string | null;
  first_sentence: string | null;
  summary_text: string | null;
  content_html_preview: string | null;
  media_urls: string[];
  full_content_extracted: boolean;
  matched_filter: boolean;
  filter_reason: string | null;
}

export interface FeedPreviewResponse {
  feed_title: string | null;
  site_url: string | null;
  favicon_url: string | null;
  items: FeedPreviewItem[];
}

/** `POST /api/v1/feeds/opml/import` */
export interface OpmlImportRequest {
  opml_xml: string;
  silent_init: boolean;
}

export interface OpmlImportResponse {
  imported_count: number;
  skipped_existing_count: number;
  feeds: FeedDto[];
}

// Note: `GET /api/v1/feeds/opml/export` returns OPML 2.0 XML (`application/xml`).

// ---------------------------------------------------------------------------
// 4. Trusted Channels (`/api/v1/channels*`)
// ---------------------------------------------------------------------------

/** Matches `crate::channels::SmtpSecurity` (`#[serde(rename_all = "snake_case")]`). */
export type SmtpSecurity = "start_tls" | "tls" | "plain";

/**
 * Standard 7-column Notion data source mapping (`crate::channels::NotionColumnMapping`).
 * `title` and `fc_id` are strictly required; optional columns may be `null` to skip.
 */
export interface NotionColumnMapping {
  /** Required (`title`): article title (standard name: `"Title"`). */
  title: string;
  /** Required (`rich_text`): SHA-256 fingerprint for idempotency reconciliation (standard name: `"FC-ID"`). */
  fc_id: string;
  /** Optional (`url`): canonical article link (standard name: `"Link"`). */
  link: string | null;
  /** Optional (`select`): feed source title (standard name: `"Source"`). */
  source: string | null;
  /** Optional (`date`): published timestamp (standard name: `"Published"`). */
  published: string | null;
  /** Optional (`select`): `"UnRead"` / `"Read"` status (standard name: `"State"`). */
  state: string | null;
  /** Optional (`multi_select`): synced feed tags (standard name: `"Tag"`). */
  tag: string | null;
}

export const DEFAULT_NOTION_COLUMN_MAPPING: NotionColumnMapping = {
  title: "Title",
  fc_id: "FC-ID",
  link: "Link",
  source: "Source",
  published: "Published",
  state: "State",
  tag: "Tag",
};

/**
 * Masked, secret-safe channel target view returned on `GET /api/v1/channels`.
 * Matches `crate::channels::ChannelConfig::redacted_view()` 1-to-1!
 * Raw secrets (`password`, `bot_token`, `integration_secret`) are NEVER returned.
 */
export type ChannelTargetView =
  | {
      kind: "smtp";
      host: string;
      port: number;
      username: string | null;
      has_password: boolean;
      security: SmtpSecurity;
      from_address: string;
      to_addresses: string[];
      reply_to: string | null;
    }
  | {
      kind: "telegram";
      /** Masked hint from `mask_secret(&cfg.bot_token)`, e.g. `"1234••••wxyz"`. */
      bot_token_hint: string;
      chat_id: string;
      message_thread_id: number | null;
      api_base_url: string | null;
    }
  | {
      kind: "notion";
      /** Masked hint from `mask_secret(&cfg.integration_secret)`, e.g. `"ntn_••••abcd"`. */
      secret_hint: string;
      data_source_id: string;
      database_id: string | null;
      parent_page_id: string | null;
      column_mapping: NotionColumnMapping;
    };

/**
 * Create payload for channel target (`POST /api/v1/channels`).
 * Deserializes directly into `crate::channels::ChannelConfig` (`#[serde(tag = "kind", rename_all = "snake_case")]`)!
 */
export type ChannelTargetCreate =
  | {
      kind: "smtp";
      host: string;
      port: number;
      username?: string | null;
      password?: string | null;
      security: SmtpSecurity;
      from_address: string;
      to_addresses: string[];
      reply_to?: string | null;
    }
  | {
      kind: "telegram";
      bot_token: string;
      chat_id: string;
      message_thread_id?: number | null;
      api_base_url?: string | null;
    }
  | {
      kind: "notion";
      integration_secret: string;
      data_source_id: string;
      database_id?: string | null;
      parent_page_id?: string | null;
      column_mapping: NotionColumnMapping;
    };

/**
 * Patch payload for channel target (`PATCH /api/v1/channels/:id`).
 * Secret fields (`password`, `bot_token`, `integration_secret`) are optional:
 * when omitted or `null`/empty, the backend preserves the stored encrypted secret.
 */
export type ChannelTargetPatch =
  | {
      kind: "smtp";
      host?: string;
      port?: number;
      username?: string | null;
      /** Omit or pass `null` to keep existing SMTP password. */
      password?: string | null;
      security?: SmtpSecurity;
      from_address?: string;
      to_addresses?: string[];
      reply_to?: string | null;
    }
  | {
      kind: "telegram";
      /** Omit or pass `null` to keep existing Bot token. */
      bot_token?: string | null;
      chat_id?: string;
      message_thread_id?: number | null;
      api_base_url?: string | null;
    }
  | {
      kind: "notion";
      /** Omit or pass `null` to keep existing Notion integration secret. */
      integration_secret?: string | null;
      data_source_id?: string;
      database_id?: string | null;
      parent_page_id?: string | null;
      column_mapping?: NotionColumnMapping;
    };

/** Matches `crate::channels::TelegramCapabilities` stored in `channels.capabilities_json`. */
export interface ChannelCapabilities {
  supports_rich_message?: boolean;
  bot_api_version?: string | null;
  bot_username?: string | null;
  probed_at?: number | string | null;
  /** Populated when target Bot API lacks `sendRichMessage` and falls back to Compact Card. */
  fallback_reason?: string | null;
}

export interface ChannelDto {
  id: number;
  name: string;
  kind: ChannelKind;
  config: ChannelTargetView;
  capabilities: ChannelCapabilities;
  min_interval_secs: number;
  jitter_max_secs: number;
  hourly_quota: number | null;
  daily_quota: number | null;
  use_proxy: boolean;
  cooldown_until: string | null;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

/** `POST /api/v1/channels` */
export interface CreateChannelRequest {
  name: string;
  kind: ChannelKind;
  config: ChannelTargetCreate;
  min_interval_secs: number;
  jitter_max_secs: number;
  hourly_quota: number | null;
  daily_quota: number | null;
  use_proxy: boolean;
  enabled: boolean;
}

/** `PATCH /api/v1/channels/:id` */
export interface UpdateChannelRequest {
  name?: string;
  config?: ChannelTargetPatch;
  min_interval_secs?: number;
  jitter_max_secs?: number;
  hourly_quota?: number | null;
  daily_quota?: number | null;
  use_proxy?: boolean;
  enabled?: boolean;
}

/** `POST /api/v1/channels/:id/test` */
export interface TestChannelResponse {
  ok: boolean;
  message: string;
  capabilities?: ChannelCapabilities;
}

/**
 * `POST /api/v1/channels/notion/init-db`
 * Auto-creates standard 7-column Notion database under `initial_data_source.properties`
 * (`Notion-Version: 2026-03-11`).
 */
export interface NotionInitDbRequest {
  secret: string;
  parent_page_id: string;
  title?: string;
  use_proxy: boolean;
}

export interface NotionInitDbResponse {
  database_id: string;
  data_source_id: string;
  database_url?: string | null;
  column_mapping: NotionColumnMapping;
}

export type NotionStandardFieldKey =
  | "title"
  | "fc_id"
  | "link"
  | "source"
  | "published"
  | "state"
  | "tag";

export type NotionColumnCheckStatus = "matched" | "mismatched" | "missing";

export interface NotionPropertySummary {
  name: string;
  property_type: string;
}

export interface NotionColumnCheckItem {
  field: NotionStandardFieldKey;
  /** Standard column name (`"Title"`, `"FC-ID"`, `"Link"`, `"Source"`, `"Published"`, `"State"`, `"Tag"`). */
  standard_name: string;
  /** Expected Notion property type (`"title"`, `"rich_text"`, `"url"`, `"select"`, `"date"`, `"multi_select"`). */
  expected_type: string;
  /** Actual Notion property type found on same-named column, or `null` if missing. */
  actual_type: string | null;
  /** True strictly for `"title"` and `"fc_id"`. */
  required: boolean;
  status: NotionColumnCheckStatus;
  /** Currently matched/mapped property name on the data source, or `null` if unmapped/missing. */
  mapped_property: string | null;
}

/**
 * `POST /api/v1/channels/notion/inspect-schema`
 * Inspects an existing Notion `data_source_id` (or database ID/URL), previews matched,
 * mismatched, and missing columns (`Title` & `FC-ID` required), and optionally patches
 * missing columns onto the Notion data source when `patch_missing: true`.
 */
export interface NotionInspectSchemaRequest {
  /** Integration secret (when empty during channel edit, `channel_id` may be supplied to reuse stored secret). */
  secret: string;
  data_source_or_db_id: string;
  patch_missing?: boolean;
  use_proxy: boolean;
  channel_id?: number;
}

export interface NotionInspectSchemaResponse {
  data_source_id: string;
  database_id: string | null;
  title: string | null;
  columns: NotionColumnCheckItem[];
  available_properties: NotionPropertySummary[];
  suggested_mapping: NotionColumnMapping;
  patched_columns: string[];
  /** True when required fields (`title` and `fc_id`) have valid mapped columns. */
  ready: boolean;
}

// ---------------------------------------------------------------------------
// 5. Routes (`/api/v1/routes*`)
// ---------------------------------------------------------------------------

/** Matches `crate::channels::TelegramDeliveryMode`. */
export type TelegramDeliveryMode =
  | "rich_message"
  | "compact_card"
  | "media_mode";

/** Matches `crate::channels::RouteOptions` (`routes.options_json`). */
export interface RouteOptions {
  /** Telegram presentation mode (default: `"rich_message"`). */
  telegram_mode?: TelegramDeliveryMode;
  /** Toggle `LinkPreviewOptions` when `telegram_mode === "compact_card"` (default: `true`). */
  link_preview?: boolean;
}

export interface RouteDto {
  id: number;
  name: string;
  feed_ids: number[];
  match_tags: string[];
  channel_id: number;
  include_keywords: string[];
  exclude_keywords: string[];
  include_regex: string | null;
  exclude_regex: string | null;
  options: RouteOptions;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

/** `POST /api/v1/routes` */
export interface CreateRouteRequest {
  name: string;
  feed_ids: number[];
  match_tags: string[];
  channel_id: number;
  include_keywords: string[];
  exclude_keywords: string[];
  include_regex: string | null;
  exclude_regex: string | null;
  options: RouteOptions;
  enabled: boolean;
}

/** `PATCH /api/v1/routes/:id` */
export interface UpdateRouteRequest {
  name?: string;
  feed_ids?: number[];
  match_tags?: string[];
  channel_id?: number;
  include_keywords?: string[];
  exclude_keywords?: string[];
  include_regex?: string | null;
  exclude_regex?: string | null;
  options?: RouteOptions;
  enabled?: boolean;
}

// ---------------------------------------------------------------------------
// 6. Outbox & Dead-Letter Tasks (`/api/v1/tasks*`)
// ---------------------------------------------------------------------------

/**
 * Matches `crate::channels::DeliveryCheckpoint` (`#[serde(tag = "kind", rename_all = "snake_case")]`)
 * persisted in `delivery_tasks.checkpoint_json`.
 */
export type DeliveryCheckpoint =
  | {
      kind: "smtp";
      message_id: string;
    }
  | {
      kind: "telegram";
      next_batch_offset: number;
      sent_message_ids: number[];
      degraded_to_compact: boolean;
    }
  | {
      kind: "notion";
      create_initiated: boolean;
      async_task_id: string | null;
      page_id: string | null;
    };

/** Enriched outbox task DTO (mirrors `crate::db::models::DeliveryTaskDetail`). */
export interface DeliveryTaskDto {
  id: number;
  route_id: number;
  route_name: string;
  channel_id: number;
  channel_name: string;
  channel_kind: ChannelKind;
  entry_id: number;
  /** May be `null` once entry payload has been purged by GC after retention window. */
  entry_title: string | null;
  entry_url: string | null;
  feed_id?: number;
  feed_title?: string | null;
  status: TaskStatus;
  attempt_count: number;
  lease_until: string | null;
  checkpoint: DeliveryCheckpoint | null;
  next_attempt_at: string | null;
  last_status_code: number | null;
  diagnostic_log: string | null;
  completed_at: string | null;
  created_at: string;
}

export interface ListTasksParams {
  status?: TaskStatus;
  limit?: number;
}

/** `POST /api/v1/tasks/retry` */
export interface RetryTasksRequest {
  task_ids?: number[];
  all_dead_letter?: boolean;
}

export interface RetryTasksResponse {
  requeued_count: number;
}

// ---------------------------------------------------------------------------
// 7. System Settings & MCP Tokens (`/api/v1/settings`, `/api/v1/mcp-tokens*`)
// ---------------------------------------------------------------------------

/** `GET /api/v1/settings` & `PUT /api/v1/settings` */
export interface SystemSettingsDto {
  /** Global HTTP / SOCKS5 proxy URL (e.g. `"socks5h://127.0.0.1:1080"`), or `null` when unset. */
  outbound_proxy_url: string | null;
  /** CIDR ranges (e.g. `"10.0.0.0/8"`) or internal hostnames (e.g. `"rsshub.internal"`) allowed past SSRF private-IP block. */
  ssrf_allowlist: string[];
}

export interface UpdateSystemSettingsRequest {
  outbound_proxy_url: string | null;
  ssrf_allowlist: string[];
}

export type McpTokenScope = "readonly" | "admin";

export interface McpTokenDto {
  id: number;
  name: string;
  token_prefix: string;
  scope: McpTokenScope;
  last_used_at: string | null;
  created_at: string;
}

/** `POST /api/v1/mcp-tokens` */
export interface CreateMcpTokenRequest {
  name: string;
  scope: McpTokenScope;
}

export interface CreateMcpTokenResponse {
  token: McpTokenDto;
  /** Full plaintext Bearer token — returned ONLY ONCE upon creation. */
  plaintext_token: string;
  /** Ready-to-copy MCP client JSON snippet pointing at Streamable HTTP `/mcp`. */
  client_config_json: string;
}
