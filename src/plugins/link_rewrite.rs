//! First-party **Link Rewrite (YOURLS)** plugin (`id = "link-rewrite"`).
//!
//! Runs during the `on_render` hook before channel rendering:
//! - Rewrites `http://` and `https://` `<a href>` links in `entry.content_html` and the article's
//!   primary link (`entry.canonical_url`), NEVER touching media elements (`<img>`, `<video>`,
//!   `<audio>`, `<source>`) or non-HTTP links (`mailto:`, `tel:`, `#anchors`).
//! - Persists shortened links in `plugin_link_cache(instance_id, long_url_sha256)` so retries,
//!   multi-route fanout, and multi-destination deliveries avoid duplicate YOURLS API calls.
//! - Protects delivery throughput with per-URL 5s timeout, 4-way concurrency, and a 50-URL cap.
//! - Defaults to per-URL graceful degradation (keeping original URL on failure + recording a
//!   warning in `diagnostic_log`), with optional per-provider `strict_mode` retrying the task on
//!   any link shortening failure.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use crate::channels::mask_secret;
use crate::crypto::{md5_hex, sha256_hex};
use crate::db::Database;
use crate::db::models::now_unix;
use crate::error::{AppError, AppResult};
use crate::ingest::html::{
    ParsedTagAttr, find_ascii_case_insensitive, find_html_tag_end,
    parse_tag_attrs_and_self_closing, split_tag_name_and_attrs,
};
use crate::net::ProtectedHttpClient;

use super::{
    HookKind, PLUGIN_API_VERSION, Plugin, PluginManifest, PluginPermission, PluginResult,
    RenderContext, RenderOverrides,
};

/// Canonical plugin identifier for Link Rewrite (YOURLS).
pub const LINK_REWRITE_PLUGIN_ID: &str = "link-rewrite";

/// Maximum number of unique URLs shortened per article delivery.
pub const MAX_REWRITE_URLS_PER_ENTRY: usize = 50;

/// Maximum concurrent YOURLS API requests per article.
pub const REWRITE_CONCURRENCY: usize = 4;

/// Per-URL YOURLS API HTTP timeout (`5s`).
pub const REWRITE_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum response payload bytes accepted from YOURLS API (`64 KiB`).
const MAX_YOURLS_RESPONSE_BYTES: usize = 64 * 1024;

/// Authentication scheme for a YOURLS API provider instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum YourlsAuthMode {
    /// Passwordless signature token (`signature=<token>`).
    #[default]
    Signature,
    /// Timestamped MD5 signature (`timestamp=<unix>&signature=md5(timestamp + token)`).
    TimedSignature,
    /// Username + password (`username=<u>&password=<p>`).
    Password,
}

/// Decrypted configuration for one YOURLS link-rewrite provider stored in
/// `plugin_instances.encrypted_config`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YourlsProviderConfig {
    pub api_url: String,
    #[serde(default)]
    pub auth_mode: YourlsAuthMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default)]
    pub exclude_domains: Vec<String>,
    #[serde(default)]
    pub strict_mode: bool,
    #[serde(default = "default_true")]
    pub pass_title: bool,
    #[serde(default)]
    pub use_proxy: bool,
}

const fn default_true() -> bool {
    true
}

impl YourlsProviderConfig {
    pub fn validate_and_normalize(mut self) -> AppResult<Self> {
        self.api_url = self.api_url.trim().to_string();
        let parsed = Url::parse(&self.api_url).map_err(|err| {
            AppError::Validation(format!("invalid YOURLS api_url '{}': {err}", self.api_url))
        })?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(AppError::Validation(
                "YOURLS api_url must use http or https with a valid hostname".to_string(),
            ));
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(AppError::Validation(
                "YOURLS api_url must not contain embedded credentials".to_string(),
            ));
        }

        self.signature = self
            .signature
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.username = self
            .username
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.password = self.password.filter(|s| !s.is_empty());

