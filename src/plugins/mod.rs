//! Built-in FeedCourier Plugin Framework (Plugin Protocol v1) and Marketplace Index client.
//!
//! Phase 1 (v0.3.0): First-party plugins implement [`Plugin`] in Rust and compile directly into
//! the single binary, while sharing the exact manifest schema (`api_version = "1"`), SQLite tables
//! (`plugins`, `plugin_instances`, `plugin_link_cache`), and hook lifecycle designed for a future
//! WASM/Extism third-party sandbox runtime.

pub mod link_rewrite;

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::channels::RouteOptions;
use crate::crypto::MasterKey;
use crate::db::Database;
use crate::db::models::{Channel, ChannelDestination, Entry, Feed, Route};
use crate::error::{AppError, AppResult};
use crate::net::{OutboundRequest, ProtectedHttpClient};

use self::link_rewrite::LinkRewritePlugin;

/// Current stable plugin protocol version (`api_version`).
pub const PLUGIN_API_VERSION: &str = "1";

/// Default remote plugin marketplace index URL hosted in `ElleryHe/FeedCourier-Plugins`.
pub const DEFAULT_PLUGIN_INDEX_URL: &str =
    "https://raw.githubusercontent.com/ElleryHe/FeedCourier-Plugins/main/index.json";

/// Compiled-in marketplace index snapshot used as an air-gapped fallback when the remote index
/// is unreachable.
pub const MARKET_SNAPSHOT_JSON: &str = include_str!("market_snapshot.json");

/// Cache TTL for remote marketplace index fetches (`6 hours`).
pub const MARKET_INDEX_CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// Maximum response size accepted for a remote marketplace `index.json` (`1 MiB`).
const MAX_MARKET_INDEX_BYTES: usize = 1024 * 1024;

/// Supported plugin hook points in Protocol v1.
///
/// `OnRender` executes in v0.3.0; `OnIngest` and `OnDelivered` are reserved in the manifest and
/// protocol schema for upcoming releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookKind {
    OnRender,
    OnIngest,
    OnDelivered,
}

/// Capabilities/permissions declared by a plugin manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginPermission {
    Network,
}

/// Static plugin metadata & declarative UI schema matching `schema/manifest.schema.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: String,
    pub category: String,
    pub icon: String,
    pub description: String,
    pub homepage: String,
    pub hooks: Vec<HookKind>,
    pub permissions: Vec<PluginPermission>,
    /// Declarative schema describing per-provider/instance configuration fields.
    pub instance_schema: Value,
    /// Declarative schema describing per-route plugin controls (e.g. provider dropdown).
    pub route_option_schema: Value,
}

/// Non-destructive per-delivery overrides produced by `on_render` plugins.
///
/// The underlying persisted `Entry` is never mutated so multi-route deliveries and archival
/// properties (`entry.canonical_url` for Telegram previews and Notion `Link` column) remain intact.
#[derive(Debug, Clone, Default)]
pub struct RenderOverrides {
    pub content_html: Option<String>,
    pub primary_link: Option<String>,
    pub warnings: Vec<String>,
}

impl RenderOverrides {
    /// Effective HTML body (`overrides.content_html`, falling back to `entry.content_html`).
    pub fn effective_content_html<'a>(&'a self, entry: &'a Entry) -> Option<&'a str> {
        self.content_html
            .as_deref()
            .or(entry.content_html.as_deref())
    }

    /// Effective primary link (`overrides.primary_link`, falling back to `entry.canonical_url`).
    pub fn effective_primary_link<'a>(&'a self, entry: &'a Entry) -> Option<&'a str> {
        self.primary_link
            .as_deref()
            .or(entry.canonical_url.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }
}

/// Immutable runtime context handed to a plugin's `on_render` hook.
pub struct RenderContext<'a> {
    pub db: &'a Database,
    pub master_key: &'a MasterKey,
    pub http: &'a ProtectedHttpClient,
    pub entry: &'a Entry,
    pub feed: &'a Feed,
    pub route: &'a Route,
    pub route_options: &'a RouteOptions,
    pub channel: &'a Channel,
    pub destination: &'a ChannelDestination,
    /// Route-specific configuration blob for this plugin (`route_options.plugins[plugin_id]`).
    pub plugin_route_config: Option<&'a Value>,
    /// `true` for normal outbox tasks (allows persisting short-link cache); `false` for Debug Push.
    pub write_cache: bool,
}

/// Control-flow decision returned by an `on_render` hook.
#[derive(Debug, Clone)]
pub enum PluginResult {
    Continue,
    Retry {
        diagnostic: String,
        retry_after_secs: Option<u64>,
    },
}

