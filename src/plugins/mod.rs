//! FeedCourier Plugin Framework (Plugin Protocol v1) — Built-in Rust Plugins, Out-of-Process
//! HTTP Bridge Plugins (`processor` + `channel` + `on_delivered`), and Marketplace Index Client.

pub mod bridge;
pub mod link_rewrite;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::channels::RouteOptions;
use crate::crypto::MasterKey;
use crate::db::Database;
use crate::db::models::{Channel, ChannelDestination, Entry, Feed, Route};
use crate::error::{AppError, AppResult};
use crate::net::{OutboundRequest, ProtectedHttpClient};

use self::bridge::{BridgeClient, BridgeProcessorPlugin};
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

/// Shorter negative-cache TTL (`60 seconds`) when falling back to the embedded snapshot without a
/// prior valid remote cache, so transient outages recover quickly.
pub const MARKET_SNAPSHOT_CACHE_TTL: Duration = Duration::from_secs(60);

/// Maximum response size accepted for a remote marketplace `index.json` (`1 MiB`).
const MAX_MARKET_INDEX_BYTES: usize = 1024 * 1024;

/// Per-plugin execution wall-clock timeout inside `run_on_render`.
const ON_RENDER_PLUGIN_TIMEOUT: Duration = Duration::from_secs(15);

/// Plugin category distinguishing delivery-time processors from delivery channel adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    #[default]
    Processor,
    Channel,
}

impl PluginKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Processor => "processor",
            Self::Channel => "channel",
        }
    }
}

/// Execution runtime for a plugin (`builtin` compiled into Rust binary vs out-of-process `bridge`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginRuntime {
    #[default]
    Builtin,
    Bridge,
}

impl PluginRuntime {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Bridge => "bridge",
        }
    }
}

/// Supported plugin hook points in Protocol v1.
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

fn default_empty_object() -> Value {
    json!({})
}

/// Static plugin metadata & declarative UI schema matching `schema/manifest.schema.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default = "default_api_version")]
    pub api_version: String,
    #[serde(default)]
    pub kind: PluginKind,
    #[serde(default)]
    pub runtime: PluginRuntime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_core: Option<String>,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub hooks: Vec<HookKind>,
    #[serde(default)]
    pub permissions: Vec<PluginPermission>,
    /// Declarative schema describing per-provider/instance configuration fields (processor plugins).
    #[serde(default = "default_empty_object")]
    pub instance_schema: Value,
    /// Declarative schema describing per-route plugin controls (e.g. provider dropdown).
    #[serde(default = "default_empty_object")]
    pub route_option_schema: Value,
    /// Declarative schema describing account-level fields (`kind = "channel"`).
    #[serde(default = "default_empty_object")]
    pub account_schema: Value,
    /// Declarative schema describing destination-level fields (`kind = "channel"`).
    #[serde(default = "default_empty_object")]
    pub destination_schema: Value,
    /// Optional channel capabilities (e.g. `supports_html`, `max_text_len`).
    #[serde(default = "default_empty_object")]
    pub capabilities: Value,
}

/// Check whether a plugin declaring `api_version` and optional `min_core` is compatible with
/// the running FeedCourier core version (`crate::config::VERSION`).
pub fn check_plugin_compatibility(
    api_version: &str,
    min_core: Option<&str>,
) -> (bool, Option<String>) {
    if api_version.trim() != PLUGIN_API_VERSION {
        return (
            false,
            Some(format!(
                "Incompatible plugin protocol v{} (core supports v{PLUGIN_API_VERSION})",
                api_version.trim()
            )),
        );
    }
    if let Some(req) = min_core.map(str::trim).filter(|s| !s.is_empty())
        && let (Some(core_v), Some(req_v)) = (
            parse_semver_triple(crate::config::VERSION),
            parse_semver_triple(req),
        )
        && core_v < req_v
    {
        return (false, Some(format!("Requires FeedCourier >= {req}")));
    }
    (true, None)
}