        match self.auth_mode {
            YourlsAuthMode::Signature | YourlsAuthMode::TimedSignature => {
                if self.signature.is_none() {
                    return Err(AppError::Validation(
                        "YOURLS signature token is required for signature / timed_signature auth"
                            .to_string(),
                    ));
                }
                // Clear inactive password credentials so old secrets are never retained on disk.
                self.username = None;
                self.password = None;
            }
            YourlsAuthMode::Password => {
                if self.username.is_none() || self.password.is_none() {
                    return Err(AppError::Validation(
                        "YOURLS username and password are required for password auth".to_string(),
                    ));
                }
                // Clear inactive signature token when switching to username/password auth.
                self.signature = None;
            }
        }

        let mut normalized_domains = Vec::with_capacity(self.exclude_domains.len());
        for d in self.exclude_domains {
            let clean = d.trim().trim_end_matches('.').to_ascii_lowercase();
            if !clean.is_empty() && !normalized_domains.contains(&clean) {
                normalized_domains.push(clean);
            }
        }
        self.exclude_domains = normalized_domains;

        Ok(self)
    }

    pub fn redacted_view(&self) -> Value {
        json!({
            "api_url": self.api_url,
            "auth_mode": self.auth_mode,
            "has_signature": self.signature.as_ref().is_some_and(|s| !s.is_empty()),
            "signature_hint": self.signature.as_deref().map(mask_secret),
            "username": self.username,
            "has_password": self.password.as_ref().is_some_and(|s| !s.is_empty()),
            "exclude_domains": self.exclude_domains,
            "strict_mode": self.strict_mode,
            "pass_title": self.pass_title,
            "use_proxy": self.use_proxy,
        })
    }

    pub fn api_host(&self) -> Option<String> {
        Url::parse(&self.api_url)
            .ok()
            .and_then(|u| {
                u.host_str()
                    .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
            })
            .filter(|h| !h.is_empty())
    }
}

/// Route-level configuration payload stored at `route.options.plugins["link-rewrite"]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LinkRewriteRouteOption {
    #[serde(default)]
    pub provider_id: Option<i64>,
}

/// Built-in YOURLS Link Rewrite plugin implementation.
#[derive(Debug, Clone)]
pub struct LinkRewritePlugin {
    manifest: PluginManifest,
}

impl Default for LinkRewritePlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LinkRewritePlugin {
    pub fn new() -> Self {
        let manifest = PluginManifest {
            id: LINK_REWRITE_PLUGIN_ID.to_string(),
            name: "Link Rewrite (YOURLS)".to_string(),
            version: crate::config::VERSION.to_string(),
            api_version: PLUGIN_API_VERSION.to_string(),
            category: "links".to_string(),
            icon: "link".to_string(),
            description: "Rewrite outgoing article links using self-hosted YOURLS short-link providers at delivery time with SQLite caching and per-route selection.".to_string(),
            homepage: "https://feedcourier.ellery.me/plugins/link-rewrite".to_string(),
            hooks: vec![HookKind::OnRender],
            permissions: vec![PluginPermission::Network],
            instance_schema: json!({
                "type": "object",
                "required": ["api_url", "auth_mode"],
                "properties": {
                    "api_url": { "type": "string", "title": "YOURLS API URL", "placeholder": "https://sho.rt/yourls-api.php" },
                    "auth_mode": {
                        "type": "string",
                        "title": "Authentication Mode",
                        "enum": ["signature", "timed_signature", "password"],
                        "default": "signature"
                    },
                    "signature": { "type": "string", "title": "Signature Token", "format": "password" },
                    "username": { "type": "string", "title": "Username" },
                    "password": { "type": "string", "title": "Password", "format": "password" },
                    "exclude_domains": { "type": "array", "items": { "type": "string" }, "title": "Excluded Domains" },
                    "strict_mode": { "type": "boolean", "title": "Strict Mode", "default": false },
                    "pass_title": { "type": "boolean", "title": "Pass Article Title", "default": true },
                    "use_proxy": { "type": "boolean", "title": "Use Outbound Proxy", "default": false }
                }
            }),
            route_option_schema: json!({
                "type": "object",
                "properties": {
                    "provider_id": {
                        "type": "select",
                        "source": "plugin_instances",
                        "nullable": true,
                        "title": "Link Rewrite Provider"
                    }
                }
            }),
        };
        Self { manifest }
    }
}

