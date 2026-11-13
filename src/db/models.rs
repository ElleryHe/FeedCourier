//! SQLite row structs, domain enums, and repository input/output DTOs.
//!
//! All persisted timestamps use UTC Unix epoch seconds (`i64`).

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::channels::{ChannelKind, DeliveryCheckpoint, RouteOptions};
use crate::error::{AppError, AppResult};

/// Well-known `system_settings.key` for the Argon2id administrator password PHC hash.
pub const SETTING_ADMIN_PASSWORD_HASH: &str = "admin_password_hash";

/// Well-known `system_settings.key` for the global outbound HTTP/SOCKS5 proxy URL.
pub const SETTING_OUTBOUND_PROXY_URL: &str = "outbound_proxy_url";

/// Well-known `system_settings.key` for SSRF CIDR/hostname allowlist entries (`Vec<String>`).
pub const SETTING_SSRF_ALLOWLIST: &str = "ssrf_allowlist";

/// Return current UTC Unix timestamp in seconds.
pub fn now_unix() -> i64 {
    Utc::now().timestamp()
}

/// Convert a UTC Unix timestamp in seconds to a `chrono::DateTime<Utc>`.
pub fn unix_to_datetime(ts: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_opt(ts, 0).single()
}

/// Format a UTC Unix timestamp in seconds as an ISO-8601 / RFC 3339 string (`YYYY-MM-DDTHH:MM:SSZ`).
pub fn unix_to_rfc3339(ts: i64) -> String {
    unix_to_datetime(ts)
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).unwrap())
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Feed polling health state (`feeds.health_status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type, Default)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum FeedHealthStatus {
    #[default]
    Active,
    Backoff,
    CircuitOpen,
    Paused,
}

impl FeedHealthStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Backoff => "backoff",
            Self::CircuitOpen => "circuit_open",
            Self::Paused => "paused",
        }
    }
}

impl fmt::Display for FeedHealthStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FeedHealthStatus {
    type Err = AppError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "active" => Ok(Self::Active),
            "backoff" => Ok(Self::Backoff),
            "circuit_open" => Ok(Self::CircuitOpen),
            "paused" => Ok(Self::Paused),
            other => Err(AppError::Validation(format!(
                "invalid feed health_status '{other}'"
            ))),
        }
    }
}

/// Outbox delivery task lifecycle state (`delivery_tasks.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type, Default)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum DeliveryTaskStatus {
    #[default]
    Queued,
    Delivering,
    Delivered,
    Retrying,
    Deferred,
    DeadLetter,
}

impl DeliveryTaskStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Delivering => "delivering",
            Self::Delivered => "delivered",
            Self::Retrying => "retrying",
            Self::Deferred => "deferred",
            Self::DeadLetter => "dead_letter",
        }
    }

    /// Returns `true` if the task is still active/in-flight and must prevent entry payload GC.
    pub const fn blocks_payload_gc(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Delivering | Self::Retrying | Self::Deferred
        )
    }
}

impl fmt::Display for DeliveryTaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DeliveryTaskStatus {
    type Err = AppError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "queued" => Ok(Self::Queued),
            "delivering" => Ok(Self::Delivering),
            "delivered" => Ok(Self::Delivered),
            "retrying" => Ok(Self::Retrying),
            "deferred" => Ok(Self::Deferred),
            "dead_letter" => Ok(Self::DeadLetter),
            other => Err(AppError::Validation(format!(
                "invalid delivery task status '{other}'"
            ))),
        }
    }
}

/// Authorization tier for MCP bearer tokens (`mcp_tokens.scope`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum McpTokenScope {
    Readonly,
    Admin,
}

impl McpTokenScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Readonly => "readonly",
            Self::Admin => "admin",
        }
    }
}

impl fmt::Display for McpTokenScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for McpTokenScope {
    type Err = AppError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "readonly" => Ok(Self::Readonly),
            "admin" => Ok(Self::Admin),
            other => Err(AppError::Validation(format!(
                "invalid MCP token scope '{other}' (expected readonly or admin)"
            ))),
        }
    }
}

/// Row from `feeds`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Feed {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub site_url: Option<String>,
    pub favicon_url: Option<String>,
    pub tags_json: String,
    pub poll_interval_secs: i64,
    pub fetch_full_content: bool,
    pub use_proxy: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub health_status: FeedHealthStatus,
    pub consecutive_failures: i64,
    pub next_poll_at: Option<i64>,
    pub last_error: Option<String>,
    pub silent_init: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Feed {
    /// Parse `tags_json` into a `Vec<String>`.
    pub fn tags(&self) -> Vec<String> {
        serde_json::from_str(&self.tags_json).unwrap_or_default()
    }
}

/// Input parameters for creating a new feed.
#[derive(Debug, Clone)]
pub struct NewFeed {
    pub url: String,
    pub title: String,
    pub site_url: Option<String>,
    pub favicon_url: Option<String>,
    pub tags: Vec<String>,
    pub poll_interval_secs: i64,
    pub fetch_full_content: bool,
    pub use_proxy: bool,
    pub silent_init: bool,
    pub next_poll_at: Option<i64>,
}

