//! Runtime configuration and build metadata constants.

use std::net::SocketAddr;
use std::path::PathBuf;

use crate::db::models::AppLocale;
use crate::error::{AppError, AppResult};

/// Canonical application name.
pub const APP_NAME: &str = "FeedCourier";

/// Build version sourced from `Cargo.toml` (`CARGO_PKG_VERSION`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Upstream source repository URL (required by AGPL-3.0 Section 13 in WebUI & MCP ServerInfo).
pub const REPO_URL: &str = "https://github.com/ElleryHe/FeedCourier";

/// License identifier for FeedCourier.
pub const LICENSE_SPDX: &str = "AGPL-3.0";

/// Default persistent data directory.
pub const DEFAULT_DATA_DIR: &str = "./data";

/// Default HTTP listen socket address.
pub const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:8080";

/// Default entry payload retention window before GC (in hours).
pub const DEFAULT_RETENTION_HOURS: u64 = 24;

/// Default maximum concurrent SQLite read connections.
pub const DEFAULT_READ_POOL_MAX_CONNECTIONS: u32 = 8;

/// Runtime configuration resolved from environment variables (with safe defaults).
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// Directory storing `feedcourier.db`, `master.key`, and online backups (`DATA_DIR`).
    pub data_dir: PathBuf,
    /// HTTP bind address for WebUI, REST API, and `/mcp` (`LISTEN_ADDR`).
    pub listen_addr: SocketAddr,
    /// Optional explicit master key in 64-char hex or base64 (`MASTER_KEY`).
    /// When omitted, `<data_dir>/master.key` is auto-generated with `0600` permissions.
    pub master_key: Option<String>,
    /// Optional initial/override WebUI administrator password (`ADMIN_PASSWORD`).
    pub admin_password: Option<String>,
    /// Initial CIDR / hostname entries permitted through the SSRF guard (`SSRF_ALLOWLIST`).
    pub ssrf_allowlist: Vec<String>,
    /// Optional default outbound HTTP/SOCKS5 proxy URL (`OUTBOUND_PROXY`).
    pub outbound_proxy: Option<String>,
    /// Retention window in hours before delivered/unrouted entry bodies are purged (`RETENTION_HOURS`).
    pub retention_hours: u64,
    /// Maximum connections in the concurrent SQLite read pool (`DB_READ_MAX_CONNECTIONS`).
    pub read_pool_max_connections: u32,
    /// Optional initial locale for delivery templates (`LOCALE`, e.g. `en` or `zh-CN`).
    pub locale: Option<AppLocale>,
    /// Optional WebUI administrator username that overrides the stored one at startup (`ADMIN_USERNAME`).
    pub admin_username: Option<String>,
    /// Optional initial IANA system timezone (`TIMEZONE`, falling back to a valid `TZ`).
    pub timezone: Option<String>,
    /// One-shot emergency removal of the administrator TOTP secret and recovery codes (`RESET_2FA`).
    pub reset_2fa: bool,
    /// Optional public base URL (e.g. `https://feeds.example.com`) enabling RFC 8058 one-click
    /// SMTP unsubscribe links (`/u/{token}`) and double-opt-in verification links (`/v/{token}`).
    pub public_url: Option<String>,
    /// Optional custom plugin marketplace `index.json` URL (`PLUGIN_INDEX_URL`).
    pub plugin_index_url: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from(DEFAULT_DATA_DIR),
            listen_addr: DEFAULT_LISTEN_ADDR
                .parse()
                .expect("DEFAULT_LISTEN_ADDR must be a valid SocketAddr"),
            master_key: None,
            admin_password: None,
            ssrf_allowlist: Vec::new(),
            outbound_proxy: None,
            retention_hours: DEFAULT_RETENTION_HOURS,
            read_pool_max_connections: DEFAULT_READ_POOL_MAX_CONNECTIONS,
            locale: None,
            admin_username: None,
            timezone: None,
            reset_2fa: false,
            public_url: None,
            plugin_index_url: None,
        }
    }
}

