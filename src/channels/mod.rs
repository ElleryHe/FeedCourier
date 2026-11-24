//! Shared delivery channel domain contracts, two-layer encrypted configuration schemas
//! (Channel Account credentials + Destination targets), route presentation options, checkpoints,
//! and the `ChannelSender` trait.

pub mod notion;
pub mod smtp;
pub mod telegram;

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use async_trait::async_trait;
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::crypto::{compute_credential_group_key, sha256_hex};
use crate::db::models::{AppLocale, Channel, ChannelDestination, DeliveryTask, Entry, Feed, Route};
use crate::error::{AppError, AppResult};
use crate::plugins::RenderOverrides;

/// Supported trusted delivery channel types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum ChannelKind {
    Smtp,
    Telegram,
    Notion,
    Plugin,
}

impl ChannelKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Smtp => "smtp",
            Self::Telegram => "telegram",
            Self::Notion => "notion",
            Self::Plugin => "plugin",
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
            "plugin" => Ok(Self::Plugin),
            other => Err(AppError::Validation(format!(
                "unsupported channel kind '{other}' (expected smtp, telegram, notion, or plugin)"
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

/// Decrypted SMTP account credentials and Force-From envelope settings stored encrypted in
/// `channels.encrypted_config`.
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
    /// Optional sender identity / physical contact line appended to email footers (CAN-SPAM compliance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_identity: Option<String>,
}

/// Decrypted SMTP destination recipients & optional Reply-To stored encrypted in
/// `channel_destinations.encrypted_config`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmtpDestinationConfig {
    /// Recipients locked inside this trusted destination.
    pub to_addresses: Vec<String>,
    #[serde(default)]
    pub reply_to: Option<String>,
}

/// Decrypted Telegram Bot account credentials stored encrypted in `channels.encrypted_config`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramChannelConfig {
    pub bot_token: String,
    /// Optional custom Bot API base URL (defaults to `https://api.telegram.org`).
    #[serde(default)]
    pub api_base_url: Option<String>,
}

/// Decrypted Telegram destination chat/topic stored encrypted in
/// `channel_destinations.encrypted_config`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramDestinationConfig {
    /// Target private user ID, group ID, or `@channelusername`.
    pub chat_id: String,
    /// Optional forum supergroup topic ID (`message_thread_id`).
    #[serde(default)]
    pub message_thread_id: Option<i64>,
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
    pub bot_first_name: Option<String>,
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
    /// Optional Notion `url` property name for shortened link written by Link Rewrite plugin
    /// (default `Some("Short URL")` for newly initialized databases; `None` on legacy mappings).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_url: Option<String>,
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
    /// Conservative default for manual/legacy 7-column Notion DB bindings (`short_url: None` so
    /// routes using Link Rewrite won't write to a missing `Short URL` column and 400-dead-letter).
    /// Newly provisioned databases via `init-db` use [`Self::standard`] (all 8 columns).
    fn default() -> Self {
        Self {
            title: "Title".to_string(),
            fc_id: "FC-ID".to_string(),
            link: Some("Link".to_string()),
            short_url: None,
            source: Some("Source".to_string()),
            published: Some("Published".to_string()),
            state: Some("State".to_string()),
            tag: Some("Tag".to_string()),
        }
    }
}

impl NotionColumnMapping {
    /// Standard 8-column Notion schema created by `init-db` (including optional `Short URL`).
    pub fn standard() -> Self {
        Self {
            title: "Title".to_string(),
            fc_id: "FC-ID".to_string(),
            link: Some("Link".to_string()),
            short_url: Some("Short URL".to_string()),
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

/// Decrypted Notion integration credentials stored encrypted in `channels.encrypted_config`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotionChannelConfig {
    pub integration_secret: String,
}

/// Decrypted Notion target data source and column mapping (`Notion-Version: 2026-03-11`) stored
/// encrypted in `channel_destinations.encrypted_config`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotionDestinationConfig {
    pub data_source_id: String,
    #[serde(default)]
    pub database_id: Option<String>,
    #[serde(default)]
    pub parent_page_id: Option<String>,
    #[serde(default)]
    pub column_mapping: NotionColumnMapping,
}

/// Decrypted account-level configuration for a channel plugin (`ChannelKind::Plugin`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginChannelConfig {
    /// Installed channel plugin ID (`plugins.id` where `kind = 'channel'`).
    pub plugin_id: String,
    /// Schema-validated account configuration object encrypted at rest.
    #[serde(default)]
    pub config: serde_json::Value,
}

