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

/// Well-known `system_settings.key` for the system UI and delivery template locale (`AppLocale`).
pub const SETTING_LOCALE: &str = "locale";

/// Well-known `system_settings.key` for the IANA system timezone used by route delivery windows.
pub const SETTING_TIMEZONE: &str = "timezone";

/// Well-known `system_settings.key` for the global outbox queue pause state.
pub const SETTING_OUTBOX_QUEUE: &str = "outbox_queue";

/// Well-known `system_settings.key` for the WebUI administrator username.
pub const SETTING_ADMIN_USERNAME: &str = "admin_username";

/// Fallback administrator username for new and legacy installations.
pub const DEFAULT_ADMIN_USERNAME: &str = "Courier";

/// `system_settings.key`: ChaCha20-Poly1305 encrypted (base64) active TOTP secret.
pub const SETTING_ADMIN_TOTP_SECRET: &str = "admin_totp_secret";

/// `system_settings.key`: encrypted (base64) TOTP secret awaiting first-code confirmation.
pub const SETTING_ADMIN_TOTP_PENDING: &str = "admin_totp_pending";

/// `system_settings.key`: last accepted TOTP time-step (replay protection).
pub const SETTING_ADMIN_TOTP_LAST_STEP: &str = "admin_totp_last_step";

/// `system_settings.key`: SHA-256 hashes of unused recovery codes.
pub const SETTING_ADMIN_RECOVERY_HASHES: &str = "admin_recovery_hashes";

/// `system_settings.key`: schema/data version of the delivery channel model (`2` = Account + Destinations).
pub const SETTING_CHANNEL_MODEL_VERSION: &str = "channel_model_version";

/// Current delivery channel data model version (v0.2.0 two-layer Channel Account + Destinations).
pub const CHANNEL_MODEL_VERSION_V2: i64 = 2;

/// `system_settings.key`: optional public base URL (`PUBLIC_URL`) for `/u/{token}` and `/v/{token}`.
pub const SETTING_PUBLIC_URL: &str = "public_url";

/// `system_settings.key`: schema/data version of the per-recipient SMTP compliance model (`1` = backfilled).
pub const SETTING_SMTP_RECIPIENT_MODEL_VERSION: &str = "smtp_recipient_model_version";

/// Current SMTP recipient compliance data model version (v0.3.0).
pub const SMTP_RECIPIENT_MODEL_VERSION_V1: i64 = 1;

/// Supported system UI and delivery template locales (`system_settings.locale`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum AppLocale {
    #[default]
    #[serde(rename = "en", alias = "en-US", alias = "en_US")]
    En,
    #[serde(rename = "zh-CN", alias = "zh", alias = "zh_CN", alias = "zh-Hans")]
    ZhCn,
}

impl AppLocale {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhCn => "zh-CN",
        }
    }

    pub const fn is_zh(self) -> bool {
        matches!(self, Self::ZhCn)
    }

    pub fn parse_lossy(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("zh") {
            Some(Self::ZhCn)
        } else if lower.starts_with("en") {
            Some(Self::En)
        } else {
            None
        }
    }
}

impl fmt::Display for AppLocale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AppLocale {
    type Err = AppError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_lossy(s).ok_or_else(|| {
            AppError::Validation(format!(
                "unsupported locale '{s}' (expected 'en' or 'zh-CN')"
            ))
        })
    }
}

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

/// Row from `channel_destinations` (a delivery destination attached to a parent `Channel` account).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChannelDestination {
    pub id: i64,
    pub channel_id: i64,
    pub name: String,
    #[serde(skip_serializing)]
    pub encrypted_config: Vec<u8>,
    pub enabled: bool,
    #[sqlx(default)]
    pub auto_paused: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Input parameters for creating a destination under an existing channel account.
#[derive(Debug, Clone)]
pub struct NewDestination {
    pub channel_id: i64,
    pub name: String,
    pub encrypted_config: Vec<u8>,
    pub enabled: bool,
}