fn parse_semver_triple(raw: &str) -> Option<(u64, u64, u64)> {
    let clean = raw
        .trim()
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()?;
    let mut parts = clean.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    let patch = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    Some((major, minor, patch))
}

/// Non-destructive per-delivery overrides produced by `on_render` plugins.
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

/// Plugin trait implemented by both compiled-in Rust plugins and HTTP bridge processor plugins.
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

/// Cached runtime metadata for a registered HTTP bridge plugin (`processor` or `channel`).
#[derive(Clone)]
pub struct BridgePluginEntry {
    pub manifest: PluginManifest,
    pub client: BridgeClient,
    pub bridge_url: String,
    pub enabled: bool,
}

struct RegistryCache {
    loaded_from_db: bool,
    enabled_by_id: HashMap<String, bool>,
    bridges: HashMap<String, BridgePluginEntry>,
    bridge_processors: HashMap<String, Arc<dyn Plugin>>,
}

/// Registry holding built-in Rust plugins plus in-memory cached bridge plugins & enabled flags.
#[derive(Clone)]
pub struct PluginRegistry {
    builtins: Arc<Vec<Arc<dyn Plugin>>>,
    builtins_by_id: Arc<HashMap<String, Arc<dyn Plugin>>>,
    cache: Arc<RwLock<RegistryCache>>,
}

impl fmt::Debug for PluginRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let builtin_ids: Vec<&str> = self
            .builtins
            .iter()
            .map(|p| p.manifest().id.as_str())
            .collect();
        let bridge_ids: Vec<String> = self
            .cache
            .read()
            .map(|g| g.bridges.keys().cloned().collect())
            .unwrap_or_default();
        f.debug_struct("PluginRegistry")
            .field("builtins", &builtin_ids)
            .field("bridges", &bridge_ids)
            .finish()
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

impl PluginRegistry {
    /// Construct a registry containing all built-in plugins (bridge plugins are loaded via `reload_from_db`).
    pub fn with_builtins() -> Self {
        let builtins: Vec<Arc<dyn Plugin>> = vec![Arc::new(LinkRewritePlugin::new())];
        let mut builtins_by_id = HashMap::with_capacity(builtins.len());
        for p in &builtins {
            builtins_by_id.insert(p.manifest().id.clone(), Arc::clone(p));
        }
        Self {
            builtins: Arc::new(builtins),
            builtins_by_id: Arc::new(builtins_by_id),
            cache: Arc::new(RwLock::new(RegistryCache {
                loaded_from_db: false,
                enabled_by_id: HashMap::new(),
                bridges: HashMap::new(),
                bridge_processors: HashMap::new(),
            })),
        }
    }

    /// Return built-in Rust plugins only.
    pub fn list(&self) -> &[Arc<dyn Plugin>] {
        &self.builtins
    }

    pub fn is_builtin(&self, id: &str) -> bool {
        self.builtins_by_id.contains_key(id.trim())
    }

    /// Look up any processor plugin (built-in or bridge) implementing [`Plugin`].
    pub fn get(&self, id: &str) -> Option<Arc<dyn Plugin>> {
        let trimmed = id.trim();
        if let Some(p) = self.builtins_by_id.get(trimmed) {
            return Some(Arc::clone(p));
        }
        self.cache
            .read()
            .ok()
            .and_then(|g| g.bridge_processors.get(trimmed).cloned())
    }

    /// Look up a registered HTTP bridge plugin entry (`processor` or `channel`).
    pub fn get_bridge(&self, id: &str) -> Option<BridgePluginEntry> {
        self.cache
            .read()
            .ok()
            .and_then(|g| g.bridges.get(id.trim()).cloned())
    }

    /// Return all registered bridge plugins sorted by ID.
    pub fn list_bridges(&self) -> Vec<BridgePluginEntry> {
        let mut list: Vec<BridgePluginEntry> = self
            .cache
            .read()
            .map(|g| g.bridges.values().cloned().collect())
            .unwrap_or_default();
        list.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
        list
    }

