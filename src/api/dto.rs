//! Canonical REST API (`/api/v1/*`) request and response DTOs aligned 1-to-1 with
//! `web/src/api/types.ts` and `web/src/api/client.ts`.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::channels::notion::normalize_notion_id;
use crate::channels::{
    ChannelConfig, ChannelKind, DeliveryCheckpoint, NotionChannelConfig, NotionColumnMapping,
    RouteOptions, SmtpChannelConfig, SmtpSecurity, TelegramChannelConfig,
};
use crate::config::{LICENSE_SPDX, REPO_URL, VERSION};
use crate::crypto::MasterKey;
use crate::db::models::{
    Channel, DeliveryTaskDetail, DeliveryTaskStatus, Feed, FeedHealthStatus, McpToken,
    McpTokenScope, OverviewSnapshot, Route, unix_to_rfc3339,
};
use crate::error::{AppError, AppResult};
use crate::ingest::ContentFilterRules;

/// Deserialize an optional nullable field (`T | null | undefined` in TypeScript PATCH payloads):
/// - Omitted JSON field -> `None` (keep existing value)
/// - Explicit `null` -> `Some(None)` (clear value)
/// - Explicit value `v` -> `Some(Some(v))` (replace value)
pub fn deserialize_optional_nullable<'de, D, T>(
    deserializer: D,
) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}

/// Generic `{ "ok": true }` action acknowledgement response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionAckResponse {
    pub ok: bool,
}

impl ActionAckResponse {
    pub const fn ok() -> Self {
        Self { ok: true }
    }
}

// ---------------------------------------------------------------------------
// 1. Auth & Compliance Metadata (`/api/v1/auth/*`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthStatusResponse {
    pub setup_required: bool,
    pub authenticated: bool,
    pub version: &'static str,
    pub repo_url: &'static str,
    pub license: &'static str,
}