/// Decrypted destination-level configuration for a channel plugin (`ChannelKind::Plugin`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginDestinationConfig {
    /// Schema-validated destination target object encrypted at rest.
    #[serde(default)]
    pub config: serde_json::Value,
}

/// Decrypted account-level channel configuration stored encrypted in `channels.encrypted_config`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChannelConfig {
    Smtp(SmtpChannelConfig),
    Telegram(TelegramChannelConfig),
    Notion(NotionChannelConfig),
    Plugin(PluginChannelConfig),
}

impl ChannelConfig {
    /// Discriminant kind matching `channels.kind`.
    pub const fn kind(&self) -> ChannelKind {
        match self {
            Self::Smtp(_) => ChannelKind::Smtp,
            Self::Telegram(_) => ChannelKind::Telegram,
            Self::Notion(_) => ChannelKind::Notion,
            Self::Plugin(_) => ChannelKind::Plugin,
        }
    }

    /// Compute shared rate-limit group key (`channels.credential_group_key`) so all destinations
    /// (and any accounts sharing identical SMTP/Bot/Notion credentials) share pacing, quotas,
    /// and cooldowns.
    ///
    /// For SMTP, the SHA-256 digest of `password` is folded into the key so distinct API keys
    /// under a shared username (e.g. SendGrid `apikey`) stay isolated from one another.
    pub fn credential_group_key(&self) -> String {
        match self {
            Self::Smtp(cfg) => {
                let identity = cfg
                    .username
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or(&cfg.from_address);
                let pw_digest = cfg
                    .password
                    .as_deref()
                    .filter(|p| !p.is_empty())
                    .map(|p| sha256_hex(p.as_bytes()))
                    .unwrap_or_default();
                compute_credential_group_key(
                    "smtp",
                    &format!(
                        "{}@{}:{}:{}",
                        identity.trim().to_ascii_lowercase(),
                        cfg.host.trim().to_ascii_lowercase(),
                        cfg.port,
                        pw_digest
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
            Self::Plugin(cfg) => {
                let canonical = serde_json::to_string(&cfg.config).unwrap_or_default();
                let digest = sha256_hex(canonical.as_bytes());
                compute_credential_group_key(
                    "plugin",
                    &format!("{}:{digest}", cfg.plugin_id.trim()),
                )
            }
        }
    }

    /// Strict identity key used by the v0.1.x → v0.2.0 automatic migration to decide whether two
    /// legacy channels represent the exact same Channel Account and can be merged without losing
    /// any credentials, TLS mode, or sender identity.
    pub fn migration_merge_key(&self) -> String {
        match self {
            Self::Smtp(cfg) => {
                let username = cfg
                    .username
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or("")
                    .to_ascii_lowercase();
                let pw_digest = cfg
                    .password
                    .as_deref()
                    .filter(|p| !p.is_empty())
                    .map(|p| sha256_hex(p.as_bytes()))
                    .unwrap_or_default();
                let security = match cfg.security {
                    SmtpSecurity::StartTls => "start_tls",
                    SmtpSecurity::Tls => "tls",
                    SmtpSecurity::Plain => "plain",
                };
                compute_credential_group_key(
                    "smtp_merge",
                    &format!(
                        "{}|{}|{}|{}|{}|{}",
                        cfg.host.trim().to_ascii_lowercase(),
                        cfg.port,
                        username,
                        pw_digest,
                        security,
                        cfg.from_address.trim().to_ascii_lowercase()
                    ),
                )
            }
            Self::Telegram(_) | Self::Notion(_) | Self::Plugin(_) => self.credential_group_key(),
        }
    }

    /// Validate required account-level fields before persisting a channel.
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
                let from_trimmed = cfg.from_address.trim();
                if from_trimmed.is_empty() {
                    return Err(AppError::Validation(
                        "SMTP from_address must not be empty".to_string(),
                    ));
                }
                let from_ascii = crate::crypto::email_to_ascii_punycode(from_trimmed);
                if from_ascii.parse::<lettre::Address>().is_err() {
                    return Err(AppError::Validation(format!(
                        "invalid SMTP from_address '{from_trimmed}'"
                    )));
                }
                Ok(())
            }
            Self::Telegram(cfg) => {
                if cfg.bot_token.trim().is_empty() {
                    return Err(AppError::Validation(
                        "Telegram bot_token must not be empty".to_string(),
                    ));
                }
                if let Some(raw_base) = cfg
                    .api_base_url
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    validate_telegram_api_base_url(raw_base)?;
                }
                Ok(())
            }
            Self::Notion(cfg) => {
                if cfg.integration_secret.trim().is_empty() {
                    return Err(AppError::Validation(
                        "Notion integration_secret must not be empty".to_string(),
                    ));
                }
                Ok(())
            }
            Self::Plugin(cfg) => {
                if cfg.plugin_id.trim().is_empty() {
                    return Err(AppError::Validation(
                        "Plugin channel plugin_id must not be empty".to_string(),
                    ));
                }
                if !cfg.config.is_object() {
                    return Err(AppError::Validation(
                        "Plugin channel config must be a JSON object".to_string(),
                    ));
                }
                Ok(())
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
                "sender_identity": cfg.sender_identity,
            }),
            Self::Telegram(cfg) => serde_json::json!({
                "kind": "telegram",
                "bot_token_hint": mask_secret(&cfg.bot_token),
                "api_base_url": cfg.api_base_url,
            }),
            Self::Notion(cfg) => serde_json::json!({
                "kind": "notion",
                "secret_hint": mask_secret(&cfg.integration_secret),
            }),
            Self::Plugin(cfg) => serde_json::json!({
                "kind": "plugin",
                "plugin_id": cfg.plugin_id,
                "config": crate::plugins::js_runtime::redact_config_generic(&cfg.config),
            }),
        }
    }
}