#[async_trait]
impl Plugin for LinkRewritePlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn validate_instance_config(&self, cfg: &Value) -> AppResult<Value> {
        let parsed: YourlsProviderConfig = serde_json::from_value(cfg.clone()).map_err(|err| {
            AppError::Validation(format!("invalid YOURLS provider config: {err}"))
        })?;
        let normalized = parsed.validate_and_normalize()?;
        Ok(serde_json::to_value(normalized)?)
    }

    fn merge_instance_config(&self, existing: &Value, patch: &Value) -> AppResult<Value> {
        let mut cur: YourlsProviderConfig = serde_json::from_value(existing.clone())
            .map_err(|err| AppError::Crypto(format!("corrupt YOURLS provider config: {err}")))?;
        let Some(obj) = patch.as_object() else {
            return Err(AppError::Validation(
                "YOURLS provider config patch must be a JSON object".to_string(),
            ));
        };

        if let Some(v) = obj.get("api_url").and_then(Value::as_str) {
            cur.api_url = v.to_string();
        }
        if let Some(v) = obj.get("auth_mode") {
            cur.auth_mode = serde_json::from_value(v.clone())
                .map_err(|err| AppError::Validation(format!("invalid YOURLS auth_mode: {err}")))?;
        }
        if let Some(v) = obj.get("signature") {
            match v {
                Value::Null => cur.signature = None,
                Value::String(s) if !s.trim().is_empty() => {
                    cur.signature = Some(s.trim().to_string());
                }
                Value::String(_) => {} // empty string preserves stored signature
                _ => {
                    return Err(AppError::Validation(
                        "YOURLS signature must be a string or null".to_string(),
                    ));
                }
            }
        }
        if let Some(v) = obj.get("username") {
            match v {
                Value::Null => cur.username = None,
                Value::String(s) => {
                    let trimmed = s.trim();
                    cur.username = (!trimmed.is_empty()).then(|| trimmed.to_string());
                }
                _ => {
                    return Err(AppError::Validation(
                        "YOURLS username must be a string or null".to_string(),
                    ));
                }
            }
        }
        if let Some(v) = obj.get("password") {
            match v {
                Value::Null => cur.password = None,
                Value::String(s) if !s.is_empty() => cur.password = Some(s.clone()),
                Value::String(_) => {} // empty string preserves stored password
                _ => {
                    return Err(AppError::Validation(
                        "YOURLS password must be a string or null".to_string(),
                    ));
                }
            }
        }
        if let Some(v) = obj.get("exclude_domains") {
            cur.exclude_domains = serde_json::from_value(v.clone()).map_err(|err| {
                AppError::Validation(format!("invalid exclude_domains list: {err}"))
            })?;
        }
        if let Some(v) = obj.get("strict_mode").and_then(Value::as_bool) {
            cur.strict_mode = v;
        }
        if let Some(v) = obj.get("pass_title").and_then(Value::as_bool) {
            cur.pass_title = v;
        }
        if let Some(v) = obj.get("use_proxy").and_then(Value::as_bool) {
            cur.use_proxy = v;
        }

        let normalized = cur.validate_and_normalize()?;
        Ok(serde_json::to_value(normalized)?)
    }

    fn redact_instance_config(&self, cfg: &Value) -> Value {
        match serde_json::from_value::<YourlsProviderConfig>(cfg.clone()) {
            Ok(parsed) => parsed.redacted_view(),
            Err(_) => json!({}),
        }
    }

    async fn validate_route_option(&self, db: &Database, option: &Value) -> AppResult<()> {
        let parsed: LinkRewriteRouteOption =
            serde_json::from_value(option.clone()).map_err(|err| {
                AppError::Validation(format!("invalid link-rewrite route options: {err}"))
            })?;
        if let Some(provider_id) = parsed.provider_id {
            let instance = db.get_plugin_instance(provider_id).await?.ok_or_else(|| {
                AppError::Validation(format!(
                    "link-rewrite provider instance {provider_id} does not exist"
                ))
            })?;
            if instance.plugin_id != LINK_REWRITE_PLUGIN_ID {
                return Err(AppError::Validation(format!(
                    "plugin instance {provider_id} does not belong to plugin '{LINK_REWRITE_PLUGIN_ID}'"
                )));
            }
        }
        Ok(())
    }

    async fn test_instance(&self, http: &ProtectedHttpClient, cfg: &Value) -> AppResult<String> {
        let provider: YourlsProviderConfig = serde_json::from_value(cfg.clone())
            .map_err(|err| AppError::Validation(format!("invalid YOURLS config: {err}")))?;
        let provider = provider.validate_and_normalize()?;
        let test_url = "https://feedcourier.ellery.me/";
        let short = shorten_single_url(http, &provider, test_url, Some("FeedCourier")).await?;
        Ok(format!(
            "YOURLS provider verified ({} -> {short})",
            provider.api_url
        ))
    }

    async fn on_render(
        &self,
        ctx: &RenderContext<'_>,
        out: &mut RenderOverrides,
    ) -> AppResult<PluginResult> {
        let Some(raw_opt) = ctx.plugin_route_config else {
            return Ok(PluginResult::Continue);
        };
        let route_opt: LinkRewriteRouteOption = match serde_json::from_value(raw_opt.clone()) {
            Ok(opt) => opt,
            Err(_) => return Ok(PluginResult::Continue),
        };
        let Some(provider_id) = route_opt.provider_id else {
            return Ok(PluginResult::Continue);
        };

        let Some(instance) = ctx.db.get_plugin_instance(provider_id).await? else {
            out.warnings.push(format!(
                "Link Rewrite: provider #{provider_id} not found; kept original URLs"
            ));
            return Ok(PluginResult::Continue);
        };
        if instance.plugin_id != LINK_REWRITE_PLUGIN_ID {
            return Ok(PluginResult::Continue);
        }
        if !instance.enabled {
            out.warnings.push(format!(
                "Link Rewrite: provider '{}' is disabled; kept original URLs",
                instance.name
            ));
            return Ok(PluginResult::Continue);
        }

        let provider_cfg: YourlsProviderConfig = ctx
            .master_key
            .decrypt_json::<YourlsProviderConfig>(&instance.encrypted_config)?
            .validate_and_normalize()?;

        let self_host = provider_cfg.api_host();
        let current_html = out
            .content_html
            .as_deref()
            .or(ctx.entry.content_html.as_deref());
        let primary_url = out
            .primary_link
            .as_deref()
            .or(ctx.entry.canonical_url.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        // 1. Collect all unique eligible URLs in document order (primary link first, then `<a href>`).
        //    We look up the SQLite cache BEFORE applying `MAX_REWRITE_URLS_PER_ENTRY` so cached links
        //    never crowd out uncached links from being shortened.
        let mut candidate_urls: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        if let Some(p_url) = primary_url
            && is_eligible_rewrite_url(p_url, self_host.as_deref(), &provider_cfg.exclude_domains)
            && seen.insert(p_url.to_string())
        {
            candidate_urls.push(p_url.to_string());
        }

        if let Some(html) = current_html {
            for href in extract_anchor_hrefs(html) {
                if !is_eligible_rewrite_url(
                    &href,
                    self_host.as_deref(),
                    &provider_cfg.exclude_domains,
                ) {
                    continue;
                }
                if seen.insert(href.clone()) {
                    candidate_urls.push(href);
                }
            }
        }

        if candidate_urls.is_empty() {
            return Ok(PluginResult::Continue);
        }

        // 2. Check SQLite cache (`plugin_link_cache`) in a single batch query.
        let hash_to_url: Vec<(String, String)> = candidate_urls
            .iter()
            .map(|u| (sha256_hex(u.as_bytes()), u.clone()))
            .collect();
        let hashes: Vec<String> = hash_to_url.iter().map(|(h, _)| h.clone()).collect();
        let cached = ctx
            .db
            .get_plugin_link_cache_batch(instance.id, &hashes)
            .await?;

        let mut resolved_map: HashMap<String, String> =
            HashMap::with_capacity(candidate_urls.len());
        let mut cache_hit_hashes: Vec<String> = Vec::new();
        let mut misses: Vec<(String, String)> = Vec::new();

        for (hash, url) in hash_to_url {
            if let Some(short) = cached.get(&hash) {
                resolved_map.insert(url, short.clone());
                cache_hit_hashes.push(hash);
            } else {
                misses.push((hash, url));
            }
        }

        let mut truncated_cap = false;
        if misses.len() > MAX_REWRITE_URLS_PER_ENTRY {
            misses.truncate(MAX_REWRITE_URLS_PER_ENTRY);
            truncated_cap = true;
        }

        if ctx.write_cache && !cache_hit_hashes.is_empty() {
            let _ = ctx
                .db
                .touch_plugin_link_cache(instance.id, &cache_hit_hashes, now_unix())
                .await;
        }

        // 3. Call YOURLS API for cache misses with bounded concurrency (`4`) & timeout (`5s`).
        let title_for_primary = ctx
            .entry
            .title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let mut failed_count = 0usize;
        let mut first_error: Option<String> = None;
        let mut max_retry_after_secs: Option<u64> = None;

        if !misses.is_empty() {
            let http = ctx.http.clone();
            let cfg_ref = &provider_cfg;
            let results: Vec<(String, String, AppResult<String>)> = stream::iter(misses)
                .map(|(hash, url)| {
                    let http = http.clone();
                    let title_hint = if cfg_ref.pass_title {
                        if primary_url == Some(url.as_str()) {
                            title_for_primary
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    async move {
                        let res = shorten_single_url(&http, cfg_ref, &url, title_hint).await;
                        (hash, url, res)
                    }
                })
                .buffer_unordered(REWRITE_CONCURRENCY)
                .collect()
                .await;

            let mut to_cache: Vec<(String, String)> = Vec::new();
            for (hash, url, res) in results {
                match res {
                    Ok(short_url) => {
                        resolved_map.insert(url, short_url.clone());
                        to_cache.push((hash, short_url));
                    }
                    Err(err) => {
                        failed_count += 1;
                        if let AppError::RateLimited {
                            retry_after_secs: Some(secs),
                            ..
                        } = &err
                        {
                            max_retry_after_secs =
                                Some(max_retry_after_secs.map_or(*secs, |cur| cur.max(*secs)));
                        }
                        if first_error.is_none() {
                            first_error = Some(err.to_string());
                        }
                    }
                }
            }

            if ctx.write_cache && !to_cache.is_empty() {
                let _ = ctx
                    .db
                    .put_plugin_link_cache_batch(instance.id, &to_cache, now_unix())
                    .await;
            }
        }

        // 4. Strict mode -> retry whole delivery task on any shortening failure.
        if failed_count > 0 && provider_cfg.strict_mode {
            let err_hint = first_error.unwrap_or_else(|| "YOURLS request failed".to_string());
            return Ok(PluginResult::Retry {
                diagnostic: format!(
                    "Link Rewrite strict mode ({}): {failed_count}/{} link(s) failed ({err_hint})",
                    instance.name,
                    candidate_urls.len()
                ),
                retry_after_secs: max_retry_after_secs,
            });
        }

        if failed_count > 0 {
            let ok_count = resolved_map.len();
            let err_hint = first_error.unwrap_or_else(|| "YOURLS request failed".to_string());
            out.warnings.push(format!(
                "Link Rewrite partial ({ok_count}/{} rewritten, {failed_count} kept original: {err_hint})",
                candidate_urls.len()
            ));
        } else if truncated_cap {
            out.warnings.push(format!(
                "Link Rewrite capped at {MAX_REWRITE_URLS_PER_ENTRY} new uncached links; remaining uncached links kept original"
            ));
        }

        // 5. Apply resolved short links to primary link and HTML `<a href>` attributes.
        if let Some(p_url) = primary_url
            && let Some(short) = resolved_map.get(p_url)
        {
            out.primary_link = Some(short.clone());
        }

        if let Some(html) = current_html
            && !resolved_map.is_empty()
        {
            out.content_html = Some(rewrite_anchor_hrefs(html, &resolved_map));
        }

        Ok(PluginResult::Continue)
    }
}

/// Determine whether a URL should be shortened by `provider`:
/// - Only `http://` and `https://` URLs with a host (skips `mailto:`, `tel:`, `#anchors`, etc.)
/// - Skips URLs hosted on the YOURLS provider's own domain (prevents recursive shortening)
/// - Skips URLs matching any domain or wildcard (`*.example.com`) in `exclude_domains`
pub fn is_eligible_rewrite_url(
    raw_url: &str,
    provider_host: Option<&str>,
    exclude_domains: &[String],
) -> bool {
    let trimmed = raw_url.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return false;
    }
    let Ok(parsed) = Url::parse(trimmed) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = parsed
        .host_str()
        .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
        .filter(|h| !h.is_empty())
    else {
        return false;
    };
    if let Some(self_h) = provider_host
        && host == self_h.trim_end_matches('.').to_ascii_lowercase()
    {
        return false;
    }
    for pattern in exclude_domains {
        if domain_matches_pattern(&host, pattern) {
            return false;
        }
    }
    true
}

fn domain_matches_pattern(host: &str, pattern: &str) -> bool {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let p = pattern.trim().trim_end_matches('.').to_ascii_lowercase();
    if h.is_empty() || p.is_empty() {
        return false;
    }
    if let Some(suffix) = p.strip_prefix("*.") {
        !suffix.is_empty() && (h == suffix || h.ends_with(&format!(".{suffix}")))
    } else {
        h == p
    }
}

/// Execute a single YOURLS `action=shorturl&format=json` POST request via [`ProtectedHttpClient`].
async fn shorten_single_url(
    http: &ProtectedHttpClient,
    cfg: &YourlsProviderConfig,
    long_url: &str,
    title: Option<&str>,
) -> AppResult<String> {
    let mut form: Vec<(String, String)> = Vec::with_capacity(6);
    form.push(("action".to_string(), "shorturl".to_string()));
    form.push(("format".to_string(), "json".to_string()));
    form.push(("url".to_string(), long_url.to_string()));

    if let Some(t) = title.map(str::trim).filter(|s| !s.is_empty()) {
        let clipped: String = t.chars().take(200).collect();
        form.push(("title".to_string(), clipped));
    }

    match cfg.auth_mode {
        YourlsAuthMode::Signature => {
            if let Some(ref sig) = cfg.signature {
                form.push(("signature".to_string(), sig.clone()));
            }
        }
        YourlsAuthMode::TimedSignature => {
            let ts = now_unix().to_string();
            let sig_token = cfg.signature.as_deref().unwrap_or("");
            let hash = md5_hex(format!("{ts}{sig_token}").as_bytes());
            form.push(("timestamp".to_string(), ts));
            form.push(("signature".to_string(), hash));
        }
        YourlsAuthMode::Password => {
            if let Some(ref u) = cfg.username {
                form.push(("username".to_string(), u.clone()));
            }
            if let Some(ref p) = cfg.password {
                form.push(("password".to_string(), p.clone()));
            }
        }
    }

    let resp = http
        .post_form_bytes(
            &cfg.api_url,
            &form,
            cfg.use_proxy,
            REWRITE_REQUEST_TIMEOUT,
            MAX_YOURLS_RESPONSE_BYTES,
        )
        .await?;

    if resp.status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(AppError::RateLimited {
            message: "YOURLS API rate-limited (HTTP 429)".to_string(),
            retry_after_secs: resp.retry_after_secs,
        });
    }

    // YOURLS returns HTTP 200 for success and duplicate URLs (`code: "error:url"`), and some
    // reverse proxies emit HTTP 400 with `code: "error:url"` + `shorturl` for duplicates. All
    // other non-2xx statuses are rejected even if a proxy/gateway returned a JSON body.
    let body: Value = serde_json::from_slice(&resp.bytes).map_err(|err| {
        AppError::Upstream(format!(
            "YOURLS returned non-JSON response (HTTP {}): {err}",
            resp.status.as_u16()
        ))
    })?;

    let code = body
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let is_duplicate_400 = resp.status == reqwest::StatusCode::BAD_REQUEST && code == "error:url";

    if (resp.status.is_success() || is_duplicate_400)
        && let Some(short) = body
            .get("shorturl")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        let parsed = Url::parse(short).map_err(|err| {
            AppError::Upstream(format!("YOURLS returned invalid shorturl '{short}': {err}"))
        })?;
        if matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some() {
            return Ok(short.to_string());
        }
    }

    let message = body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("no shorturl returned");
    Err(AppError::Upstream(format!(
        "YOURLS API error (HTTP {}, {code}): {message}",
        resp.status.as_u16()
    )))
}

