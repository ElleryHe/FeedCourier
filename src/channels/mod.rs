//! Shared delivery channel domain contracts, encrypted configuration schemas,
//! route presentation options, checkpoints, and the `ChannelSender` trait.
//!
//! Concrete channel implementations (`smtp`, `telegram`, `notion`) plug into this module in Wave 2.

pub mod smtp;
pub mod telegram;

use std::fmt;
use std::str::FromStr;

use async_trait::async_trait;
use htmd as _;
use serde::{Deserialize, Serialize};

use crate::crypto::compute_credential_group_key;
use crate::db::models::{Channel, DeliveryTask, Entry, Feed, Route};
use crate::error::{AppError, AppResult};

/// Supported trusted delivery channel types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum ChannelKind {
    Smtp,
    Telegram,
    Notion,
}

impl ChannelKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Smtp => "smtp",
            Self::Telegram => "telegram",
            Self::Notion => "notion",
        }
    }
}

impl fmt::Display for ChannelKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ChannelKind {
    type Err = AppError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "smtp" => Ok(Self::Smtp),
            "telegram" => Ok(Self::Telegram),
            "notion" => Ok(Self::Notion),
            other => Err(AppError::Validation(format!(
                "unsupported channel kind '{other}' (expected smtp, telegram, or notion)"
            ))),
        }
    }
}

/// Transport encryption mode for SMTP connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SmtpSecurity {
    /// Opportunistic/explicit STARTTLS upgrade (typically port 587).
    #[default]
    StartTls,
    /// Implicit TLS / SMTPS wrapper (typically port 465).
    Tls,
    /// Unencrypted plain SMTP (only for trusted local relays, e.g. port 25/1025).
    Plain,
}

/// Decrypted SMTP channel credentials and Force-From envelope settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmtpChannelConfig {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub security: SmtpSecurity,
    /// Fixed SPF/DKIM/DMARC-aligned sender address (`Force-From`).
    pub from_address: String,
    /// Recipients locked inside this trusted channel.
    pub to_addresses: Vec<String>,
    #[serde(default)]
    pub reply_to: Option<String>,
}

/// Decrypted Telegram Bot channel credentials and target chat/topic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramChannelConfig {
    pub bot_token: String,
    /// Target private user ID, group ID, or `@channelusername`.
    pub chat_id: String,
    /// Optional forum supergroup topic ID (`message_thread_id`).
    #[serde(default)]
    pub message_thread_id: Option<i64>,
    /// Optional custom Bot API base URL (defaults to `https://api.telegram.org`).
    #[serde(default)]
    pub api_base_url: Option<String>,
}

/// Probed Telegram Bot API capabilities cached in `channels.capabilities_json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TelegramCapabilities {
    #[serde(default)]
    pub supports_rich_message: bool,
    #[serde(default)]
    pub bot_api_version: Option<String>,
    #[serde(default)]
    pub bot_username: Option<String>,
    #[serde(default)]
    pub probed_at: Option<i64>,
    #[serde(default)]
    pub fallback_reason: Option<String>,
}

/// Property name mapping for Notion (`Notion-Version: 2026-03-11` `data_source_id`).
///
/// Only `title` (`title`) and `fc_id` (`rich_text`) are strictly required; optional properties
/// may be skipped (`None`) when binding an existing user database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotionColumnMapping {
    /// Notion `title` property name (required, default `"Title"`).
    pub title: String,
    /// Notion `rich_text` deduplication property name (required, default `"FC-ID"`).
    pub fc_id: String,
    /// Notion `url` property name (default `Some("Link")`).
    #[serde(default)]
    pub link: Option<String>,
    /// Notion `select` property name for feed title (default `Some("Source")`).
    #[serde(default)]
    pub source: Option<String>,
    /// Notion `date` property name for original publish timestamp (default `Some("Published")`).
    #[serde(default)]
    pub published: Option<String>,
    /// Notion `select` property name initialized to `"UnRead"` (default `Some("State")`).
    #[serde(default)]
    pub state: Option<String>,
    /// Notion `multi_select` property name synced from feed tags (default `Some("Tag")`).
    #[serde(default)]
    pub tag: Option<String>,
}

impl Default for NotionColumnMapping {
    fn default() -> Self {
        Self::standard()
    }
}

impl NotionColumnMapping {
    /// Standard 7-column Notion schema created by `init-db`.
    pub fn standard() -> Self {
        Self {
            title: "Title".to_string(),
            fc_id: "FC-ID".to_string(),
            link: Some("Link".to_string()),
            source: Some("Source".to_string()),
            published: Some("Published".to_string()),
            state: Some("State".to_string()),
            tag: Some("Tag".to_string()),
        }
    }

    /// Ensure mandatory `title` and `fc_id` columns are mapped.
    pub fn validate(&self) -> AppResult<()> {
        if self.title.trim().is_empty() {
            return Err(AppError::Validation(
                "Notion column mapping requires a non-empty 'title' property name".to_string(),
            ));
        }
        if self.fc_id.trim().is_empty() {
            return Err(AppError::Validation(
                "Notion column mapping requires a non-empty 'fc_id' (FC-ID) property name"
                    .to_string(),
            ));
        }
        Ok(())
    }
}