impl AppConfig {
    /// Load configuration from process environment variables.
    pub fn from_env() -> AppResult<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Load configuration using a custom lookup closure (deterministic in unit tests).
    pub fn from_lookup<F>(lookup: F) -> AppResult<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        let non_empty = |key: &str| {
            lookup(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };

        let data_dir = non_empty("DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR));

        let listen_addr_raw =
            non_empty("LISTEN_ADDR").unwrap_or_else(|| DEFAULT_LISTEN_ADDR.to_string());
        let listen_addr: SocketAddr = listen_addr_raw.parse().map_err(|err| {
            AppError::Config(format!("invalid LISTEN_ADDR '{listen_addr_raw}': {err}"))
        })?;

        let master_key = non_empty("MASTER_KEY");
        let admin_password = lookup("ADMIN_PASSWORD").filter(|v| !v.is_empty());

        let ssrf_allowlist = non_empty("SSRF_ALLOWLIST")
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default();

        let outbound_proxy = non_empty("OUTBOUND_PROXY")
            .or_else(|| non_empty("ALL_PROXY"))
            .or_else(|| non_empty("HTTPS_PROXY"))
            .or_else(|| non_empty("HTTP_PROXY"));

        let retention_hours = match non_empty("RETENTION_HOURS") {
            Some(raw) => raw.parse::<u64>().map_err(|err| {
                AppError::Config(format!("invalid RETENTION_HOURS '{raw}': {err}"))
            })?,
            None => DEFAULT_RETENTION_HOURS,
        };

        let read_pool_max_connections = match non_empty("DB_READ_MAX_CONNECTIONS") {
            Some(raw) => {
                let parsed = raw.parse::<u32>().map_err(|err| {
                    AppError::Config(format!("invalid DB_READ_MAX_CONNECTIONS '{raw}': {err}"))
                })?;
                if parsed == 0 {
                    return Err(AppError::Config(
                        "DB_READ_MAX_CONNECTIONS must be at least 1".to_string(),
                    ));
                }
                parsed
            }
            None => DEFAULT_READ_POOL_MAX_CONNECTIONS,
        };

        let locale = match non_empty("LOCALE") {
            Some(raw) => Some(AppLocale::parse_lossy(&raw).ok_or_else(|| {
                AppError::Config(format!("invalid LOCALE '{raw}' (expected 'en' or 'zh-CN')"))
            })?),
            None => None,
        };

        let admin_username = non_empty("ADMIN_USERNAME");
        if let Some(name) = admin_username.as_deref() {
            validate_admin_username(name)?;
        }

        let timezone = match non_empty("TIMEZONE") {
            Some(raw) => {
                crate::schedule::parse_timezone(&raw).map_err(|_| {
                    AppError::Config(format!("invalid TIMEZONE '{raw}' (expected an IANA name)"))
                })?;
                Some(raw)
            }
            // A container-level `TZ` may be unset or non-IANA (e.g. `:/etc/localtime`); ignore then.
            None => non_empty("TZ").filter(|raw| crate::schedule::parse_timezone(raw).is_ok()),
        };

        let reset_2fa = non_empty("RESET_2FA").is_some_and(|raw| {
            matches!(
                raw.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        });

        let public_url = match non_empty("PUBLIC_URL") {
            Some(raw) => Some(validate_public_url(&raw)?),
            None => None,
        };

        let plugin_index_url = match non_empty("PLUGIN_INDEX_URL") {
            Some(raw) => {
                let parsed = url::Url::parse(&raw).map_err(|err| {
                    AppError::Config(format!("invalid PLUGIN_INDEX_URL '{raw}': {err}"))
                })?;
                if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                    return Err(AppError::Config(format!(
                        "invalid PLUGIN_INDEX_URL '{raw}': must be an http/https URL with a host"
                    )));
                }
                Some(raw)
            }
            None => None,
        };

        Ok(Self {
            data_dir,
            listen_addr,
            master_key,
            admin_password,
            ssrf_allowlist,
            outbound_proxy,
            retention_hours,
            read_pool_max_connections,
            locale,
            admin_username,
            timezone,
            reset_2fa,
            public_url,
            plugin_index_url,
        })
    }

    /// Construct a test/custom configuration rooted at `data_dir`.
    pub fn with_data_dir(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            ..Self::default()
        }
    }

    /// Path to the SQLite database file (`<data_dir>/feedcourier.db`).
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("feedcourier.db")
    }

    /// Path to the auto-generated master key file (`<data_dir>/master.key`).
    pub fn master_key_path(&self) -> PathBuf {
        self.data_dir.join("master.key")
    }

    /// Retention window converted to seconds.
    pub fn retention_secs(&self) -> i64 {
        i64::try_from(self.retention_hours.saturating_mul(3600)).unwrap_or(i64::MAX)
    }
}

/// Validate an administrator username (1–64 chars, no control characters, trimmed).
pub fn validate_admin_username(raw: &str) -> AppResult<()> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 64 {
        return Err(AppError::Validation(
            "username must be between 1 and 64 characters".to_string(),
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(AppError::Validation(
            "username must not contain control characters".to_string(),
        ));
    }
    Ok(())
}

/// Validate and normalize `PUBLIC_URL` (must be an absolute `http://` or `https://` URL with
/// host and without embedded userinfo, query string, or fragment; trailing `/` is stripped).
pub fn validate_public_url(raw: &str) -> AppResult<String> {
    let trimmed = raw.trim();
    let parsed = url::Url::parse(trimmed)
        .map_err(|err| AppError::Validation(format!("invalid PUBLIC_URL '{trimmed}': {err}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(AppError::Validation(format!(
            "PUBLIC_URL must use http or https scheme, got '{}'",
            parsed.scheme()
        )));
    }
    if parsed.host_str().is_none() {
        return Err(AppError::Validation(
            "PUBLIC_URL must include a hostname".to_string(),
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(AppError::Validation(
            "PUBLIC_URL must not contain embedded credentials".to_string(),
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(AppError::Validation(
            "PUBLIC_URL must not contain query parameters or fragments".to_string(),
        ));
    }
    Ok(parsed.as_str().trim_end_matches('/').to_string())
}
