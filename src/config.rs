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

/// Default log retention window in days (`LOG_RETENTION_DAYS`).
pub const DEFAULT_LOG_RETENTION_DAYS: u32 = 7;

/// Default maximum rows kept in SQLite `logs.db` (`LOG_MAX_ROWS`).
pub const DEFAULT_LOG_MAX_ROWS: u64 = 100_000;

/// Default ClickHouse database name (`LOG_CLICKHOUSE_DATABASE`).
pub const DEFAULT_LOG_CLICKHOUSE_DATABASE: &str = "feedcourier";

/// Default ClickHouse table name (`LOG_CLICKHOUSE_TABLE`).
pub const DEFAULT_LOG_CLICKHOUSE_TABLE: &str = "logs";

/// Stdout log serialization format (`LOG_FORMAT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

impl LogFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
        }
    }
}

/// Internal log storage backend (`LOG_STORE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogStoreKind {
    #[default]
    Sqlite,
    ClickHouse,
    Off,
}

impl LogStoreKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::ClickHouse => "clickhouse",
            Self::Off => "off",
        }
    }
}

/// Logging output, persistence, retention, and optional ClickHouse configuration.
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Stdout output format (`LOG_FORMAT=text|json`).
    pub format: LogFormat,
    /// Persistent log store backend (`LOG_STORE=sqlite|clickhouse|off`).
    pub store: LogStoreKind,
    /// Minimum level persisted into the log store (`LOG_STORE_LEVEL`, default `"info"`).
    pub store_level: String,
    /// Retention window in days for SQLite GC and ClickHouse table TTL (`LOG_RETENTION_DAYS`).
    pub retention_days: u32,
    /// Maximum rows retained in SQLite `logs.db` (`LOG_MAX_ROWS`, ignored by ClickHouse).
    pub max_rows: u64,
    /// ClickHouse HTTP endpoint URL (`LOG_CLICKHOUSE_URL`, e.g. `http://clickhouse:8123`).
    pub clickhouse_url: Option<String>,
    /// ClickHouse database name (`LOG_CLICKHOUSE_DATABASE`, default `"feedcourier"`).
    pub clickhouse_database: String,
    /// ClickHouse table name (`LOG_CLICKHOUSE_TABLE`, default `"logs"`).
    pub clickhouse_table: String,
    /// Optional ClickHouse username (`LOG_CLICKHOUSE_USER`).
    pub clickhouse_user: Option<String>,
    /// Optional ClickHouse password (`LOG_CLICKHOUSE_PASSWORD`).
    pub clickhouse_password: Option<String>,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            format: LogFormat::Text,
            store: LogStoreKind::Sqlite,
            store_level: "info".to_string(),
            retention_days: DEFAULT_LOG_RETENTION_DAYS,
            max_rows: DEFAULT_LOG_MAX_ROWS,
            clickhouse_url: None,
            clickhouse_database: DEFAULT_LOG_CLICKHOUSE_DATABASE.to_string(),
            clickhouse_table: DEFAULT_LOG_CLICKHOUSE_TABLE.to_string(),
            clickhouse_user: None,
            clickhouse_password: None,
        }
    }
}

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
    /// Whether to trust reverse-proxy client IP headers (`X-Real-IP` / rightmost `X-Forwarded-For`)
    /// on public compliance endpoints (`/u/`, `/v/`). Defaults to `false` (use TCP `SocketAddr`).
    pub trust_proxy: bool,
    /// Structured logging format, persistent log storage, retention, and optional ClickHouse config.
    pub logging: LogConfig,
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
            trust_proxy: false,
            logging: LogConfig::default(),
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

        let trust_proxy = non_empty("TRUST_PROXY").is_some_and(|raw| {
            matches!(
                raw.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        });

        let log_format = match non_empty("LOG_FORMAT") {
            Some(raw) => match raw.to_ascii_lowercase().as_str() {
                "text" => LogFormat::Text,
                "json" => LogFormat::Json,
                _ => {
                    return Err(AppError::Config(format!(
                        "invalid LOG_FORMAT '{raw}' (expected 'text' or 'json')"
                    )));
                }
            },
            None => LogFormat::Text,
        };

        let log_store = match non_empty("LOG_STORE") {
            Some(raw) => match raw.to_ascii_lowercase().as_str() {
                "sqlite" => LogStoreKind::Sqlite,
                "clickhouse" => LogStoreKind::ClickHouse,
                "off" | "none" | "disabled" => LogStoreKind::Off,
                _ => {
                    return Err(AppError::Config(format!(
                        "invalid LOG_STORE '{raw}' (expected 'sqlite', 'clickhouse', or 'off')"
                    )));
                }
            },
            None => LogStoreKind::Sqlite,
        };

        let log_store_level = match non_empty("LOG_STORE_LEVEL") {
            Some(raw) => {
                let lower = raw.to_ascii_lowercase();
                if !matches!(
                    lower.as_str(),
                    "trace" | "debug" | "info" | "warn" | "error"
                ) {
                    return Err(AppError::Config(format!(
                        "invalid LOG_STORE_LEVEL '{raw}' (expected trace, debug, info, warn, or error)"
                    )));
                }
                lower
            }
            None => "info".to_string(),
        };

        let log_retention_days = match non_empty("LOG_RETENTION_DAYS") {
            Some(raw) => {
                let parsed = raw.parse::<u32>().map_err(|err| {
                    AppError::Config(format!("invalid LOG_RETENTION_DAYS '{raw}': {err}"))
                })?;
                if parsed == 0 {
                    return Err(AppError::Config(
                        "LOG_RETENTION_DAYS must be at least 1".to_string(),
                    ));
                }
                parsed
            }
            None => DEFAULT_LOG_RETENTION_DAYS,
        };

        let log_max_rows = match non_empty("LOG_MAX_ROWS") {
            Some(raw) => {
                let parsed = raw.parse::<u64>().map_err(|err| {
                    AppError::Config(format!("invalid LOG_MAX_ROWS '{raw}': {err}"))
                })?;
                if parsed < 100 {
                    return Err(AppError::Config(
                        "LOG_MAX_ROWS must be at least 100".to_string(),
                    ));
                }
                parsed
            }
            None => DEFAULT_LOG_MAX_ROWS,
        };

        let clickhouse_url = match non_empty("LOG_CLICKHOUSE_URL") {
            Some(raw) => {
                let parsed = url::Url::parse(&raw).map_err(|err| {
                    AppError::Config(format!("invalid LOG_CLICKHOUSE_URL '{raw}': {err}"))
                })?;
                if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                    return Err(AppError::Config(format!(
                        "invalid LOG_CLICKHOUSE_URL '{raw}': must be an http/https URL with a host"
                    )));
                }
                Some(raw.trim_end_matches('/').to_string())
            }
            None => None,
        };

        if log_store == LogStoreKind::ClickHouse && clickhouse_url.is_none() {
            return Err(AppError::Config(
                "LOG_CLICKHOUSE_URL is required when LOG_STORE=clickhouse".to_string(),
            ));
        }

        let clickhouse_database = non_empty("LOG_CLICKHOUSE_DATABASE")
            .unwrap_or_else(|| DEFAULT_LOG_CLICKHOUSE_DATABASE.to_string());
        validate_clickhouse_ident("LOG_CLICKHOUSE_DATABASE", &clickhouse_database)?;

        let clickhouse_table = non_empty("LOG_CLICKHOUSE_TABLE")
            .unwrap_or_else(|| DEFAULT_LOG_CLICKHOUSE_TABLE.to_string());
        validate_clickhouse_ident("LOG_CLICKHOUSE_TABLE", &clickhouse_table)?;

        let clickhouse_user = non_empty("LOG_CLICKHOUSE_USER");
        let clickhouse_password = lookup("LOG_CLICKHOUSE_PASSWORD").filter(|v| !v.is_empty());

        let logging = LogConfig {
            format: log_format,
            store: log_store,
            store_level: log_store_level,
            retention_days: log_retention_days,
            max_rows: log_max_rows,
            clickhouse_url,
            clickhouse_database,
            clickhouse_table,
            clickhouse_user,
            clickhouse_password,
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
            trust_proxy,
            logging,
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

    /// Path to the separate SQLite log database file (`<data_dir>/logs.db`).
    pub fn logs_db_path(&self) -> PathBuf {
        self.data_dir.join("logs.db")
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

/// Strictly validate ClickHouse database/table identifiers (`^[A-Za-z_][A-Za-z0-9_]{0,63}$`).
pub fn validate_clickhouse_ident(env_name: &str, raw: &str) -> AppResult<()> {
    let bytes = raw.as_bytes();
    let valid = !bytes.is_empty()
        && bytes.len() <= 64
        && (bytes[0].is_ascii_alphabetic() || bytes[0] == b'_')
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_');
    if !valid {
        return Err(AppError::Config(format!(
            "invalid {env_name} '{raw}': must match ^[A-Za-z_][A-Za-z0-9_]{{0,63}}$"
        )));
    }
    Ok(())
}