impl AuthStatusResponse {
    pub const fn new(setup_required: bool, authenticated: bool) -> Self {
        Self {
            setup_required,
            authenticated,
            version: VERSION,
            repo_url: REPO_URL,
            license: LICENSE_SPDX,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthPasswordRequest {
    pub password: String,
}

// ---------------------------------------------------------------------------
// 2. Overview & Maintenance (`/api/v1/overview`, `/api/v1/maintenance/*`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedHealthCounts {
    pub active: i64,
    pub backoff: i64,
    pub circuit_open: i64,
    pub paused: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryCounts {
    pub total_fingerprints: i64,
    pub active_payloads: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStatusCounts {
    pub queued: i64,
    pub delivering: i64,
    pub retrying: i64,
    pub deferred: i64,
    pub dead_letter: i64,
    pub delivered: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelCooldownSummary {
    pub channel_id: i64,
    pub channel_name: String,
    pub kind: ChannelKind,
    pub cooldown_until: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverviewResponse {
    pub feeds: FeedHealthCounts,
    pub entries: EntryCounts,
    pub tasks: TaskStatusCounts,
    pub active_cooldowns: Vec<ChannelCooldownSummary>,
}

impl OverviewResponse {
    pub fn from_snapshot(
        snap: OverviewSnapshot,
        active_cooldowns: Vec<ChannelCooldownSummary>,
    ) -> Self {
        Self {
            feeds: FeedHealthCounts {
                active: snap.feeds_active,
                backoff: snap.feeds_backoff,
                circuit_open: snap.feeds_circuit_open,
                paused: snap.feeds_paused,
            },
            entries: EntryCounts {
                total_fingerprints: snap.entries_total_fingerprints,
                active_payloads: snap.entries_active_payloads,
            },
            tasks: TaskStatusCounts {
                queued: snap.tasks_queued,
                delivering: snap.tasks_delivering,
                retrying: snap.tasks_retrying,
                deferred: snap.tasks_deferred,
                dead_letter: snap.tasks_dead_letter,
                delivered: snap.tasks_delivered,
            },
            active_cooldowns,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcResponse {
    pub purged_entries: u64,
}

// ---------------------------------------------------------------------------
// 3. Feeds & OPML (`/api/v1/feeds*`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedDto {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub site_url: Option<String>,
    pub favicon_url: Option<String>,
    pub tags: Vec<String>,
    pub poll_interval_secs: i64,
    pub fetch_full_content: bool,
    pub use_proxy: bool,
    pub silent_init: bool,
    pub health_status: FeedHealthStatus,
    pub consecutive_failures: i64,
    pub next_poll_at: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl FeedDto {
    pub fn from_model(feed: Feed) -> Self {
        let tags = feed.tags();
        Self {
            id: feed.id,
            url: feed.url,
            title: feed.title,
            site_url: feed.site_url,
            favicon_url: feed.favicon_url,
            tags,
            poll_interval_secs: feed.poll_interval_secs,
            fetch_full_content: feed.fetch_full_content,
            use_proxy: feed.use_proxy,
            silent_init: feed.silent_init,
            health_status: feed.health_status,
            consecutive_failures: feed.consecutive_failures,
            next_poll_at: feed.next_poll_at.map(unix_to_rfc3339),
            last_error: feed.last_error,
            created_at: unix_to_rfc3339(feed.created_at),
            updated_at: unix_to_rfc3339(feed.updated_at),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateFeedRequest {
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub poll_interval_secs: Option<i64>,
    #[serde(default)]
    pub fetch_full_content: Option<bool>,
    #[serde(default)]
    pub use_proxy: Option<bool>,
    #[serde(default)]
    pub silent_init: Option<bool>,
    #[serde(default)]
    pub push_latest_one: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateFeedRequest {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_nullable")]
    pub title: Option<Option<String>>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub poll_interval_secs: Option<i64>,
    #[serde(default)]
    pub fetch_full_content: Option<bool>,
    #[serde(default)]
    pub use_proxy: Option<bool>,
    #[serde(default)]
    pub silent_init: Option<bool>,
    #[serde(default)]
    pub health_status: Option<FeedHealthStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollFeedResponse {
    pub feed: FeedDto,
    pub new_entries: usize,
    pub enqueued_tasks: usize,
    pub not_modified: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeedPreviewRequest {
    pub url: String,
    #[serde(default)]
    pub fetch_full_content: bool,
    #[serde(default)]
    pub use_proxy: bool,
    #[serde(default)]
    pub filter: Option<ContentFilterRules>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpmlImportRequest {
    pub opml_xml: String,
    #[serde(default = "default_true")]
    pub silent_init: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpmlImportResponse {
    pub imported_count: usize,
    pub skipped_existing_count: usize,
    pub feeds: Vec<FeedDto>,
}

// ---------------------------------------------------------------------------
// 4. Trusted Channels (`/api/v1/channels*`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelDto {
    pub id: i64,
    pub name: String,
    pub kind: ChannelKind,
    pub config: Value,
    pub capabilities: Value,
    pub min_interval_secs: i64,
    pub jitter_max_secs: i64,
    pub hourly_quota: Option<i64>,
    pub daily_quota: Option<i64>,
    pub use_proxy: bool,
    pub cooldown_until: Option<String>,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl ChannelDto {
    pub fn from_model(channel: Channel, master_key: &MasterKey) -> AppResult<Self> {
        let decrypted: ChannelConfig = master_key.decrypt_json(&channel.encrypted_config)?;
        let redacted_view = decrypted.redacted_view();
        let capabilities = channel.capabilities_value();

        Ok(Self {
            id: channel.id,
            name: channel.name,
            kind: channel.kind,
            config: redacted_view,
            capabilities,
            min_interval_secs: channel.min_interval_secs,
            jitter_max_secs: channel.jitter_max_secs,
            hourly_quota: channel.hourly_quota,
            daily_quota: channel.daily_quota,
            use_proxy: channel.use_proxy,
            cooldown_until: channel.cooldown_until.map(unix_to_rfc3339),
            enabled: channel.enabled,
            created_at: unix_to_rfc3339(channel.created_at),
            updated_at: unix_to_rfc3339(channel.updated_at),
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateChannelRequest {
    pub name: String,
    pub kind: ChannelKind,
    pub config: ChannelConfig,
    #[serde(default = "default_min_interval_secs")]
    pub min_interval_secs: i64,
    #[serde(default = "default_jitter_max_secs")]
    pub jitter_max_secs: i64,
    #[serde(default)]
    pub hourly_quota: Option<i64>,
    #[serde(default)]
    pub daily_quota: Option<i64>,
    #[serde(default)]
    pub use_proxy: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Secret-preserving target patch payload (`PATCH /api/v1/channels/{id}`).
///
/// When `password` / `bot_token` / `integration_secret` is omitted, `null`, or empty,
/// the backend preserves the stored encrypted secret intact.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChannelTargetPatch {
    Smtp {
        #[serde(default)]
        host: Option<String>,
        #[serde(default)]
        port: Option<u16>,
        #[serde(default, deserialize_with = "deserialize_optional_nullable")]
        username: Option<Option<String>>,
        #[serde(default)]
        password: Option<String>,
        #[serde(default)]
        security: Option<SmtpSecurity>,
        #[serde(default)]
        from_address: Option<String>,
        #[serde(default)]
        to_addresses: Option<Vec<String>>,
        #[serde(default, deserialize_with = "deserialize_optional_nullable")]
        reply_to: Option<Option<String>>,
    },
    Telegram {
        #[serde(default)]
        bot_token: Option<String>,
        #[serde(default)]
        chat_id: Option<String>,
        #[serde(default, deserialize_with = "deserialize_optional_nullable")]
        message_thread_id: Option<Option<i64>>,
        #[serde(default, deserialize_with = "deserialize_optional_nullable")]
        api_base_url: Option<Option<String>>,
    },
    Notion {
        #[serde(default)]
        integration_secret: Option<String>,
        #[serde(default)]
        data_source_id: Option<String>,
        #[serde(default, deserialize_with = "deserialize_optional_nullable")]
        database_id: Option<Option<String>>,
        #[serde(default, deserialize_with = "deserialize_optional_nullable")]
        parent_page_id: Option<Option<String>>,
        #[serde(default)]
        column_mapping: Option<NotionColumnMapping>,
    },
}

impl ChannelTargetPatch {
    pub const fn kind(&self) -> ChannelKind {
        match self {
            Self::Smtp { .. } => ChannelKind::Smtp,
            Self::Telegram { .. } => ChannelKind::Telegram,
            Self::Notion { .. } => ChannelKind::Notion,
        }
    }

    /// Merge this patch onto `existing`, returning `(merged_config, telegram_endpoint_changed)`.
    pub fn apply_to(&self, existing: &ChannelConfig) -> AppResult<(ChannelConfig, bool)> {
        match (existing, self) {
            (
                ChannelConfig::Smtp(cur),
                Self::Smtp {
                    host,
                    port,
                    username,
                    password,
                    security,
                    from_address,
                    to_addresses,
                    reply_to,
                },
            ) => {
                let next_username = match username {
                    Some(opt) => normalize_opt_string(opt.clone()),
                    None => cur.username.clone(),
                };
                let next_password = match password.as_deref() {
                    Some(raw) if !raw.is_empty() => Some(raw.to_string()),
                    _ => cur.password.clone(),
                };
                let next_reply_to = match reply_to {
                    Some(opt) => normalize_opt_string(opt.clone()),
                    None => cur.reply_to.clone(),
                };
                let next_to_addresses = match to_addresses {
                    Some(list) => normalize_string_vec(list.clone()),
                    None => cur.to_addresses.clone(),
                };

                let merged = ChannelConfig::Smtp(SmtpChannelConfig {
                    host: host
                        .as_deref()
                        .map(str::trim)
                        .unwrap_or(&cur.host)
                        .to_string(),
                    port: port.unwrap_or(cur.port),
                    username: next_username,
                    password: next_password,
                    security: security.unwrap_or(cur.security),
                    from_address: from_address
                        .as_deref()
                        .map(str::trim)
                        .unwrap_or(&cur.from_address)
                        .to_string(),
                    to_addresses: next_to_addresses,
                    reply_to: next_reply_to,
                });
                merged.validate()?;
                Ok((merged, false))
            }
            (
                ChannelConfig::Telegram(cur),
                Self::Telegram {
                    bot_token,
                    chat_id,
                    message_thread_id,
                    api_base_url,
                },
            ) => {
                let (next_token, token_changed) = match bot_token.as_deref().map(str::trim) {
                    Some(tok) if !tok.is_empty() => (tok.to_string(), tok != cur.bot_token),
                    _ => (cur.bot_token.clone(), false),
                };
                let next_api_base = match api_base_url {
                    Some(opt) => normalize_opt_string(opt.clone()),
                    None => cur.api_base_url.clone(),
                };
                let base_changed = next_api_base != cur.api_base_url;
                let next_thread = match message_thread_id {
                    Some(opt) => *opt,
                    None => cur.message_thread_id,
                };

                let merged = ChannelConfig::Telegram(TelegramChannelConfig {
                    bot_token: next_token,
                    chat_id: chat_id
                        .as_deref()
                        .map(str::trim)
                        .unwrap_or(&cur.chat_id)
                        .to_string(),
                    message_thread_id: next_thread,
                    api_base_url: next_api_base,
                });
                merged.validate()?;
                Ok((merged, token_changed || base_changed))
            }
            (
                ChannelConfig::Notion(cur),
                Self::Notion {
                    integration_secret,
                    data_source_id,
                    database_id,
                    parent_page_id,
                    column_mapping,
                },
            ) => {
                let next_secret = match integration_secret.as_deref().map(str::trim) {
                    Some(sec) if !sec.is_empty() => sec.to_string(),
                    _ => cur.integration_secret.clone(),
                };
                let next_ds_id = match data_source_id.as_deref().map(str::trim) {
                    Some(raw) if !raw.is_empty() => normalize_notion_id(raw)?,
                    _ => cur.data_source_id.clone(),
                };
                let next_db_id = match database_id {
                    Some(Some(raw)) if !raw.trim().is_empty() => Some(normalize_notion_id(raw)?),
                    Some(_) => None,
                    None => cur.database_id.clone(),
                };
                let next_parent_id = match parent_page_id {
                    Some(Some(raw)) if !raw.trim().is_empty() => Some(normalize_notion_id(raw)?),
                    Some(_) => None,
                    None => cur.parent_page_id.clone(),
                };

                let merged = ChannelConfig::Notion(NotionChannelConfig {
                    integration_secret: next_secret,
                    data_source_id: next_ds_id,
                    database_id: next_db_id,
                    parent_page_id: next_parent_id,
                    column_mapping: column_mapping
                        .clone()
                        .unwrap_or_else(|| cur.column_mapping.clone()),
                });
                merged.validate()?;
                Ok((merged, false))
            }
            (existing_cfg, patch) => Err(AppError::Validation(format!(
                "cannot change channel kind from '{}' to '{}'",
                existing_cfg.kind(),
                patch.kind()
            ))),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateChannelRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub config: Option<ChannelTargetPatch>,
    #[serde(default)]
    pub min_interval_secs: Option<i64>,
    #[serde(default)]
    pub jitter_max_secs: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_nullable")]
    pub hourly_quota: Option<Option<i64>>,
    #[serde(default, deserialize_with = "deserialize_optional_nullable")]
    pub daily_quota: Option<Option<i64>>,
    #[serde(default)]
    pub use_proxy: Option<bool>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestChannelResponse {
    pub ok: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Value>,
}

// ---------------------------------------------------------------------------
// 5. Routes (`/api/v1/routes*`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteDto {
    pub id: i64,
    pub name: String,
    pub feed_ids: Vec<i64>,
    pub match_tags: Vec<String>,
    pub channel_id: i64,
    pub include_keywords: Vec<String>,
    pub exclude_keywords: Vec<String>,
    pub include_regex: Option<String>,
    pub exclude_regex: Option<String>,
    pub options: RouteOptions,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl RouteDto {
    pub fn from_model(route: Route) -> Self {
        let feed_ids = route.feed_ids();
        let match_tags = route.match_tags();
        let include_keywords = route.include_keywords();
        let exclude_keywords = route.exclude_keywords();
        let options = route.options();

        Self {
            id: route.id,
            name: route.name,
            feed_ids,
            match_tags,
            channel_id: route.channel_id,
            include_keywords,
            exclude_keywords,
            include_regex: route.include_regex,
            exclude_regex: route.exclude_regex,
            options,
            enabled: route.enabled,
            created_at: unix_to_rfc3339(route.created_at),
            updated_at: unix_to_rfc3339(route.updated_at),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRouteRequest {
    pub name: String,
    #[serde(default)]
    pub feed_ids: Vec<i64>,
    #[serde(default)]
    pub match_tags: Vec<String>,
    pub channel_id: i64,
    #[serde(default)]
    pub include_keywords: Vec<String>,
    #[serde(default)]
    pub exclude_keywords: Vec<String>,
    #[serde(default)]
    pub include_regex: Option<String>,
    #[serde(default)]
    pub exclude_regex: Option<String>,
    #[serde(default)]
    pub options: RouteOptions,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateRouteRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub feed_ids: Option<Vec<i64>>,
    #[serde(default)]
    pub match_tags: Option<Vec<String>>,
    #[serde(default)]
    pub channel_id: Option<i64>,
    #[serde(default)]
    pub include_keywords: Option<Vec<String>>,
    #[serde(default)]
    pub exclude_keywords: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_nullable")]
    pub include_regex: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_nullable")]
    pub exclude_regex: Option<Option<String>>,
    #[serde(default)]
    pub options: Option<RouteOptions>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

// ---------------------------------------------------------------------------
// 6. Outbox & Dead-Letter Tasks (`/api/v1/tasks*`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListTasksQuery {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryTaskDto {
    pub id: i64,
    pub route_id: i64,
    pub route_name: String,
    pub channel_id: i64,
    pub channel_name: String,
    pub channel_kind: ChannelKind,
    pub entry_id: i64,
    pub entry_title: Option<String>,
    pub entry_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feed_id: Option<i64>,
    pub feed_title: Option<String>,
    pub status: DeliveryTaskStatus,
    pub attempt_count: i64,
    pub lease_until: Option<String>,
    pub checkpoint: Option<DeliveryCheckpoint>,
    pub next_attempt_at: Option<String>,
    pub last_status_code: Option<i32>,
    pub diagnostic_log: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
}

impl DeliveryTaskDto {
    pub fn from_detail(detail: DeliveryTaskDetail, feed_meta: Option<(i64, String)>) -> Self {
        let checkpoint = detail
            .checkpoint_json
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .and_then(|raw| serde_json::from_str::<DeliveryCheckpoint>(raw).ok());

        let (feed_id, feed_title) = match feed_meta {
            Some((fid, ftitle)) => (Some(fid), Some(ftitle)),
            None => (None, None),
        };

        Self {
            id: detail.id,
            route_id: detail.route_id,
            route_name: detail.route_name,
            channel_id: detail.channel_id,
            channel_name: detail.channel_name,
            channel_kind: detail.channel_kind,
            entry_id: detail.entry_id,
            entry_title: detail.entry_title,
            entry_url: detail.entry_url,
            feed_id,
            feed_title,
            status: detail.status,
            attempt_count: detail.attempt_count,
            lease_until: detail.lease_until.map(unix_to_rfc3339),
            checkpoint,
            next_attempt_at: Some(unix_to_rfc3339(detail.next_attempt_at)),
            last_status_code: detail.last_status_code,
            diagnostic_log: detail.diagnostic_log,
            completed_at: detail.completed_at.map(unix_to_rfc3339),
            created_at: unix_to_rfc3339(detail.created_at),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RetryTasksRequest {
    #[serde(default)]
    pub task_ids: Option<Vec<i64>>,
    #[serde(default)]
    pub all_dead_letter: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryTasksResponse {
    pub requeued_count: u64,
}

// ---------------------------------------------------------------------------
// 7. System Settings & MCP Tokens (`/api/v1/settings`, `/api/v1/mcp-tokens*`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemSettingsDto {
    pub outbound_proxy_url: Option<String>,
    pub ssrf_allowlist: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSystemSettingsRequest {
    #[serde(default)]
    pub outbound_proxy_url: Option<String>,
    #[serde(default)]
    pub ssrf_allowlist: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTokenDto {
    pub id: i64,
    pub name: String,
    pub token_prefix: String,
    pub scope: McpTokenScope,
    pub last_used_at: Option<String>,
    pub created_at: String,
}

impl McpTokenDto {
    pub fn from_model(token: McpToken) -> Self {
        Self {
            id: token.id,
            name: token.name,
            token_prefix: token.token_prefix,
            scope: token.scope,
            last_used_at: token.last_used_at.map(unix_to_rfc3339),
            created_at: unix_to_rfc3339(token.created_at),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateMcpTokenRequest {
    pub name: String,
    pub scope: McpTokenScope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateMcpTokenResponse {
    pub token: McpTokenDto,
    pub plaintext_token: String,
    pub client_config_json: String,
}

// ---------------------------------------------------------------------------
// Normalization Helpers
// ---------------------------------------------------------------------------

pub fn normalize_opt_string(input: Option<String>) -> Option<String> {
    input
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn normalize_string_vec(items: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let trimmed = item.trim();
        if !trimmed.is_empty() && !out.iter().any(|existing| existing == trimmed) {
            out.push(trimmed.to_string());
        }
    }
    out
}

pub fn normalize_channel_config_on_create(cfg: &mut ChannelConfig) -> AppResult<()> {
    match cfg {
        ChannelConfig::Smtp(smtp) => {
            smtp.host = smtp.host.trim().to_string();
            smtp.username = normalize_opt_string(smtp.username.take());
            smtp.password = smtp.password.take().filter(|p| !p.is_empty());
            smtp.from_address = smtp.from_address.trim().to_string();
            smtp.to_addresses = normalize_string_vec(std::mem::take(&mut smtp.to_addresses));
            smtp.reply_to = normalize_opt_string(smtp.reply_to.take());
        }
        ChannelConfig::Telegram(tg) => {
            tg.bot_token = tg.bot_token.trim().to_string();
            tg.chat_id = tg.chat_id.trim().to_string();
            tg.api_base_url = normalize_opt_string(tg.api_base_url.take());
        }
        ChannelConfig::Notion(notion) => {
            notion.integration_secret = notion.integration_secret.trim().to_string();
            notion.data_source_id = normalize_notion_id(&notion.data_source_id)?;
            notion.database_id = match notion.database_id.take() {
                Some(raw) if !raw.trim().is_empty() => Some(normalize_notion_id(&raw)?),
                _ => None,
            };
            notion.parent_page_id = match notion.parent_page_id.take() {
                Some(raw) if !raw.trim().is_empty() => Some(normalize_notion_id(&raw)?),
                _ => None,
            };
        }
    }
    cfg.validate()
}

const fn default_true() -> bool {
    true
}

const fn default_min_interval_secs() -> i64 {
    15
}

const fn default_jitter_max_secs() -> i64 {
    10
}