    /// Return the manifest for any installed plugin (built-in or bridge).
    pub fn get_manifest(&self, id: &str) -> Option<PluginManifest> {
        let trimmed = id.trim();
        if let Some(p) = self.builtins_by_id.get(trimmed) {
            return Some(p.manifest().clone());
        }
        self.cache
            .read()
            .ok()
            .and_then(|g| g.bridges.get(trimmed).map(|b| b.manifest.clone()))
    }

    /// Ensure every built-in plugin has a row in SQLite `plugins` (idempotent; preserves `enabled`).
    pub async fn sync_builtin_rows(&self, db: &Database) -> AppResult<()> {
        for plugin in self.builtins.iter() {
            let m = plugin.manifest();
            db.ensure_plugin_row(&m.id, &m.version).await?;
        }
        Ok(())
    }

    /// Reload all plugin enabled states and registered HTTP bridge plugins from SQLite into memory.
    /// Called at startup and immediately after any plugin create/update/refresh/delete mutation.
    pub async fn reload_from_db(&self, db: &Database, master_key: &MasterKey) -> AppResult<()> {
        let rows = db.list_plugins().await?;
        let mut enabled_by_id = HashMap::with_capacity(rows.len());
        let mut bridges = HashMap::new();
        let mut bridge_processors: HashMap<String, Arc<dyn Plugin>> = HashMap::new();

        for row in rows {
            enabled_by_id.insert(row.id.clone(), row.enabled);
            if row.source == "bridge" {
                let Some(bridge_url) = row.bridge_url.clone() else {
                    continue;
                };
                let Some(enc_secret) = row.encrypted_bridge_secret.as_deref() else {
                    continue;
                };
                let Some(manifest_json) = row.manifest_json.as_deref() else {
                    continue;
                };
                let secret: String = match master_key.decrypt_json(enc_secret) {
                    Ok(s) => s,
                    Err(err) => {
                        tracing::warn!(
                            plugin_id = %row.id,
                            error = %err,
                            "failed to decrypt bridge secret; skipping bridge plugin"
                        );
                        continue;
                    }
                };
                let mut manifest: PluginManifest = match serde_json::from_str(manifest_json) {
                    Ok(m) => m,
                    Err(err) => {
                        tracing::warn!(
                            plugin_id = %row.id,
                            error = %err,
                            "failed to parse persisted bridge manifest_json; skipping bridge plugin"
                        );
                        continue;
                    }
                };
                manifest.runtime = PluginRuntime::Bridge;
                let client = match BridgeClient::new(bridge_url.clone(), secret, row.id.clone()) {
                    Ok(c) => c,
                    Err(err) => {
                        tracing::warn!(
                            plugin_id = %row.id,
                            error = %err,
                            "invalid persisted bridge_url; skipping bridge plugin"
                        );
                        continue;
                    }
                };
                if manifest.kind == PluginKind::Processor {
                    let proc_plugin = BridgeProcessorPlugin::new(manifest.clone(), client.clone());
                    bridge_processors.insert(row.id.clone(), Arc::new(proc_plugin));
                }
                bridges.insert(
                    row.id.clone(),
                    BridgePluginEntry {
                        manifest,
                        client,
                        bridge_url,
                        enabled: row.enabled,
                    },
                );
            }
        }

        if let Ok(mut guard) = self.cache.write() {
            guard.loaded_from_db = true;
            guard.enabled_by_id = enabled_by_id;
            guard.bridges = bridges;
            guard.bridge_processors = bridge_processors;
        }
        Ok(())
    }

    /// Check whether a plugin is enabled using the in-memory cache (falling back to DB only if
    /// `reload_from_db` has not been invoked yet, e.g. in isolated unit tests).
    async fn is_plugin_enabled(&self, db: &Database, id: &str) -> AppResult<bool> {
        if let Ok(guard) = self.cache.read()
            && guard.loaded_from_db
        {
            return Ok(guard.enabled_by_id.get(id).copied().unwrap_or(false));
        }
        Ok(db.get_plugin(id).await?.is_some_and(|r| r.enabled))
    }