// =============================================================================
// HTML `<a href>` Extractor & Rewriter (strictly touches ONLY `<a href>`)
// =============================================================================

/// Detect if an opening tag starts a `<script>` or `<style>` raw-text block so we skip its body.
fn raw_text_block_tag(full_tag: &str) -> Option<&'static str> {
    let inner = full_tag.strip_prefix('<')?.strip_suffix('>')?.trim_start();
    if inner.is_empty()
        || inner.starts_with('/')
        || inner.starts_with('!')
        || inner.starts_with('?')
    {
        return None;
    }
    let (name, attr_tail) = split_tag_name_and_attrs(inner);
    let (_, self_closing) = parse_tag_attrs_and_self_closing(attr_tail);
    if self_closing {
        return None;
    }
    if name.eq_ignore_ascii_case("script") {
        Some("script")
    } else if name.eq_ignore_ascii_case("style") {
        Some("style")
    } else {
        None
    }
}

/// Find the byte offset immediately after `</script...>` or `</style...>` within `html[from..]`.
fn find_raw_text_close_end(html: &str, from: usize, tag: &str) -> usize {
    let needle = format!("</{tag}");
    let mut search = from;
    while let Some(rel) = find_ascii_case_insensitive(&html[search..], &needle) {
        let close_start = search + rel;
        let after_name = close_start + needle.len();
        let next_byte = html.as_bytes().get(after_name).copied();
        if matches!(next_byte, Some(b'>' | b'/') | None)
            || next_byte.is_some_and(|b| b.is_ascii_whitespace())
        {
            if let Some(gt_rel) = html[close_start..].find('>') {
                return close_start + gt_rel + 1;
            }
            return html.len();
        }
        search = after_name;
    }
    html.len()
}