/// First-party Rust plugin trait (Protocol v1).
#[async_trait]
pub trait Plugin: Send + Sync {
    fn manifest(&self) -> &PluginManifest;

    /// Validate and normalize a new provider/instance configuration JSON before encryption.
    fn validate_instance_config(&self, cfg: &Value) -> AppResult<Value>;

    /// Secret-preserving merge of a partial PATCH payload onto an existing decrypted instance config.
    fn merge_instance_config(&self, existing: &Value, patch: &Value) -> AppResult<Value>;

    /// Redact secret fields (`signature`, `password`, etc.) before returning instance config to UI.
    fn redact_instance_config(&self, cfg: &Value) -> Value;

    /// Validate route-level plugin options (e.g. verifying referenced `provider_id` belongs to plugin).
    async fn validate_route_option(&self, db: &Database, option: &Value) -> AppResult<()>;

    /// Execute an interactive connectivity test for a provider/instance config.
    async fn test_instance(&self, http: &ProtectedHttpClient, cfg: &Value) -> AppResult<String>;

    /// Transform article links/content immediately before a channel adapter renders the delivery.
    async fn on_render(
        &self,
        ctx: &RenderContext<'_>,
        out: &mut RenderOverrides,
    ) -> AppResult<PluginResult>;
}

/// Retry signal bubbled up from [`PluginRegistry::run_on_render`] when a strict-mode plugin fails.
#[derive(Debug, Clone)]
pub struct PluginRetry {
    pub plugin_id: String,
    pub diagnostic: String,
    pub retry_after_secs: Option<u64>,
}

/// Registry holding all compiled-in FeedCourier plugins.
#[derive(Clone)]
pub struct PluginRegistry {
    plugins: Vec<Arc<dyn Plugin>>,
    by_id: HashMap<String, Arc<dyn Plugin>>,
}

impl fmt::Debug for PluginRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ids: Vec<&str> = self
            .plugins
            .iter()
            .map(|p| p.manifest().id.as_str())
            .collect();
        f.debug_struct("PluginRegistry")
            .field("plugins", &ids)
            .finish()
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

impl PluginRegistry {
    /// Construct a registry containing all built-in v0.3.0 plugins.
    pub fn with_builtins() -> Self {
        let builtins: Vec<Arc<dyn Plugin>> = vec![Arc::new(LinkRewritePlugin::new())];
        let mut by_id = HashMap::with_capacity(builtins.len());
        for p in &builtins {
            by_id.insert(p.manifest().id.clone(), Arc::clone(p));
        }
        Self {
            plugins: builtins,
            by_id,
        }
    }

    pub fn list(&self) -> &[Arc<dyn Plugin>] {
        &self.plugins
    }

    pub fn get(&self, id: &str) -> Option<&Arc<dyn Plugin>> {
        self.by_id.get(id.trim())
    }

    /// Ensure every built-in plugin has a row in SQLite `plugins` (idempotent; preserves `enabled`).
    pub async fn sync_builtin_rows(&self, db: &Database) -> AppResult<()> {
        for plugin in &self.plugins {
            let m = plugin.manifest();
            db.ensure_plugin_row(&m.id, &m.version).await?;
        }
        Ok(())
    }

    /// Validate all entries in `route_options.plugins` before creating or updating a route.
    pub async fn validate_route_plugins(
        &self,
        db: &Database,
        route_plugins: &std::collections::BTreeMap<String, Value>,
    ) -> AppResult<()> {
        for (plugin_id, opt_val) in route_plugins {
            if opt_val.is_null() {
                continue;
            }
            let plugin = self.get(plugin_id).ok_or_else(|| {
                AppError::Validation(format!("unknown plugin '{plugin_id}' in route options"))
            })?;
            plugin.validate_route_option(db, opt_val).await?;
        }
        Ok(())
    }

    /// Execute the `on_render` hook chain for every enabled plugin referenced by `route_options`.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_on_render(
        &self,
        db: &Database,
        master_key: &MasterKey,
        http: &ProtectedHttpClient,
        entry: &Entry,
        feed: &Feed,
        route: &Route,
        route_options: &RouteOptions,
        channel: &Channel,
        destination: &ChannelDestination,
        write_cache: bool,
    ) -> AppResult<(RenderOverrides, Option<PluginRetry>)> {
        let mut overrides = RenderOverrides::default();
        if route_options.plugins.is_empty() {
            return Ok((overrides, None));
        }

        for plugin in &self.plugins {
            let manifest = plugin.manifest();
            let Some(route_cfg) = route_options.plugins.get(&manifest.id) else {
                continue;
            };
            if route_cfg.is_null() {
                continue;
            }
            let Some(row) = db.get_plugin(&manifest.id).await? else {
                continue;
            };
            if !row.enabled {
                continue;
            }

            let ctx = RenderContext {
                db,
                master_key,
                http,
                entry,
                feed,
                route,
                route_options,
                channel,
                destination,
                plugin_route_config: Some(route_cfg),
                write_cache,
            };

            match plugin.on_render(&ctx, &mut overrides).await? {
                PluginResult::Continue => {}
                PluginResult::Retry {
                    diagnostic,
                    retry_after_secs,
                } => {
                    return Ok((
                        overrides,
                        Some(PluginRetry {
                            plugin_id: manifest.id.clone(),
                            diagnostic,
                            retry_after_secs,
                        }),
                    ));
                }
            }
        }

        Ok((overrides, None))
    }
}