impl NewFeed {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            title: String::new(),
            site_url: None,
            favicon_url: None,
            tags: Vec::new(),
            poll_interval_secs: 1800,
            fetch_full_content: false,
            use_proxy: false,
            silent_init: true,
            next_poll_at: None,
        }
    }
}

/// Partial update payload for `feeds`.
#[derive(Debug, Clone, Default)]
pub struct UpdateFeed {
    pub url: Option<String>,
    pub title: Option<String>,
    pub site_url: Option<Option<String>>,
    pub favicon_url: Option<Option<String>>,
    pub tags: Option<Vec<String>>,
    pub poll_interval_secs: Option<i64>,
    pub fetch_full_content: Option<bool>,
    pub use_proxy: Option<bool>,
    pub silent_init: Option<bool>,
    pub health_status: Option<FeedHealthStatus>,
}

/// Extracted media item persisted temporarily in `entries.media_json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaAttachment {
    pub url: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub kind: MediaAttachmentKind,
    #[serde(default)]
    pub length_bytes: Option<u64>,
    #[serde(default)]
    pub caption: Option<String>,
}

/// Media category extracted from `<enclosure>`, `media:content`, or inline HTML tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MediaAttachmentKind {
    #[default]
    Image,
    Video,
    Audio,
    Document,
}

/// Row from `entries`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Entry {
    pub id: i64,
    pub feed_id: i64,
    pub fingerprint_sha256: String,
    pub published_at: Option<i64>,
    pub discovered_at: i64,
    pub payload_purged_at: Option<i64>,
    pub title: Option<String>,
    pub canonical_url: Option<String>,
    pub author: Option<String>,
    pub content_html: Option<String>,
    pub first_sentence: Option<String>,
    pub summary_text: Option<String>,
    pub media_json: Option<String>,
}

impl Entry {
    /// Returns `true` if the transient article body & media metadata have been garbage-collected.
    pub const fn is_purged(&self) -> bool {
        self.payload_purged_at.is_some()
    }

    /// Parse `media_json` into `Vec<MediaAttachment>`.
    pub fn media(&self) -> Vec<MediaAttachment> {
        self.media_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default()
    }
}

/// Input parameters for inserting a newly discovered entry.
#[derive(Debug, Clone)]
pub struct NewEntry {
    pub feed_id: i64,
    pub fingerprint_sha256: String,
    pub published_at: Option<i64>,
    pub discovered_at: i64,
    pub title: Option<String>,
    pub canonical_url: Option<String>,
    pub author: Option<String>,
    pub content_html: Option<String>,
    pub first_sentence: Option<String>,
    pub summary_text: Option<String>,
    pub media: Vec<MediaAttachment>,
}

/// Result of idempotent `(feed_id, fingerprint_sha256)` entry insertion.
#[derive(Debug, Clone)]
pub enum EntryInsertOutcome {
    /// Brand-new fingerprint stored.
    Inserted(Box<Entry>),
    /// Fingerprint already existed on this feed; silent deduplication hit.
    Duplicate { existing_id: i64 },
}

/// Row from `channels`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Channel {
    pub id: i64,
    pub name: String,
    pub kind: ChannelKind,
    #[serde(skip_serializing)]
    pub encrypted_config: Vec<u8>,
    pub credential_group_key: String,
    pub capabilities_json: String,
    pub min_interval_secs: i64,
    pub jitter_max_secs: i64,
    pub hourly_quota: Option<i64>,
    pub daily_quota: Option<i64>,
    pub use_proxy: bool,
    pub cooldown_until: Option<i64>,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Channel {
    /// Parse `capabilities_json` as generic JSON value.
    pub fn capabilities_value(&self) -> serde_json::Value {
        serde_json::from_str(&self.capabilities_json)
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()))
    }
}

/// Input parameters for creating a channel.
#[derive(Debug, Clone)]
pub struct NewChannel {
    pub name: String,
    pub kind: ChannelKind,
    pub encrypted_config: Vec<u8>,
    pub credential_group_key: String,
    pub capabilities_json: String,
    pub min_interval_secs: i64,
    pub jitter_max_secs: i64,
    pub hourly_quota: Option<i64>,
    pub daily_quota: Option<i64>,
    pub use_proxy: bool,
    pub enabled: bool,
}

/// Partial update payload for `channels`.
#[derive(Debug, Clone, Default)]
pub struct UpdateChannel {
    pub name: Option<String>,
    pub encrypted_config: Option<Vec<u8>>,
    pub credential_group_key: Option<String>,
    pub capabilities_json: Option<String>,
    pub min_interval_secs: Option<i64>,
    pub jitter_max_secs: Option<i64>,
    pub hourly_quota: Option<Option<i64>>,
    pub daily_quota: Option<Option<i64>>,
    pub use_proxy: Option<bool>,
    pub cooldown_until: Option<Option<i64>>,
    pub enabled: Option<bool>,
}