/// Extract decoded `href` attribute values from all `<a ...>` opening tags in `html`, skipping
/// HTML comments and `<script>` / `<style>` raw-text blocks.
pub fn extract_anchor_hrefs(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = html.as_bytes();
    let mut idx = 0;

    while idx < bytes.len() {
        let Some(rel_lt) = html[idx..].find('<') else {
            break;
        };
        let lt = idx + rel_lt;
        if html[lt..].starts_with("<!--") {
            if let Some(end_c) = html[lt + 4..].find("-->") {
                idx = lt + 4 + end_c + 3;
                continue;
            }
            break;
        }
        let Some(tag_end) = find_html_tag_end(&html[lt..]) else {
            break;
        };
        let full_tag = &html[lt..=lt + tag_end];
        idx = lt + tag_end + 1;

        if let Some(raw_tag) = raw_text_block_tag(full_tag) {
            idx = find_raw_text_close_end(html, idx, raw_tag);
            continue;
        }

        if let Some((_, attrs)) = parse_anchor_tag(full_tag) {
            for attr in attrs {
                if attr.name.eq_ignore_ascii_case("href")
                    && let Some(val) = attr.value
                {
                    let trimmed = val.trim();
                    if !trimmed.is_empty() {
                        out.push(trimmed.to_string());
                    }
                }
            }
        }
    }
    out
}