/// Decrypted destination configuration stored encrypted in `channel_destinations.encrypted_config`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum DestinationConfig {
    Smtp(SmtpDestinationConfig),
    Telegram(TelegramDestinationConfig),
    Notion(NotionDestinationConfig),
    Plugin(PluginDestinationConfig),
}

impl DestinationConfig {
    /// Discriminant kind matching the parent channel's `channels.kind`.
    pub const fn kind(&self) -> ChannelKind {
        match self {
            Self::Smtp(_) => ChannelKind::Smtp,
            Self::Telegram(_) => ChannelKind::Telegram,
            Self::Notion(_) => ChannelKind::Notion,
            Self::Plugin(_) => ChannelKind::Plugin,
        }
    }

    /// Validate required destination target fields before persisting.
    pub fn validate(&self) -> AppResult<()> {
        match self {
            Self::Smtp(cfg) => {
                if cfg.to_addresses.is_empty()
                    || cfg.to_addresses.iter().all(|addr| addr.trim().is_empty())
                {
                    return Err(AppError::Validation(
                        "SMTP to_addresses must contain at least one recipient".to_string(),
                    ));
                }
                for raw in &cfg.to_addresses {
                    let trimmed = raw.trim();
                    if !trimmed.is_empty() {
                        let ascii = crate::crypto::email_to_ascii_punycode(trimmed);
                        if ascii.parse::<lettre::message::Mailbox>().is_err() {
                            return Err(AppError::Validation(format!(
                                "invalid SMTP recipient address '{trimmed}'"
                            )));
                        }
                    }
                }
                if let Some(rt) = cfg
                    .reply_to
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    let rt_ascii = crate::crypto::email_to_ascii_punycode(rt);
                    if rt_ascii.parse::<lettre::message::Mailbox>().is_err() {
                        return Err(AppError::Validation(format!(
                            "invalid SMTP reply_to address '{rt}'"
                        )));
                    }
                }
                Ok(())
            }
            Self::Telegram(cfg) => {
                if cfg.chat_id.trim().is_empty() {
                    return Err(AppError::Validation(
                        "Telegram chat_id must not be empty".to_string(),
                    ));
                }
                Ok(())
            }
            Self::Notion(cfg) => {
                if cfg.data_source_id.trim().is_empty() {
                    return Err(AppError::Validation(
                        "Notion data_source_id must not be empty".to_string(),
                    ));
                }
                cfg.column_mapping.validate()
            }
            Self::Plugin(cfg) => {
                if !cfg.config.is_object() {
                    return Err(AppError::Validation(
                        "Plugin destination config must be a JSON object".to_string(),
                    ));
                }
                Ok(())
            }
        }
    }

    /// Produce a JSON representation safe to return to the WebUI (`GET /api/v1/channels`).
    pub fn view(&self) -> serde_json::Value {
        match self {
            Self::Smtp(cfg) => serde_json::json!({
                "kind": "smtp",
                "to_addresses": cfg.to_addresses,
                "reply_to": cfg.reply_to,
            }),
            Self::Telegram(cfg) => serde_json::json!({
                "kind": "telegram",
                "chat_id": cfg.chat_id,
                "message_thread_id": cfg.message_thread_id,
            }),
            Self::Notion(cfg) => serde_json::json!({
                "kind": "notion",
                "data_source_id": cfg.data_source_id,
                "database_id": cfg.database_id,
                "parent_page_id": cfg.parent_page_id,
                "column_mapping": cfg.column_mapping,
            }),
            Self::Plugin(cfg) => serde_json::json!({
                "kind": "plugin",
                "config": crate::plugins::js_runtime::redact_config_generic(&cfg.config),
            }),
        }
    }
}