/// Partial update payload for `channel_destinations`.
#[derive(Debug, Clone, Default)]
pub struct UpdateDestination {
    pub name: Option<String>,
    pub encrypted_config: Option<Vec<u8>>,
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
    pub destination_id: i64,
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
    pub destination_id: i64,
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
    pub destination_id: Option<i64>,
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
    pub destination_id: i64,
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

/// Enriched outbox row joining `entries`, `routes`, `channels`, and `channel_destinations` for
/// WebUI & MCP inspection.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct DeliveryTaskDetail {
    pub id: i64,
    pub route_id: i64,
    pub route_name: String,
    pub channel_id: i64,
    pub channel_name: String,
    pub channel_kind: ChannelKind,
    pub destination_id: i64,
    pub destination_name: String,
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

/// Strictest effective pacing and quota limits aggregated across enabled channels sharing a
/// `credential_group_key`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GroupEffectiveLimits {
    pub hourly_quota: Option<i64>,
    pub daily_quota: Option<i64>,
    pub min_interval_secs: i64,
    pub jitter_max_secs: i64,
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

// =============================================================================
// Plugins & Link Cache Models (v0.3.0)
// =============================================================================

/// Row from `plugins`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PluginRow {
    pub id: String,
    pub enabled: bool,
    pub installed_version: String,
    pub settings_json: String,
    pub source: String,
    pub kind: String,
    pub script_js: Option<String>,
    pub script_sha256: Option<String>,
    pub manifest_json: Option<String>,
    pub installed_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Row from `plugin_events` (persistent at-least-once outbox for `on_delivered` hooks).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PluginEventRow {
    pub id: i64,
    pub plugin_id: String,
    pub event: String,
    pub idempotency_key: String,
    pub payload_json: String,
    pub attempts: i64,
    pub next_attempt_at: i64,
    pub created_at: i64,
}

/// Input tuple for atomically enqueuing a `plugin_events` row alongside task completion.
#[derive(Debug, Clone)]
pub struct NewPluginEvent {
    pub plugin_id: String,
    pub event: String,
    pub idempotency_key: String,
    pub payload_json: String,
}

/// Row from `plugin_instances` (encrypted provider credentials/config).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PluginInstanceRow {
    pub id: i64,
    pub plugin_id: String,
    pub name: String,
    #[serde(skip_serializing)]
    pub encrypted_config: Vec<u8>,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

// =============================================================================
// SMTP Per-Recipient Compliance Models (v0.3.0)
// =============================================================================

/// Verification & consent lifecycle state for one SMTP recipient address (`smtp_recipients.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum SmtpRecipientStatus {
    Unverified,
    Pending,
    Verified,
    Unsubscribed,
    Bounced,
}

impl SmtpRecipientStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::Pending => "pending",
            Self::Verified => "verified",
            Self::Unsubscribed => "unsubscribed",
            Self::Bounced => "bounced",
        }
    }
}

impl fmt::Display for SmtpRecipientStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Row from `smtp_recipients` (stores only `address_hmac`, never plaintext email).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SmtpRecipientRow {
    pub id: i64,
    pub destination_id: i64,
    pub address_hmac: String,
    pub status: SmtpRecipientStatus,
    #[serde(skip_serializing)]
    pub code_sha256: Option<String>,
    pub code_expires_at: Option<i64>,
    pub code_attempts: i64,
    pub last_code_sent_at: Option<i64>,
    pub codes_sent_window_start: Option<i64>,
    pub codes_sent_in_window: i64,
    pub consent_method: Option<String>,
    pub consented_at: Option<i64>,
    pub revoked_method: Option<String>,
    pub revoked_at: Option<i64>,
    pub bounce_diagnostic: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Aggregate recipient verification status counts attached to SMTP destination DTOs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmtpRecipientSummary {
    pub total: i64,
    pub verified: i64,
    pub pending: i64,
    pub unverified: i64,
    pub unsubscribed: i64,
    pub bounced: i64,
}