/// Decrypted Notion data source credentials and property mapping (`Notion-Version: 2026-03-11`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotionChannelConfig {
    pub integration_secret: String,
    pub data_source_id: String,
    #[serde(default)]
    pub database_id: Option<String>,
    #[serde(default)]
    pub parent_page_id: Option<String>,
    #[serde(default)]
    pub column_mapping: NotionColumnMapping,
}

/// Decrypted channel configuration stored encrypted in `channels.encrypted_config`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChannelConfig {
    Smtp(SmtpChannelConfig),
    Telegram(TelegramChannelConfig),
    Notion(NotionChannelConfig),
}

impl ChannelConfig {
    /// Discriminant kind matching `channels.kind`.
    pub const fn kind(&self) -> ChannelKind {
        match self {
            Self::Smtp(_) => ChannelKind::Smtp,
            Self::Telegram(_) => ChannelKind::Telegram,
            Self::Notion(_) => ChannelKind::Notion,
        }
    }

    /// Compute shared rate-limit group key (`channels.credential_group_key`) so multiple channels
    /// sharing the same SMTP account, Bot token, or Notion integration share pacing & cooldowns.
    pub fn credential_group_key(&self) -> String {
        match self {
            Self::Smtp(cfg) => {
                let identity = cfg
                    .username
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or(&cfg.from_address);
                compute_credential_group_key(
                    "smtp",
                    &format!(
                        "{}@{}:{}",
                        identity.trim().to_ascii_lowercase(),
                        cfg.host.trim().to_ascii_lowercase(),
                        cfg.port
                    ),
                )
            }
            Self::Telegram(cfg) => {
                let base = cfg
                    .api_base_url
                    .as_deref()
                    .unwrap_or("https://api.telegram.org")
                    .trim_end_matches('/')
                    .to_ascii_lowercase();
                compute_credential_group_key(
                    "telegram",
                    &format!("{base}:{}", cfg.bot_token.trim()),
                )
            }
            Self::Notion(cfg) => {
                compute_credential_group_key("notion", cfg.integration_secret.trim())
            }
        }
    }

    /// Validate required fields before persisting a channel.
    pub fn validate(&self) -> AppResult<()> {
        match self {
            Self::Smtp(cfg) => {
                if cfg.host.trim().is_empty() {
                    return Err(AppError::Validation(
                        "SMTP host must not be empty".to_string(),
                    ));
                }
                if cfg.port == 0 {
                    return Err(AppError::Validation(
                        "SMTP port must be greater than 0".to_string(),
                    ));
                }
                if cfg.from_address.trim().is_empty() {
                    return Err(AppError::Validation(
                        "SMTP from_address must not be empty".to_string(),
                    ));
                }
                if cfg.to_addresses.is_empty()
                    || cfg.to_addresses.iter().all(|addr| addr.trim().is_empty())
                {
                    return Err(AppError::Validation(
                        "SMTP to_addresses must contain at least one recipient".to_string(),
                    ));
                }
                Ok(())
            }
            Self::Telegram(cfg) => {
                if cfg.bot_token.trim().is_empty() {
                    return Err(AppError::Validation(
                        "Telegram bot_token must not be empty".to_string(),
                    ));
                }
                if cfg.chat_id.trim().is_empty() {
                    return Err(AppError::Validation(
                        "Telegram chat_id must not be empty".to_string(),
                    ));
                }
                Ok(())
            }
            Self::Notion(cfg) => {
                if cfg.integration_secret.trim().is_empty() {
                    return Err(AppError::Validation(
                        "Notion integration_secret must not be empty".to_string(),
                    ));
                }
                if cfg.data_source_id.trim().is_empty() {
                    return Err(AppError::Validation(
                        "Notion data_source_id must not be empty".to_string(),
                    ));
                }
                cfg.column_mapping.validate()
            }
        }
    }

    /// Produce a redacted JSON representation safe to return to the WebUI (`GET /api/v1/channels`).
    /// Secrets (`password`, `bot_token`, `integration_secret`) are masked.
    pub fn redacted_view(&self) -> serde_json::Value {
        match self {
            Self::Smtp(cfg) => serde_json::json!({
                "kind": "smtp",
                "host": cfg.host,
                "port": cfg.port,
                "username": cfg.username,
                "has_password": cfg.password.as_ref().is_some_and(|p| !p.is_empty()),
                "security": cfg.security,
                "from_address": cfg.from_address,
                "to_addresses": cfg.to_addresses,
                "reply_to": cfg.reply_to,
            }),
            Self::Telegram(cfg) => serde_json::json!({
                "kind": "telegram",
                "bot_token_hint": mask_secret(&cfg.bot_token),
                "chat_id": cfg.chat_id,
                "message_thread_id": cfg.message_thread_id,
                "api_base_url": cfg.api_base_url,
            }),
            Self::Notion(cfg) => serde_json::json!({
                "kind": "notion",
                "secret_hint": mask_secret(&cfg.integration_secret),
                "data_source_id": cfg.data_source_id,
                "database_id": cfg.database_id,
                "parent_page_id": cfg.parent_page_id,
                "column_mapping": cfg.column_mapping,
            }),
        }
    }
}