/// Pre-v0.2.0 combined channel configuration schema used strictly by the automatic startup
/// migration (`Database::migrate_channels_to_v2`) to split legacy `channels.encrypted_config`
/// blobs into `(ChannelConfig, DestinationConfig)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LegacyChannelConfig {
    Smtp {
        host: String,
        port: u16,
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        password: Option<String>,
        #[serde(default)]
        security: SmtpSecurity,
        from_address: String,
        to_addresses: Vec<String>,
        #[serde(default)]
        reply_to: Option<String>,
    },
    Telegram {
        bot_token: String,
        chat_id: String,
        #[serde(default)]
        message_thread_id: Option<i64>,
        #[serde(default)]
        api_base_url: Option<String>,
    },
    Notion {
        integration_secret: String,
        data_source_id: String,
        #[serde(default)]
        database_id: Option<String>,
        #[serde(default)]
        parent_page_id: Option<String>,
        #[serde(default)]
        column_mapping: NotionColumnMapping,
    },
}

impl LegacyChannelConfig {
    /// Split a legacy v0.1.x configuration into its v0.2.0 account + destination halves.
    pub fn into_split(self) -> (ChannelConfig, DestinationConfig) {
        match self {
            Self::Smtp {
                host,
                port,
                username,
                password,
                security,
                from_address,
                to_addresses,
                reply_to,
            } => (
                ChannelConfig::Smtp(SmtpChannelConfig {
                    host,
                    port,
                    username,
                    password,
                    security,
                    from_address,
                    sender_identity: None,
                }),
                DestinationConfig::Smtp(SmtpDestinationConfig {
                    to_addresses,
                    reply_to,
                }),
            ),
            Self::Telegram {
                bot_token,
                chat_id,
                message_thread_id,
                api_base_url,
            } => (
                ChannelConfig::Telegram(TelegramChannelConfig {
                    bot_token,
                    api_base_url,
                }),
                DestinationConfig::Telegram(TelegramDestinationConfig {
                    chat_id,
                    message_thread_id,
                }),
            ),
            Self::Notion {
                integration_secret,
                data_source_id,
                database_id,
                parent_page_id,
                column_mapping,
            } => (
                ChannelConfig::Notion(NotionChannelConfig { integration_secret }),
                DestinationConfig::Notion(NotionDestinationConfig {
                    data_source_id,
                    database_id,
                    parent_page_id,
                    column_mapping,
                }),
            ),
        }
    }
}