    /// Validate all entries in `route_options.plugins` and `route_options.plugin_order`.
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
                AppError::Validation(format!(
                    "unknown processor plugin '{plugin_id}' in route options"
                ))
            })?;
            plugin.validate_route_option(db, opt_val).await?;
        }
        Ok(())
    }

    /// Compute the ordered list of enabled bridge plugins subscribed to `HookKind::OnDelivered`
    /// for a completed delivery task (route-configured processor plugins + optional channel plugin).
    pub fn on_delivered_subscriber_ids(
        &self,
        route_options: &RouteOptions,
        channel_plugin_id: Option<&str>,
    ) -> Vec<String> {
        let Ok(guard) = self.cache.read() else {
            return Vec::new();
        };
        let mut seen = HashSet::new();
        let mut out = Vec::new();

        for (pid, opt) in &route_options.plugins {
            if opt.is_null() {
                continue;
            }
            if let Some(bridge) = guard.bridges.get(pid)
                && bridge.enabled
                && bridge.manifest.hooks.contains(&HookKind::OnDelivered)
                && seen.insert(pid.clone())
            {
                out.push(pid.clone());
            }
        }
        if let Some(ch_pid) = channel_plugin_id
            && let Some(bridge) = guard.bridges.get(ch_pid)
            && bridge.enabled
            && bridge.manifest.hooks.contains(&HookKind::OnDelivered)
            && seen.insert(ch_pid.to_string())
        {
            out.push(ch_pid.to_string());
        }
        out
    }

    /// Execute the `on_render` hook chain for every enabled plugin referenced by `route_options`,
    /// honoring `route_options.plugin_order`, enforcing per-plugin timeouts, and isolating
    /// non-strict plugin failures into delivery warnings.
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

        // Build deterministic execution sequence: explicit `plugin_order` first, then remaining keys.
        let mut ordered_ids: Vec<&str> = Vec::with_capacity(route_options.plugins.len());
        let mut seen: HashSet<&str> = HashSet::new();
        for pid in &route_options.plugin_order {
            if let Some(opt) = route_options.plugins.get(pid)
                && !opt.is_null()
                && seen.insert(pid.as_str())
            {
                ordered_ids.push(pid.as_str());
            }
        }
        for (pid, opt) in &route_options.plugins {
            if !opt.is_null() && seen.insert(pid.as_str()) {
                ordered_ids.push(pid.as_str());
            }
        }

        for plugin_id in ordered_ids {
            let Some(route_cfg) = route_options.plugins.get(plugin_id) else {
                continue;
            };
            let Some(plugin) = self.get(plugin_id) else {
                continue;
            };
            if !plugin.manifest().hooks.contains(&HookKind::OnRender) {
                continue;
            }
            if !self.is_plugin_enabled(db, plugin_id).await? {
                continue;
            }

            let strict_mode = route_cfg
                .get("strict_mode")
                .and_then(Value::as_bool)
                .unwrap_or(false);

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

            let started = Instant::now();
            let run_res = tokio::time::timeout(
                ON_RENDER_PLUGIN_TIMEOUT,
                plugin.on_render(&ctx, &mut overrides),
            )
            .await;
            let duration_ms = started.elapsed().as_millis() as u64;

            match run_res {
                Ok(Ok(PluginResult::Continue)) => {
                    tracing::debug!(
                        plugin_id,
                        route_id = route.id,
                        duration_ms,
                        "on_render plugin completed"
                    );
                }
                Ok(Ok(PluginResult::Retry {
                    diagnostic,
                    retry_after_secs,
                })) => {
                    return Ok((
                        overrides,
                        Some(PluginRetry {
                            plugin_id: plugin_id.to_string(),
                            diagnostic,
                            retry_after_secs,
                        }),
                    ));
                }
                Ok(Err(err)) => {
                    let msg = format!(
                        "Plugin '{plugin_id}' failed ({}): kept original",
                        crate::logging::redact::redact_text(&err.to_string())
                    );
                    tracing::warn!(
                        plugin_id,
                        route_id = route.id,
                        duration_ms,
                        strict_mode,
                        error = %err,
                        "on_render plugin returned error"
                    );
                    if strict_mode {
                        return Ok((
                            overrides,
                            Some(PluginRetry {
                                plugin_id: plugin_id.to_string(),
                                diagnostic: msg,
                                retry_after_secs: None,
                            }),
                        ));
                    }
                    overrides.warnings.push(msg);
                }
                Err(_) => {
                    let msg = format!(
                        "Plugin '{plugin_id}' timed out after {}s; kept original",
                        ON_RENDER_PLUGIN_TIMEOUT.as_secs()
                    );
                    tracing::warn!(
                        plugin_id,
                        route_id = route.id,
                        duration_ms,
                        strict_mode,
                        "on_render plugin timed out"
                    );
                    if strict_mode {
                        return Ok((
                            overrides,
                            Some(PluginRetry {
                                plugin_id: plugin_id.to_string(),
                                diagnostic: msg,
                                retry_after_secs: None,
                            }),
                        ));
                    }
                    overrides.warnings.push(msg);
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
    /// `"builtin"`, `"processor"`, `"channel"`, or `"external"` / `"coming_soon"`.
    #[serde(default = "default_market_kind")]
    pub kind: String,
    /// `"builtin"` or `"bridge"`.
    #[serde(default)]
    pub runtime: Option<String>,
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
    pub image: Option<String>,
    #[serde(default)]
    pub compose_snippet: Option<String>,
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

/// Validate that an optional external marketplace URL uses strictly `http://` or `https://`
/// with a non-empty hostname; strips `javascript:`, `data:`, `file:`, and malformed values.
pub fn sanitize_http_url(raw: Option<String>) -> Option<String> {
    let trimmed = raw?.trim().to_string();
    if trimmed.is_empty() {
        return None;
    }
    let parsed = url::Url::parse(&trimmed).ok()?;
    if matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some() {
        Some(trimmed)
    } else {
        None
    }
}

impl MarketIndexDocument {
    /// Sanitize untrusted external links (`homepage`, `repository`) on every plugin entry.
    pub fn sanitized(mut self) -> Self {
        for entry in &mut self.plugins {
            entry.homepage = sanitize_http_url(entry.homepage.take());
            entry.repository = sanitize_http_url(entry.repository.take());
        }
        self
    }
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

    pub async fn get_index(
        &self,
        http: &ProtectedHttpClient,
        force_refresh: bool,
    ) -> (MarketIndexDocument, &'static str) {
        if !force_refresh
            && let Ok(guard) = self.cache.lock()
            && let Some(ref cached) = *guard
        {
            let ttl = if cached.source == "remote" {
                MARKET_INDEX_CACHE_TTL
            } else {
                MARKET_SNAPSHOT_CACHE_TTL
            };
            if cached.fetched_at.elapsed() < ttl {
                return (cached.doc.clone(), cached.source);
            }
        }

        let use_proxy = http.settings().outbound_proxy_url.is_some();
        let req = OutboundRequest::new(&self.index_url)
            .use_proxy(use_proxy)
            .timeout(Duration::from_secs(8))
            .max_bytes(MAX_MARKET_INDEX_BYTES);

        match http.get_bytes(req).await {
            Ok(resp) if resp.status.is_success() => {
                match serde_json::from_slice::<MarketIndexDocument>(&resp.bytes) {
                    Ok(raw_doc) => {
                        let doc = raw_doc.sanitized();
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

        let snapshot: MarketIndexDocument =
            serde_json::from_str::<MarketIndexDocument>(MARKET_SNAPSHOT_JSON)
                .expect("embedded market_snapshot.json must be valid JSON")
                .sanitized();
        if let Ok(mut guard) = self.cache.lock() {
            let preserve_valid_remote = guard.as_ref().is_some_and(|existing| {
                existing.source == "remote"
                    && existing.fetched_at.elapsed() < MARKET_INDEX_CACHE_TTL
            });
            if !preserve_valid_remote {
                *guard = Some(CachedMarketIndex {
                    doc: snapshot.clone(),
                    source: "snapshot",
                    fetched_at: Instant::now(),
                });
            }
        }
        (snapshot, "snapshot")
    }
}