/// Row from `routes`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Route {
    pub id: i64,
    pub name: String,
    pub feed_ids_json: String,
    pub match_tags_json: String,
    pub channel_id: i64,
    pub include_keywords_json: String,
    pub exclude_keywords_json: String,
    pub include_regex: Option<String>,
    pub exclude_regex: Option<String>,
    pub options_json: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Route {
    pub fn feed_ids(&self) -> Vec<i64> {
        serde_json::from_str(&self.feed_ids_json).unwrap_or_default()
    }

    pub fn match_tags(&self) -> Vec<String> {
        serde_json::from_str(&self.match_tags_json).unwrap_or_default()
    }

    pub fn include_keywords(&self) -> Vec<String> {
        serde_json::from_str(&self.include_keywords_json).unwrap_or_default()
    }

    pub fn exclude_keywords(&self) -> Vec<String> {
        serde_json::from_str(&self.exclude_keywords_json).unwrap_or_default()
    }

    pub fn options(&self) -> RouteOptions {
        serde_json::from_str(&self.options_json).unwrap_or_default()
    }
}

/// Input parameters for creating a route.
#[derive(Debug, Clone)]
pub struct NewRoute {
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
}

/// Partial update payload for `routes`.
#[derive(Debug, Clone, Default)]
pub struct UpdateRoute {
    pub name: Option<String>,
    pub feed_ids: Option<Vec<i64>>,
    pub match_tags: Option<Vec<String>>,
    pub channel_id: Option<i64>,
    pub include_keywords: Option<Vec<String>>,
    pub exclude_keywords: Option<Vec<String>>,
    pub include_regex: Option<Option<String>>,
    pub exclude_regex: Option<Option<String>>,
    pub options: Option<RouteOptions>,
    pub enabled: Option<bool>,
}

/// Row from `delivery_tasks`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct DeliveryTask {
    pub id: i64,
    pub route_id: i64,
    pub channel_id: i64,
    pub entry_id: i64,
    pub status: DeliveryTaskStatus,
    pub lease_until: Option<i64>,
    pub checkpoint_json: Option<String>,
    pub attempt_count: i64,
    pub next_attempt_at: i64,
    pub last_status_code: Option<i32>,
    pub diagnostic_log: Option<String>,
    pub completed_at: Option<i64>,
    pub created_at: i64,
}

impl DeliveryTask {
    /// Parse `checkpoint_json` into typed `DeliveryCheckpoint`.
    pub fn checkpoint(&self) -> AppResult<Option<DeliveryCheckpoint>> {
        match self.checkpoint_json.as_deref() {
            None | Some("") => Ok(None),
            Some(raw) => {
                let parsed = serde_json::from_str(raw)?;
                Ok(Some(parsed))
            }
        }
    }
}

/// Enriched outbox row joining `entries`, `routes`, and `channels` for WebUI & MCP inspection.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct DeliveryTaskDetail {
    pub id: i64,
    pub route_id: i64,
    pub route_name: String,
    pub channel_id: i64,
    pub channel_name: String,
    pub channel_kind: ChannelKind,
    pub entry_id: i64,
    pub entry_title: Option<String>,
    pub entry_url: Option<String>,
    pub status: DeliveryTaskStatus,
    pub lease_until: Option<i64>,
    pub checkpoint_json: Option<String>,
    pub attempt_count: i64,
    pub next_attempt_at: i64,
    pub last_status_code: Option<i32>,
    pub diagnostic_log: Option<String>,
    pub completed_at: Option<i64>,
    pub created_at: i64,
}

/// Rolling hourly and daily delivery counters for quota enforcement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaUsage {
    pub channel_last_hour: i64,
    pub channel_last_day: i64,
    pub credential_group_last_hour: i64,
    pub credential_group_last_day: i64,
    pub last_completed_at: Option<i64>,
}

/// Aggregated dashboard metrics returned for `GET /api/v1/overview`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OverviewSnapshot {
    pub feeds_active: i64,
    pub feeds_backoff: i64,
    pub feeds_circuit_open: i64,
    pub feeds_paused: i64,
    pub entries_total_fingerprints: i64,
    pub entries_active_payloads: i64,
    pub tasks_queued: i64,
    pub tasks_delivering: i64,
    pub tasks_retrying: i64,
    pub tasks_deferred: i64,
    pub tasks_dead_letter: i64,
    pub tasks_delivered: i64,
}

/// Row from `system_settings`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SystemSetting {
    pub key: String,
    pub value_json: String,
    pub updated_at: i64,
}

/// Row from `mcp_tokens`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct McpToken {
    pub id: i64,
    pub name: String,
    #[serde(skip_serializing)]
    pub token_hash: String,
    pub token_prefix: String,
    pub scope: McpTokenScope,
    pub last_used_at: Option<i64>,
    pub created_at: i64,
}

/// Row from `sessions` (HttpOnly WebUI administrator session cookies).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Session {
    #[serde(skip_serializing)]
    pub token_hash: String,
    pub expires_at: i64,
    pub created_at: i64,
}