/// Mask a secret token keeping at most 4 prefix and 4 suffix characters.
pub fn mask_secret(secret: &str) -> String {
    let trimmed = secret.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 8 {
        return "••••••••".to_string();
    }
    let prefix: String = chars[..4].iter().collect();
    let suffix: String = chars[chars.len() - 4..].iter().collect();
    format!("{prefix}••••{suffix}")
}

/// Telegram per-route presentation mode (`feedcourier.md` Section 五.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TelegramDeliveryMode {
    #[default]
    RichMessage,
    CompactCard,
    MediaMode,
}

/// Per-route presentation options stored in `routes.options_json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteOptions {
    #[serde(default)]
    pub telegram_mode: TelegramDeliveryMode,
    #[serde(default = "default_true")]
    pub link_preview: bool,
}

const fn default_true() -> bool {
    true
}

impl Default for RouteOptions {
    fn default() -> Self {
        Self {
            telegram_mode: TelegramDeliveryMode::RichMessage,
            link_preview: true,
        }
    }
}

/// Mid-flight delivery checkpoint persisted in `delivery_tasks.checkpoint_json` for crash/retry recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeliveryCheckpoint {
    Smtp {
        message_id: String,
    },
    Telegram {
        /// Index of the next media batch (groups of 10) to send in `media_mode`.
        #[serde(default)]
        next_batch_offset: usize,
        #[serde(default)]
        sent_message_ids: Vec<i64>,
        #[serde(default)]
        degraded_to_compact: bool,
    },
    Notion {
        /// Set to `true` immediately before issuing `POST /v1/pages` so a crash before receiving
        /// the HTTP response triggers an `FC-ID` lookup before retrying creation.
        #[serde(default)]
        create_initiated: bool,
        /// Populated upon receiving `202 Accepted` when `allow_async: true` is active.
        #[serde(default)]
        async_task_id: Option<String>,
        /// Populated once page creation is confirmed.
        #[serde(default)]
        page_id: Option<String>,
    },
}

/// Callback interface allowing `ChannelSender::send` to flush intermediate checkpoints
/// (e.g. before `POST /v1/pages`, after `202 async_task_id`, or after each `sendMediaGroup` batch)
/// directly to SQLite prior to returning.
#[async_trait]
pub trait CheckpointSink: Send + Sync {
    async fn save_checkpoint(&self, checkpoint: &DeliveryCheckpoint) -> AppResult<()>;
}

/// Full immutable context supplied to a `ChannelSender` for executing a single outbox task.
pub struct DeliveryContext<'a> {
    pub task: &'a DeliveryTask,
    pub entry: &'a Entry,
    pub feed: &'a Feed,
    pub route: &'a Route,
    pub route_options: RouteOptions,
    pub channel: &'a Channel,
    pub channel_config: &'a ChannelConfig,
    pub checkpoint: Option<DeliveryCheckpoint>,
    pub checkpoint_sink: Option<&'a dyn CheckpointSink>,
}

/// Result of attempting a single delivery task via a `ChannelSender`.
#[derive(Debug, Clone)]
pub enum DeliveryOutcome {
    /// Downstream confirmed receipt.
    Delivered {
        status_code: Option<i32>,
        checkpoint: Option<DeliveryCheckpoint>,
        diagnostic: Option<String>,
        updated_capabilities: Option<serde_json::Value>,
    },
    /// Transient failure or downstream rate-limit (`429` / `retry_after` / SMTP `421`/`45x`).
    Retry {
        retry_after_secs: Option<u64>,
        status_code: Option<i32>,
        checkpoint: Option<DeliveryCheckpoint>,
        diagnostic: String,
        /// When `true`, places all channels sharing `credential_group_key` into cooldown.
        cooldown_credential_group: bool,
    },
    /// Hourly or daily quota reached; defer until `next_window_at` without burning retry budget.
    Deferred {
        next_window_at: i64,
        checkpoint: Option<DeliveryCheckpoint>,
        diagnostic: String,
    },
    /// Unrecoverable failure (e.g. invalid credentials, deleted target chat/database); move to `dead_letter`.
    PermanentFailure {
        status_code: Option<i32>,
        checkpoint: Option<DeliveryCheckpoint>,
        diagnostic: String,
    },
}

/// Unified asynchronous sender contract implemented by SMTP, Telegram, and Notion adapters.
#[async_trait]
pub trait ChannelSender: Send + Sync {
    /// Channel kind handled by this adapter.
    fn kind(&self) -> ChannelKind;

    /// Deliver a single entry according to the route's options and existing checkpoint.
    async fn send(&self, ctx: &DeliveryContext<'_>) -> AppResult<DeliveryOutcome>;

    /// Send an interactive connectivity test message/page when configuring a channel in WebUI/MCP.
    async fn send_test(&self, config: &ChannelConfig, use_proxy: bool) -> AppResult<String>;
}
