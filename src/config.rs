//! Runtime configuration and build metadata constants.

use std::net::SocketAddr;
use std::path::PathBuf;

use crate::error::{AppError, AppResult};

/// Canonical application name.
pub const APP_NAME: &str = "FeedCourier";

/// Build version sourced from `Cargo.toml` (`CARGO_PKG_VERSION`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Upstream source repository URL (required by AGPL-3.0-only Section 13 in WebUI & MCP ServerInfo).
pub const REPO_URL: &str = "https://github.com/ElleryHe/FeedCourier";

/// SPDX license identifier for FeedCourier.
pub const LICENSE_SPDX: &str = "AGPL-3.0-only";

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

        Ok(Self {
            data_dir,
            listen_addr,
            master_key,
            admin_password,
            ssrf_allowlist,
            outbound_proxy,
            retention_hours,
            read_pool_max_connections,
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