// =============================================================================
// Plugin Marketplace Index Client (6h memory cache + compiled snapshot fallback)
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketIndexDocument {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub plugins: Vec<MarketPluginEntry>,
}

const fn default_schema_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketPluginEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default = "default_api_version")]
    pub api_version: String,
    /// `"builtin"` or `"external"` / `"coming_soon"`.
    #[serde(default = "default_market_kind")]
    pub kind: String,
    #[serde(default)]
    pub min_core: Option<String>,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub hooks: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
}

fn default_api_version() -> String {
    PLUGIN_API_VERSION.to_string()
}

fn default_market_kind() -> String {
    "external".to_string()
}

#[derive(Debug, Clone)]
struct CachedMarketIndex {
    doc: MarketIndexDocument,
    source: &'static str,
    fetched_at: Instant,
}

/// In-memory cached marketplace index loader backed by [`ProtectedHttpClient`].
#[derive(Debug, Clone)]
pub struct MarketIndexCache {
    index_url: String,
    cache: Arc<Mutex<Option<CachedMarketIndex>>>,
}

impl MarketIndexCache {
    pub fn new(custom_url: Option<String>) -> Self {
        let index_url = custom_url
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_PLUGIN_INDEX_URL.to_string());
        Self {
            index_url,
            cache: Arc::new(Mutex::new(None)),
        }
    }

    pub fn index_url(&self) -> &str {
        &self.index_url
    }

    /// Return the marketplace index, using the 6h cache, fetching remotely via the SSRF-guarded
    /// client when stale, and gracefully falling back to the compiled snapshot on failure.
    pub async fn get_index(
        &self,
        http: &ProtectedHttpClient,
        force_refresh: bool,
    ) -> (MarketIndexDocument, &'static str) {
        if !force_refresh
            && let Ok(guard) = self.cache.lock()
            && let Some(ref cached) = *guard
            && cached.fetched_at.elapsed() < MARKET_INDEX_CACHE_TTL
        {
            return (cached.doc.clone(), cached.source);
        }

        let use_proxy = http.settings().outbound_proxy_url.is_some();
        let req = OutboundRequest::new(&self.index_url)
            .use_proxy(use_proxy)
            .timeout(Duration::from_secs(8))
            .max_bytes(MAX_MARKET_INDEX_BYTES);

        match http.get_bytes(req).await {
            Ok(resp) if resp.status.is_success() => {
                match serde_json::from_slice::<MarketIndexDocument>(&resp.bytes) {
                    Ok(doc) => {
                        if let Ok(mut guard) = self.cache.lock() {
                            *guard = Some(CachedMarketIndex {
                                doc: doc.clone(),
                                source: "remote",
                                fetched_at: Instant::now(),
                            });
                        }
                        return (doc, "remote");
                    }
                    Err(err) => {
                        tracing::warn!(
                            url = %self.index_url,
                            error = %err,
                            "failed to parse remote plugin marketplace index; falling back to snapshot"
                        );
                    }
                }
            }
            Ok(resp) => {
                tracing::warn!(
                    url = %self.index_url,
                    status = %resp.status,
                    "remote plugin marketplace index returned non-2xx; falling back to snapshot"
                );
            }
            Err(err) => {
                tracing::warn!(
                    url = %self.index_url,
                    error = %err,
                    "failed to fetch remote plugin marketplace index; falling back to snapshot"
                );
            }
        }

        let snapshot: MarketIndexDocument = serde_json::from_str(MARKET_SNAPSHOT_JSON)
            .expect("embedded market_snapshot.json must be valid JSON");
        if let Ok(mut guard) = self.cache.lock() {
            *guard = Some(CachedMarketIndex {
                doc: snapshot.clone(),
                source: "snapshot",
                fetched_at: Instant::now(),
            });
        }
        (snapshot, "snapshot")
    }
}