/// Rewrite matching `<a href="...">` attributes in `html` according to `replacements`, leaving
/// every other tag (`<img>`, `<video>`, `<audio>`, `<source>`, `<script>`, `<style>`, text,
/// comments) byte-for-byte intact.
pub fn rewrite_anchor_hrefs(html: &str, replacements: &HashMap<String, String>) -> String {
    if replacements.is_empty() {
        return html.to_string();
    }

    let mut out = String::with_capacity(html.len() + 64);
    let bytes = html.as_bytes();
    let mut idx = 0;

    while idx < bytes.len() {
        let Some(rel_lt) = html[idx..].find('<') else {
            out.push_str(&html[idx..]);
            break;
        };
        let lt = idx + rel_lt;
        out.push_str(&html[idx..lt]);

        if html[lt..].starts_with("<!--") {
            if let Some(end_c) = html[lt + 4..].find("-->") {
                let end = lt + 4 + end_c + 3;
                out.push_str(&html[lt..end]);
                idx = end;
                continue;
            }
            out.push_str(&html[lt..]);
            break;
        }

        let Some(tag_end) = find_html_tag_end(&html[lt..]) else {
            out.push_str(&html[lt..]);
            break;
        };
        let full_tag = &html[lt..=lt + tag_end];
        idx = lt + tag_end + 1;

        if let Some(raw_tag) = raw_text_block_tag(full_tag) {
            let block_end = find_raw_text_close_end(html, idx, raw_tag);
            out.push_str(&html[lt..block_end]);
            idx = block_end;
            continue;
        }

        let Some((tag_name, mut attrs)) = parse_anchor_tag(full_tag) else {
            out.push_str(full_tag);
            continue;
        };

        let mut modified = false;
        for attr in &mut attrs {
            if attr.name.eq_ignore_ascii_case("href")
                && let Some(val) = attr.value.as_deref()
                && let Some(short) = replacements.get(val.trim())
            {
                attr.value = Some(short.clone());
                modified = true;
            }
        }

        if !modified {
            out.push_str(full_tag);
            continue;
        }

        out.push('<');
        out.push_str(&tag_name);
        for attr in &attrs {
            out.push(' ');
            out.push_str(&attr.name);
            if let Some(ref val) = attr.value {
                out.push_str("=\"");
                push_escaped_attr(&mut out, val);
                out.push('"');
            }
        }
        // `<a>` is a non-void HTML element; never emit `<a ... />`.
        out.push('>');
    }

    out
}

fn parse_anchor_tag(tag_str: &str) -> Option<(String, Vec<ParsedTagAttr>)> {
    let inner = tag_str.strip_prefix('<')?.strip_suffix('>')?;
    let trimmed = inner.trim_start();
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.starts_with('!')
        || trimmed.starts_with('?')
    {
        return None;
    }

    let (tag_name, attr_tail) = split_tag_name_and_attrs(trimmed);
    if !tag_name.eq_ignore_ascii_case("a") {
        return None;
    }

    let (attrs, _self_closing) = parse_tag_attrs_and_self_closing(attr_tail);
    Some((tag_name.to_ascii_lowercase(), attrs))
}

fn push_escaped_attr(out: &mut String, val: &str) {
    for ch in val.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            other => out.push(other),
        }
    }
}