/// Validate a custom Telegram `api_base_url`: only `http://` or `https://` with an explicit host,
/// and no userinfo (`user:pass@`), query string, or fragment.
pub fn validate_telegram_api_base_url(raw: &str) -> AppResult<url::Url> {
    let trimmed = raw.trim();
    let parsed = url::Url::parse(trimmed).map_err(|err| {
        AppError::Validation(format!("invalid Telegram api_base_url '{trimmed}': {err}"))
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(AppError::Validation(format!(
            "Telegram api_base_url must use http or https scheme, got '{}'",
            parsed.scheme()
        )));
    }
    if parsed.host_str().is_none() {
        return Err(AppError::Validation(
            "Telegram api_base_url must include a host".to_string(),
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(AppError::Validation(
            "Telegram api_base_url must not contain embedded credentials (userinfo)".to_string(),
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(AppError::Validation(
            "Telegram api_base_url must not contain query parameters or fragments".to_string(),
        ));
    }
    Ok(parsed)
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

/// Daily delivery window (`HH:MM`–`HH:MM`, route or system timezone). `end < start` wraps past
/// midnight; `start == end` means the whole day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryTimeWindow {
    pub start: String,
    pub end: String,
}

/// Per-route presentation and scheduling options stored in `routes.options_json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteOptions {
    #[serde(default)]
    pub telegram_mode: TelegramDeliveryMode,
    #[serde(default = "default_true")]
    pub link_preview: bool,
    /// Delivery template language of this route. `None` inherits the legacy system locale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<AppLocale>,
    /// Optional per-route IANA timezone (e.g. `"Asia/Shanghai"`). `None` inherits the system
    /// timezone (`system_settings.timezone`). Controls delivery windows, SMTP article timestamps,
    /// and Telegram rich-message footer timestamps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Daily windows in the route timezone during which this route may deliver (empty = always).
    #[serde(default)]
    pub active_windows: Vec<DeliveryTimeWindow>,
    /// ISO weekdays (`1` = Monday … `7` = Sunday) on which windows start (empty = every day).
    #[serde(default)]
    pub active_weekdays: Vec<u8>,
    /// Per-plugin route options keyed by plugin ID (e.g. `{"link-rewrite": {"provider_id": 3}}`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<String, serde_json::Value>,
    /// Optional explicit execution order for `on_render` plugins on this route.
    /// Plugins present in `plugins` but omitted from `plugin_order` run afterwards in ID order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugin_order: Vec<String>,
}

const fn default_true() -> bool {
    true
}

impl Default for RouteOptions {
    fn default() -> Self {
        Self {
            telegram_mode: TelegramDeliveryMode::RichMessage,
            link_preview: true,
            locale: None,
            timezone: None,
            active_windows: Vec::new(),
            active_weekdays: Vec::new(),
            plugins: BTreeMap::new(),
            plugin_order: Vec::new(),
        }
    }
}

/// Mid-flight delivery checkpoint persisted in `delivery_tasks.checkpoint_json` for crash/retry recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeliveryCheckpoint {
    Smtp {
        message_id: String,
        /// `address_hmac` entries of recipients already delivered in a multi-recipient SMTP task.
        #[serde(default)]
        sent_recipients: Vec<String>,
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
    /// Opaque channel plugin checkpoint state (capped at 16 KiB).
    Plugin {
        #[serde(default)]
        state: serde_json::Value,
    },
}

/// Individual verified SMTP recipient prepared for single-recipient compliant delivery.
#[derive(Debug, Clone)]
pub struct SmtpDeliveryRecipient {
    pub recipient_id: i64,
    pub email: String,
    pub address_hmac: String,
    /// Optional RFC 8058 one-click unsubscribe URL (`{PUBLIC_URL}/u/{token}`).
    pub unsubscribe_url: Option<String>,
}

/// Compliance delivery plan attached to [`DeliveryContext`] for verified per-recipient SMTP sends.
#[derive(Debug, Clone)]
pub struct SmtpDeliveryPlan {
    pub recipients: Vec<SmtpDeliveryRecipient>,
    /// Delay between individual recipients within the same task (default `1s`; `0` in fast tests).
    pub per_recipient_delay: std::time::Duration,
}

/// Callback interface allowing `ChannelSender::send` to flush intermediate checkpoints,
/// write back lazily resolved destination configs (e.g. Notion `database_id -> data_source_id`),
/// renew task leases during multi-recipient sends, and record SMTP hard bounces.
#[async_trait]
pub trait CheckpointSink: Send + Sync {
    async fn save_checkpoint(&self, checkpoint: &DeliveryCheckpoint) -> AppResult<()>;

    /// Persist a lazily resolved/updated destination config (e.g. Notion `data_source_id`).
    async fn update_destination_config(&self, _config: &DestinationConfig) -> AppResult<()> {
        Ok(())
    }

    /// Extend the active outbox task lease during long multi-recipient sends.
    async fn renew_lease(&self) -> AppResult<()> {
        Ok(())
    }

    /// Mark an SMTP recipient row as `bounced` after receiving a synchronous 5xx SMTP failure.
    async fn record_smtp_hard_bounce(
        &self,
        _recipient_id: i64,
        _diagnostic: &str,
    ) -> AppResult<()> {
        Ok(())
    }
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
    pub destination: &'a ChannelDestination,
    pub destination_config: &'a DestinationConfig,
    pub timezone: Tz,
    pub checkpoint: Option<DeliveryCheckpoint>,
    pub checkpoint_sink: Option<&'a dyn CheckpointSink>,
    /// Optional content/link mutations produced by enabled `on_render` plugins.
    pub overrides: RenderOverrides,
    /// Optional per-recipient SMTP compliance delivery plan (set by outbox scheduler).
    pub smtp_delivery: Option<SmtpDeliveryPlan>,
}

impl<'a> DeliveryContext<'a> {
    /// Effective HTML body honoring any `on_render` plugin override.
    pub fn effective_content_html(&self) -> Option<&str> {
        self.overrides.effective_content_html(self.entry)
    }

    /// Effective primary article link honoring any `on_render` plugin override.
    ///
    /// Note: Telegram `link_preview_options.url` and Notion's `Link` property intentionally read
    /// `self.entry.canonical_url` directly so link previews and archive references remain durable.
    pub fn effective_primary_link(&self) -> Option<&str> {
        self.overrides.effective_primary_link(self.entry)
    }
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

    /// Send an interactive connectivity test message/page to a specific destination under an account.
    async fn send_test(
        &self,
        channel_config: &ChannelConfig,
        destination_config: &DestinationConfig,
        use_proxy: bool,
    ) -> AppResult<String>;
}
