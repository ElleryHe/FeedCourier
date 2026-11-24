//! Out-of-process HTTP Bridge Plugin Protocol v1 (`processor` + `channel` + `on_delivered`).
//!
//! Bridges are operator-registered sidecar / HTTP services speaking JSON over HTTP(S) authenticated
//! with HMAC-SHA256 (`X-FeedCourier-Timestamp`, `X-FeedCourier-Signature: v1=<hex>`).
//!
//! Security & trust boundaries:
//! - Only the exact registered `(scheme, host, port)` of a bridge URL is contacted (dedicated
//!   `reqwest::Client` with `.no_proxy()` and `redirect::Policy::none()`), allowing Docker-internal
//!   sidecar hostnames without relaxing `SSRF_ALLOWLIST` for feeds or external URLs.
//! - `/deliver` timeout: 30s; all other endpoints (`/manifest`, `/validate`, `/test`,
//!   `/hooks/on_render`, `/hooks/on_delivered`): 10s. Response payload cap: 1 MiB.
//! - HTML returned by `/hooks/on_render` is always re-sanitized through `ammonia` before entering
//!   downstream channel renderers.
//! - Bridge checkpoint payloads are capped at 16 KiB.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use hmac::{Hmac, KeyInit as HmacKeyInit, Mac};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::Sha256;
use url::Url;

use crate::channels::{
    ChannelConfig, ChannelKind, ChannelSender, DeliveryCheckpoint, DeliveryContext,
    DeliveryOutcome, DestinationConfig, mask_secret,
};
use crate::crypto::sha256_hex;
use crate::db::Database;
use crate::db::models::now_unix;
use crate::error::{AppError, AppResult};
use crate::ingest::html::{html_to_plain_text, sanitize_and_extract_html};
use crate::net::{ProtectedHttpClient, parse_retry_after_secs};

use super::{
    HookKind, PLUGIN_API_VERSION, Plugin, PluginKind, PluginManifest, PluginRegistry, PluginResult,
    PluginRuntime, RenderContext, RenderOverrides, check_plugin_compatibility,
};

type HmacSha256 = Hmac<Sha256>;

pub const BRIDGE_DELIVER_TIMEOUT: Duration = Duration::from_secs(30);
pub const BRIDGE_DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
pub const BRIDGE_MAX_RESPONSE_BYTES: usize = 1024 * 1024;
pub const BRIDGE_MAX_CHECKPOINT_BYTES: usize = 16 * 1024;
pub const BRIDGE_SIGNATURE_WINDOW_SECS: i64 = 300;

pub const HEADER_TIMESTAMP: &str = "X-FeedCourier-Timestamp";
pub const HEADER_SIGNATURE: &str = "X-FeedCourier-Signature";
pub const HEADER_PLUGIN_ID: &str = "X-FeedCourier-Plugin-Id";
pub const HEADER_IDEMPOTENCY_KEY: &str = "X-FeedCourier-Idempotency-Key";

static DEBUG_NONCE_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_debug_nonce() -> String {
    let seq = DEBUG_NONCE_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{}:{seq}", chrono::Utc::now().timestamp_millis())
}

/// Compute `v1=<hex(HMAC-SHA256(secret, "{timestamp}.{body}"))>`.
pub fn sign_bridge_request(secret: &str, timestamp: i64, body: &[u8]) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    format!("v1={}", hex::encode(mac.finalize().into_bytes()))
}

/// Verify a bridge HMAC signature with ±300s replay window (used by tests and reference helpers).
pub fn verify_bridge_signature(
    secret: &str,
    timestamp: i64,
    body: &[u8],
    header_sig: &str,
    now_secs: i64,
) -> bool {
    if (now_secs - timestamp).abs() > BRIDGE_SIGNATURE_WINDOW_SECS {
        return false;
    }
    let expected = sign_bridge_request(secret, timestamp, body);
    crate::crypto::constant_time_eq(expected.as_bytes(), header_sig.trim().as_bytes())
}

/// Validate and normalize an operator-registered bridge base URL (`http://` or `https://`,
/// explicit host, no userinfo/query/fragment, trailing slash stripped).
pub fn validate_bridge_url(raw: &str) -> AppResult<String> {
    let trimmed = raw.trim();
    let parsed = Url::parse(trimmed)
        .map_err(|err| AppError::Validation(format!("invalid bridge_url '{trimmed}': {err}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(AppError::Validation(format!(
            "bridge_url must use http or https scheme, got '{}'",
            parsed.scheme()
        )));
    }
    if parsed.host_str().is_none() {
        return Err(AppError::Validation(
            "bridge_url must include a hostname".to_string(),
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(AppError::Validation(
            "bridge_url must not contain embedded credentials".to_string(),
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(AppError::Validation(
            "bridge_url must not contain query parameters or fragments".to_string(),
        ));
    }
    Ok(parsed.as_str().trim_end_matches('/').to_string())
}

/// Validate a plugin ID (`^[a-z0-9][a-z0-9_-]{1,63}$`).
pub fn validate_plugin_id(id: &str) -> AppResult<()> {
    let bytes = id.as_bytes();
    let ok = bytes.len() >= 2
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'));
    if !ok {
        return Err(AppError::Validation(format!(
            "invalid plugin id '{id}': must be 2–64 lowercase ASCII letters, digits, '-' or '_'"
        )));
    }
    Ok(())
}

/// Validate a fetched bridge manifest against Protocol v1 requirements & core compatibility.
pub fn validate_bridge_manifest(mut manifest: PluginManifest) -> AppResult<PluginManifest> {
    manifest.id = manifest.id.trim().to_string();
    validate_plugin_id(&manifest.id)?;
    if manifest.name.trim().is_empty() {
        return Err(AppError::Validation(
            "bridge manifest 'name' must not be empty".to_string(),
        ));
    }
    if manifest.version.trim().is_empty() {
        return Err(AppError::Validation(
            "bridge manifest 'version' must not be empty".to_string(),
        ));
    }
    if manifest.api_version.trim() != PLUGIN_API_VERSION {
        return Err(AppError::Validation(format!(
            "unsupported bridge api_version '{}' (expected '{PLUGIN_API_VERSION}')",
            manifest.api_version
        )));
    }
    let (compat, reason) =
        check_plugin_compatibility(&manifest.api_version, manifest.min_core.as_deref());
    if !compat {
        return Err(AppError::Validation(reason.unwrap_or_else(|| {
            "plugin is incompatible with this FeedCourier version".to_string()
        })));
    }
    if manifest.kind == PluginKind::Processor && manifest.hooks.is_empty() {
        return Err(AppError::Validation(
            "processor bridge manifest must declare at least one hook (e.g. on_render or on_delivered)"
                .to_string(),
        ));
    }
    if !manifest.homepage.trim().is_empty()
        && super::sanitize_http_url(Some(manifest.homepage.clone())).is_none()
    {
        manifest.homepage.clear();
    }
    manifest.runtime = PluginRuntime::Bridge;
    Ok(manifest)
}

/// Isolated HTTP client bound strictly to ONE registered bridge URL (exact host:port, no proxy,
/// no redirects, bounded response size, HMAC-SHA256 signed requests).
#[derive(Clone)]
pub struct BridgeClient {
    http: Client,
    base_url: String,
    secret: String,
    plugin_id: String,
}

impl std::fmt::Debug for BridgeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeClient")
            .field("base_url", &self.base_url)
            .field("plugin_id", &self.plugin_id)
            .finish()
    }
}

impl BridgeClient {
    pub fn new(base_url: String, secret: String, plugin_id: String) -> AppResult<Self> {
        let normalized_url = validate_bridge_url(&base_url)?;
        let http = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|err| {
                AppError::Internal(anyhow::anyhow!("bridge HTTP client error: {err}"))
            })?;
        Ok(Self {
            http,
            base_url: normalized_url,
            secret,
            plugin_id,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    async fn read_bounded_body(resp: reqwest::Response) -> AppResult<(u16, Option<u64>, Vec<u8>)> {
        let status = resp.status().as_u16();
        if (300..=399).contains(&status) {
            return Err(AppError::SsrfBlocked(format!(
                "redirect (HTTP {status}) is disallowed on bridge plugin requests"
            )));
        }
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_retry_after_secs);

        if let Some(len) = resp.content_length()
            && len > BRIDGE_MAX_RESPONSE_BYTES as u64
        {
            return Err(AppError::Upstream(format!(
                "bridge response Content-Length ({len} bytes) exceeds 1 MiB limit"
            )));
        }

        let mut stream = resp.bytes_stream();
        let mut buf = Vec::new();
        use futures_util::StreamExt;
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|err| {
                AppError::Upstream(format!("failed reading bridge response body: {err}"))
            })?;
            if buf.len().saturating_add(bytes.len()) > BRIDGE_MAX_RESPONSE_BYTES {
                return Err(AppError::Upstream(
                    "bridge response body exceeded 1 MiB limit".to_string(),
                ));
            }
            buf.extend_from_slice(&bytes);
        }
        Ok((status, retry_after, buf))
    }

    /// `GET /manifest` with signed empty body.
    pub async fn fetch_manifest(&self) -> AppResult<PluginManifest> {
        let url = format!("{}/manifest", self.base_url);
        let ts = now_unix();
        let sig = sign_bridge_request(&self.secret, ts, b"");
        let resp = self
            .http
            .get(&url)
            .timeout(BRIDGE_DEFAULT_TIMEOUT)
            .header(HEADER_TIMESTAMP, ts.to_string())
            .header(HEADER_SIGNATURE, sig)
            .header(HEADER_PLUGIN_ID, &self.plugin_id)
            .send()
            .await
            .map_err(|err| {
                AppError::Upstream(format!("failed to connect to bridge '{url}': {err}"))
            })?;

        let (status, _, body) = Self::read_bounded_body(resp).await?;
        if !(200..300).contains(&status) {
            let preview = String::from_utf8_lossy(&body)
                .chars()
                .take(240)
                .collect::<String>();
            return Err(AppError::Upstream(format!(
                "bridge GET /manifest returned HTTP {status}: {preview}"
            )));
        }
        let manifest: PluginManifest = serde_json::from_slice(&body).map_err(|err| {
            AppError::Validation(format!("invalid JSON from bridge GET /manifest: {err}"))
        })?;
        validate_bridge_manifest(manifest)
    }

    /// Signed JSON `POST` to a relative bridge endpoint (e.g. `"/validate"`, `"/deliver"`).
    pub async fn post_json(
        &self,
        endpoint: &str,
        payload: &Value,
        idempotency_key: Option<&str>,
        timeout: Duration,
    ) -> AppResult<(u16, Option<u64>, Vec<u8>)> {
        let path = if endpoint.starts_with('/') {
            endpoint.to_string()
        } else {
            format!("/{endpoint}")
        };
        let url = format!("{}{path}", self.base_url);
        let body = serde_json::to_vec(payload)?;
        let ts = now_unix();
        let sig = sign_bridge_request(&self.secret, ts, &body);

        let mut req = self
            .http
            .post(&url)
            .timeout(timeout)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(HEADER_TIMESTAMP, ts.to_string())
            .header(HEADER_SIGNATURE, sig)
            .header(HEADER_PLUGIN_ID, &self.plugin_id);
        if let Some(key) = idempotency_key {
            req = req.header(HEADER_IDEMPOTENCY_KEY, key);
        }

        let resp = req.body(body).send().await.map_err(|err| {
            if err.is_timeout() {
                AppError::Upstream(format!("bridge request to '{path}' timed out: {err}"))
            } else {
                AppError::Upstream(format!("bridge request to '{path}' failed: {err}"))
            }
        })?;

        Self::read_bounded_body(resp).await
    }

    /// `POST /validate` for `account`, `destination`, or `instance` config.
    pub async fn validate_remote(&self, scope: &str, config: &Value) -> AppResult<Value> {
        let payload = json!({
            "scope": scope,
            "config": config,
        });
        let (status, _, body) = self
            .post_json("/validate", &payload, None, BRIDGE_DEFAULT_TIMEOUT)
            .await?;
        if !(200..300).contains(&status) {
            let parsed_err = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .unwrap_or_else(|| {
                    String::from_utf8_lossy(&body)
                        .chars()
                        .take(240)
                        .collect::<String>()
                });
            return Err(AppError::Validation(format!(
                "bridge validation failed ({status}): {parsed_err}"
            )));
        }
        let parsed: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({ "ok": true }));
        if parsed.get("ok").and_then(Value::as_bool) == Some(false) {
            let msg = parsed
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("configuration rejected by bridge");
            return Err(AppError::Validation(msg.to_string()));
        }
        Ok(parsed
            .get("normalized_config")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or_else(|| config.clone()))
    }

    /// `POST /test` for interactive connectivity check.
    pub async fn test_remote(&self, payload: &Value) -> AppResult<String> {
        let (status, _, body) = self
            .post_json("/test", payload, None, BRIDGE_DEFAULT_TIMEOUT)
            .await?;
        let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            let msg = parsed
                .get("error")
                .or_else(|| parsed.get("message"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| {
                    String::from_utf8_lossy(&body)
                        .chars()
                        .take(240)
                        .collect::<String>()
                });
            return Err(AppError::Upstream(format!(
                "bridge connectivity test failed (HTTP {status}): {msg}"
            )));
        }
        if parsed.get("ok").and_then(Value::as_bool) == Some(false) {
            let msg = parsed
                .get("error")
                .or_else(|| parsed.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("bridge reported test failure");
            return Err(AppError::Upstream(msg.to_string()));
        }
        Ok(parsed
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Bridge connectivity verified")
            .to_string())
    }

    /// `POST /hooks/on_delivered` with idempotency key.
    pub async fn send_on_delivered(&self, idempotency_key: &str, payload: &Value) -> AppResult<()> {
        let body = json!({
            "event": "on_delivered",
            "idempotency_key": idempotency_key,
            "payload": payload,
        });
        let (status, _, resp_bytes) = self
            .post_json(
                "/hooks/on_delivered",
                &body,
                Some(idempotency_key),
                BRIDGE_DEFAULT_TIMEOUT,
            )
            .await?;
        if (200..300).contains(&status) {
            Ok(())
        } else {
            let preview = String::from_utf8_lossy(&resp_bytes)
                .chars()
                .take(200)
                .collect::<String>();
            Err(AppError::Upstream(format!(
                "bridge POST /hooks/on_delivered returned HTTP {status}: {preview}"
            )))
        }
    }
}

// =============================================================================
// JSON Schema Validation, Secret-Preserving PATCH Merge & Redaction Helpers
// =============================================================================

fn is_schema_secret_prop(key: &str, prop_schema: Option<&Value>) -> bool {
    if let Some(prop) = prop_schema {
        if prop.get("format").and_then(Value::as_str) == Some("password") {
            return true;
        }
        if prop.get("x-secret").and_then(Value::as_bool) == Some(true) {
            return true;
        }
    }
    crate::logging::redact::is_sensitive_field_name(key)
}

/// Validate a config JSON object against a declarative JSON Schema (`type: object`, `required`, `properties`).
pub fn validate_config_against_schema(schema: &Value, cfg: &Value) -> AppResult<Value> {
    validate_config_against_schema_inner(schema, cfg, true)
}

fn validate_config_against_schema_inner(
    schema: &Value,
    cfg: &Value,
    apply_defaults: bool,
) -> AppResult<Value> {
    let obj = cfg
        .as_object()
        .ok_or_else(|| AppError::Validation("configuration must be a JSON object".to_string()))?;

    let props = schema.get("properties").and_then(Value::as_object);
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let mut normalized = Map::new();

    if let Some(props_map) = props {
        for (key, prop) in props_map {
            let is_req = required.iter().any(|r| r == key);
            match obj.get(key) {
                None | Some(Value::Null) => {
                    if apply_defaults && let Some(default_val) = prop.get("default") {
                        normalized.insert(key.clone(), default_val.clone());
                    } else if is_req {
                        return Err(AppError::Validation(format!(
                            "missing required field '{key}'"
                        )));
                    }
                }
                Some(val) => {
                    let expected_type = prop.get("type").and_then(Value::as_str);
                    match expected_type {
                        Some("string") => {
                            let s = val.as_str().ok_or_else(|| {
                                AppError::Validation(format!("field '{key}' must be a string"))
                            })?;
                            let trimmed = if is_schema_secret_prop(key, Some(prop)) {
                                s
                            } else {
                                s.trim()
                            };
                            if is_req && trimmed.is_empty() {
                                return Err(AppError::Validation(format!(
                                    "required field '{key}' must not be empty"
                                )));
                            }
                            if !trimmed.is_empty()
                                && prop.get("format").and_then(Value::as_str) == Some("uri")
                            {
                                let parsed = Url::parse(trimmed).map_err(|err| {
                                    AppError::Validation(format!(
                                        "invalid URL in field '{key}': {err}"
                                    ))
                                })?;
                                if !matches!(parsed.scheme(), "http" | "https")
                                    || parsed.host_str().is_none()
                                {
                                    return Err(AppError::Validation(format!(
                                        "field '{key}' must be an http/https URL"
                                    )));
                                }
                            }
                            if let Some(enums) = prop.get("enum").and_then(Value::as_array)
                                && !enums.is_empty()
                                && !enums.iter().any(|e| e.as_str() == Some(trimmed))
                            {
                                return Err(AppError::Validation(format!(
                                    "field '{key}' value '{trimmed}' is not an allowed enum option"
                                )));
                            }
                            if !trimmed.is_empty() {
                                normalized.insert(key.clone(), Value::String(trimmed.to_string()));
                            }
                        }
                        Some("boolean") => {
                            if !val.is_boolean() {
                                return Err(AppError::Validation(format!(
                                    "field '{key}' must be a boolean"
                                )));
                            }
                            normalized.insert(key.clone(), val.clone());
                        }
                        Some("integer") => {
                            if !val.is_i64() && !val.is_u64() {
                                return Err(AppError::Validation(format!(
                                    "field '{key}' must be an integer"
                                )));
                            }
                            normalized.insert(key.clone(), val.clone());
                        }
                        Some("number") => {
                            if !val.is_number() {
                                return Err(AppError::Validation(format!(
                                    "field '{key}' must be a number"
                                )));
                            }
                            normalized.insert(key.clone(), val.clone());
                        }
                        Some("array") => {
                            if !val.is_array() {
                                return Err(AppError::Validation(format!(
                                    "field '{key}' must be an array"
                                )));
                            }
                            normalized.insert(key.clone(), val.clone());
                        }
                        _ => {
                            normalized.insert(key.clone(), val.clone());
                        }
                    }
                }
            }
        }
        // Preserve any extra keys not listed in schema properties (forward-compatible).
        for (k, v) in obj {
            if !props_map.contains_key(k) && !v.is_null() {
                normalized.insert(k.clone(), v.clone());
            }
        }
    } else {
        for (k, v) in obj {
            if !v.is_null() {
                normalized.insert(k.clone(), v.clone());
            }
        }
    }

    Ok(Value::Object(normalized))
}

/// Merge a partial PATCH config onto an existing decrypted config, preserving stored secrets
/// when omitted or empty, clearing optional non-secret fields when explicitly set to `null`.
pub fn merge_config_against_schema(
    schema: &Value,
    existing: &Value,
    patch: &Value,
) -> AppResult<Value> {
    let mut merged = existing.as_object().cloned().unwrap_or_default();
    let patch_obj = patch
        .as_object()
        .ok_or_else(|| AppError::Validation("patch config must be a JSON object".to_string()))?;
    let props = schema.get("properties").and_then(Value::as_object);

    for (key, val) in patch_obj {
        let prop_schema = props.and_then(|p| p.get(key));
        let secret = is_schema_secret_prop(key, prop_schema);
        if val.is_null() {
            merged.remove(key);
        } else if secret && val.as_str().is_some_and(str::is_empty) {
            // Blank secret string on PATCH preserves existing stored secret.
            continue;
        } else {
            merged.insert(key.clone(), val.clone());
        }
    }

    validate_config_against_schema_inner(schema, &Value::Object(merged), false)
}

/// Redact secret fields in a config object using an explicit JSON schema.
pub fn redact_config_with_schema(schema: &Value, cfg: &Value) -> Value {
    let Some(obj) = cfg.as_object() else {
        return Value::Object(Map::new());
    };
    let props = schema.get("properties").and_then(Value::as_object);
    let mut out = Map::new();
    for (k, v) in obj {
        let prop_schema = props.and_then(|p| p.get(k));
        if is_schema_secret_prop(k, prop_schema) {
            let has_val = v.as_str().is_some_and(|s| !s.is_empty());
            out.insert(format!("has_{k}"), Value::Bool(has_val));
            if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
                out.insert(format!("{k}_hint"), Value::String(mask_secret(s)));
            }
        } else {
            out.insert(k.clone(), v.clone());
        }
    }
    Value::Object(out)
}

/// Generic fallback secret redactor when schema metadata is not directly in scope.
pub fn redact_config_generic(cfg: &Value) -> Value {
    redact_config_with_schema(&json!({}), cfg)
}

// =============================================================================
// Bridge Processor Plugin Adapter (`Plugin` trait)
// =============================================================================

#[derive(Debug, Deserialize)]
struct BridgeOnRenderResponse {
    #[serde(default)]
    outcome: Option<String>,
    #[serde(default)]
    content_html: Option<String>,
    #[serde(default)]
    primary_link: Option<String>,
    #[serde(default)]
    warnings: Vec<String>,
    #[serde(default)]
    diagnostic: Option<String>,
    #[serde(default)]
    retry_after_secs: Option<u64>,
}

/// Out-of-process HTTP bridge processor plugin implementing FeedCourier's [`Plugin`] trait.
pub struct BridgeProcessorPlugin {
    manifest: PluginManifest,
    client: BridgeClient,
}

impl BridgeProcessorPlugin {
    pub fn new(manifest: PluginManifest, client: BridgeClient) -> Self {
        Self { manifest, client }
    }

    pub fn client(&self) -> &BridgeClient {
        &self.client
    }
}

#[async_trait]
impl Plugin for BridgeProcessorPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn validate_instance_config(&self, cfg: &Value) -> AppResult<Value> {
        validate_config_against_schema(&self.manifest.instance_schema, cfg)
    }

    fn merge_instance_config(&self, existing: &Value, patch: &Value) -> AppResult<Value> {
        merge_config_against_schema(&self.manifest.instance_schema, existing, patch)
    }

    fn redact_instance_config(&self, cfg: &Value) -> Value {
        redact_config_with_schema(&self.manifest.instance_schema, cfg)
    }

    async fn validate_route_option(&self, db: &Database, option: &Value) -> AppResult<()> {
        if option.is_null() {
            return Ok(());
        }
        let obj = option.as_object().ok_or_else(|| {
            AppError::Validation(format!(
                "plugin '{}' route option must be a JSON object",
                self.manifest.id
            ))
        })?;
        if let Some(enabled_val) = obj.get("enabled")
            && !enabled_val.is_null()
            && !enabled_val.is_boolean()
        {
            return Err(AppError::Validation(format!(
                "plugin '{}' route option 'enabled' must be a boolean",
                self.manifest.id
            )));
        }
        if let Some(strict_val) = obj.get("strict_mode")
            && !strict_val.is_null()
            && !strict_val.is_boolean()
        {
            return Err(AppError::Validation(format!(
                "plugin '{}' route option 'strict_mode' must be a boolean",
                self.manifest.id
            )));
        }
        if let Some(pid_val) = obj.get("provider_id")
            && !pid_val.is_null()
        {
            let pid = pid_val.as_i64().filter(|&v| v > 0).ok_or_else(|| {
                AppError::Validation("provider_id must be a positive integer".to_string())
            })?;
            let inst = db.get_plugin_instance(pid).await?.ok_or_else(|| {
                AppError::Validation(format!(
                    "plugin instance {pid} not found for plugin '{}'",
                    self.manifest.id
                ))
            })?;
            if inst.plugin_id != self.manifest.id {
                return Err(AppError::Validation(format!(
                    "plugin instance {pid} does not belong to plugin '{}'",
                    self.manifest.id
                )));
            }
        }
        if obj.get("enabled").and_then(Value::as_bool) != Some(false) {
            validate_config_against_schema(&self.manifest.route_option_schema, option)?;
        }
        Ok(())
    }

    async fn test_instance(&self, _http: &ProtectedHttpClient, cfg: &Value) -> AppResult<String> {
        let validated = self.validate_instance_config(cfg)?;
        let normalized = self.client.validate_remote("instance", &validated).await?;
        self.client
            .test_remote(&json!({ "scope": "instance", "config": normalized }))
            .await
    }

    async fn effective_strict_mode(&self, ctx: &RenderContext<'_>) -> bool {
        let Some(route_opt) = ctx.plugin_route_config else {
            return false;
        };
        if route_opt.get("strict_mode").and_then(Value::as_bool) == Some(true) {
            return true;
        }
        if let Some(pid) = route_opt.get("provider_id").and_then(Value::as_i64)
            && let Ok(Some(inst)) = ctx.db.get_plugin_instance(pid).await
            && inst.plugin_id == self.manifest.id
            && inst.enabled
            && let Ok(decrypted) = ctx.master_key.decrypt_json::<Value>(&inst.encrypted_config)
            && decrypted.get("strict_mode").and_then(Value::as_bool) == Some(true)
        {
            return true;
        }
        false
    }

    async fn on_render(
        &self,
        ctx: &RenderContext<'_>,
        out: &mut RenderOverrides,
    ) -> AppResult<PluginResult> {
        if !self.manifest.hooks.contains(&HookKind::OnRender) {
            return Ok(PluginResult::Continue);
        }

        let route_opt = ctx
            .plugin_route_config
            .cloned()
            .unwrap_or_else(|| json!({}));
        let mut instance_cfg = Value::Null;
        let mut strict_mode = route_opt
            .get("strict_mode")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if let Some(pid) = route_opt.get("provider_id").and_then(Value::as_i64) {
            match ctx.db.get_plugin_instance(pid).await? {
                Some(inst) if inst.plugin_id == self.manifest.id && inst.enabled => {
                    let decrypted: Value = ctx.master_key.decrypt_json(&inst.encrypted_config)?;
                    if decrypted.get("strict_mode").and_then(Value::as_bool) == Some(true) {
                        strict_mode = true;
                    }
                    instance_cfg = decrypted;
                }
                Some(inst) if !inst.enabled => {
                    out.warnings.push(format!(
                        "Plugin '{}' provider '{}' is disabled; kept original content",
                        self.manifest.id, inst.name
                    ));
                    return Ok(PluginResult::Continue);
                }
                _ => {
                    out.warnings.push(format!(
                        "Plugin '{}' provider #{pid} not found; kept original content",
                        self.manifest.id
                    ));
                    return Ok(PluginResult::Continue);
                }
            }
        }

        let current_html = out
            .content_html
            .as_deref()
            .or(ctx.entry.content_html.as_deref());
        let primary_link = out
            .primary_link
            .as_deref()
            .or(ctx.entry.canonical_url.as_deref());
        let idempotency_input = if !ctx.write_cache {
            format!(
                "on_render-debug:{}:{}:{}:{}",
                self.manifest.id,
                ctx.route.id,
                ctx.entry.fingerprint_sha256,
                next_debug_nonce()
            )
        } else {
            format!(
                "on_render:{}:{}:{}",
                self.manifest.id, ctx.route.id, ctx.entry.fingerprint_sha256
            )
        };
        let idempotency_key = sha256_hex(idempotency_input.as_bytes());

        let payload = json!({
            "idempotency_key": idempotency_key,
            "plugin_id": self.manifest.id,
            "instance_config": instance_cfg,
            "route_option": route_opt,
            "entry": {
                "title": ctx.entry.title,
                "url": primary_link,
                "summary_text": ctx.entry.summary_text,
                "content_html": current_html,
                "author": ctx.entry.author,
                "published_at": ctx.entry.published_at,
            },
            "feed": {
                "id": ctx.feed.id,
                "title": ctx.feed.title,
                "url": ctx.feed.url,
            },
            "route": {
                "id": ctx.route.id,
                "name": ctx.route.name,
                "locale": ctx.route_options.locale.map(|l| l.as_str()),
                "timezone": ctx.route_options.timezone,
            },
            "write_cache": ctx.write_cache,
        });

        let call_res = self
            .client
            .post_json(
                "/hooks/on_render",
                &payload,
                Some(&idempotency_key),
                BRIDGE_DEFAULT_TIMEOUT,
            )
            .await;

        let (status, retry_after, body) = match call_res {
            Ok(v) => v,
            Err(err) => {
                let msg = format!("Plugin '{}' on_render error: {err}", self.manifest.id);
                if strict_mode {
                    return Ok(PluginResult::Retry {
                        diagnostic: msg,
                        retry_after_secs: None,
                    });
                }
                out.warnings.push(msg);
                return Ok(PluginResult::Continue);
            }
        };

        if !(200..300).contains(&status) {
            let preview = String::from_utf8_lossy(&body)
                .chars()
                .take(200)
                .collect::<String>();
            let msg = format!(
                "Plugin '{}' on_render HTTP {status}: {preview}",
                self.manifest.id
            );
            if strict_mode {
                return Ok(PluginResult::Retry {
                    diagnostic: msg,
                    retry_after_secs: retry_after,
                });
            }
            out.warnings.push(msg);
            return Ok(PluginResult::Continue);
        }

        let parsed: BridgeOnRenderResponse = match serde_json::from_slice(&body) {
            Ok(p) => p,
            Err(err) => {
                let msg = format!(
                    "Plugin '{}' returned invalid JSON from /hooks/on_render: {err}",
                    self.manifest.id
                );
                if strict_mode {
                    return Ok(PluginResult::Retry {
                        diagnostic: msg,
                        retry_after_secs: None,
                    });
                }
                out.warnings.push(msg);
                return Ok(PluginResult::Continue);
            }
        };

        if parsed
            .outcome
            .as_deref()
            .is_some_and(|o| o.eq_ignore_ascii_case("retry"))
        {
            let diag = parsed
                .diagnostic
                .unwrap_or_else(|| format!("Plugin '{}' requested retry", self.manifest.id));
            if strict_mode {
                return Ok(PluginResult::Retry {
                    diagnostic: diag,
                    retry_after_secs: parsed.retry_after_secs.or(retry_after),
                });
            }
            out.warnings.push(diag);
            return Ok(PluginResult::Continue);
        }

        // Security invariant: HTML returned from an external bridge MUST pass through ammonia!
        if let Some(new_html) = parsed.content_html {
            let base_url = primary_link.and_then(|u| Url::parse(u).ok());
            let sanitized = sanitize_and_extract_html(&new_html, base_url.as_ref());
            out.content_html = Some(sanitized.content_html);
        }

        if let Some(new_link) = parsed.primary_link {
            if let Some(safe) = super::sanitize_http_url(Some(new_link)) {
                out.primary_link = Some(safe);
            } else {
                out.warnings.push(format!(
                    "Plugin '{}' returned non-http(s) primary_link; ignored",
                    self.manifest.id
                ));
            }
        }

        for w in parsed.warnings.into_iter().take(5) {
            out.warnings.push(crate::logging::redact::redact_text(&w));
        }

        Ok(PluginResult::Continue)
    }
}

// =============================================================================
// Bridge Channel Plugin Adapter (`ChannelSender` trait for `ChannelKind::Plugin`)
// =============================================================================

#[derive(Debug, Deserialize)]
struct BridgeDeliverResponse {
    outcome: String,
    #[serde(default)]
    retry_after_secs: Option<u64>,
    #[serde(default)]
    next_window_at: Option<i64>,
    #[serde(default)]
    checkpoint: Option<Value>,
    #[serde(default)]
    diagnostic: Option<String>,
    #[serde(default)]
    external_id: Option<String>,
}

/// Out-of-process HTTP bridge delivery adapter implementing [`ChannelSender`] for `ChannelKind::Plugin`.
#[derive(Clone)]
pub struct BridgeChannelSender {
    registry: PluginRegistry,
}

impl BridgeChannelSender {
    pub fn new(registry: PluginRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl ChannelSender for BridgeChannelSender {
    fn kind(&self) -> ChannelKind {
        ChannelKind::Plugin
    }

    async fn send(&self, ctx: &DeliveryContext<'_>) -> AppResult<DeliveryOutcome> {
        let ChannelConfig::Plugin(acct_cfg) = ctx.channel_config else {
            return Err(AppError::Validation(
                "BridgeChannelSender expected ChannelConfig::Plugin".to_string(),
            ));
        };
        let DestinationConfig::Plugin(dest_cfg) = ctx.destination_config else {
            return Err(AppError::Validation(
                "BridgeChannelSender expected DestinationConfig::Plugin".to_string(),
            ));
        };

        let Some(bridge) = self.registry.get_bridge(&acct_cfg.plugin_id) else {
            return Ok(DeliveryOutcome::PermanentFailure {
                status_code: None,
                checkpoint: ctx.checkpoint.clone(),
                diagnostic: format!(
                    "Channel plugin '{}' is no longer installed",
                    acct_cfg.plugin_id
                ),
            });
        };
        if bridge.manifest.kind != PluginKind::Channel {
            return Ok(DeliveryOutcome::PermanentFailure {
                status_code: None,
                checkpoint: ctx.checkpoint.clone(),
                diagnostic: format!(
                    "Plugin '{}' has kind '{}' instead of 'channel'",
                    acct_cfg.plugin_id,
                    bridge.manifest.kind.as_str()
                ),
            });
        }
        if !bridge.compatible {
            return Ok(DeliveryOutcome::Deferred {
                next_window_at: now_unix().saturating_add(300),
                checkpoint: ctx.checkpoint.clone(),
                diagnostic: format!(
                    "Channel plugin '{}' is incompatible with current core: {}",
                    acct_cfg.plugin_id,
                    bridge
                        .compatibility_reason
                        .as_deref()
                        .unwrap_or("version mismatch")
                ),
            });
        }
        if !bridge.enabled {
            return Ok(DeliveryOutcome::Deferred {
                next_window_at: now_unix().saturating_add(300),
                checkpoint: ctx.checkpoint.clone(),
                diagnostic: format!(
                    "Channel plugin '{}' is currently disabled",
                    acct_cfg.plugin_id
                ),
            });
        }

        let effective_html = ctx.effective_content_html();
        let effective_url = ctx.effective_primary_link();
        let plain_text = effective_html
            .map(html_to_plain_text)
            .filter(|s| !s.is_empty())
            .or_else(|| ctx.entry.summary_text.clone())
            .or_else(|| ctx.entry.first_sentence.clone())
            .unwrap_or_default();

        let idempotency_input = if ctx.task.id == 0 {
            format!(
                "deliver-debug:{}:{}:{}:{}",
                acct_cfg.plugin_id,
                ctx.destination.id,
                ctx.entry.fingerprint_sha256,
                next_debug_nonce()
            )
        } else {
            format!(
                "deliver:{}:{}:{}:{}",
                acct_cfg.plugin_id, ctx.destination.id, ctx.task.id, ctx.entry.fingerprint_sha256
            )
        };
        let idempotency_key = sha256_hex(idempotency_input.as_bytes());
        let cp_value = match &ctx.checkpoint {
            Some(DeliveryCheckpoint::Plugin { state }) => state.clone(),
            _ => Value::Null,
        };

        let media: Vec<Value> = ctx
            .entry
            .media()
            .into_iter()
            .map(|m| {
                json!({
                    "kind": m.kind,
                    "url": m.url,
                    "mime_type": m.mime_type,
                    "caption": m.caption,
                })
            })
            .collect();

        let payload = json!({
            "idempotency_key": idempotency_key,
            "task_id": ctx.task.id,
            "attempt": ctx.task.attempt_count,
            "account": acct_cfg.config,
            "destination": dest_cfg.config,
            "entry": {
                "title": ctx.entry.title,
                "url": effective_url,
                "summary_text": ctx.entry.summary_text,
                "content_html": effective_html,
                "plain_text": plain_text,
                "author": ctx.entry.author,
                "published_at": ctx.entry.published_at,
                "media": media,
            },
            "feed": {
                "id": ctx.feed.id,
                "title": ctx.feed.title,
                "url": ctx.feed.url,
            },
            "route": {
                "id": ctx.route.id,
                "name": ctx.route.name,
                "locale": ctx.route_options.locale.map(|l| l.as_str()),
                "timezone": ctx.timezone.name(),
            },
            "checkpoint": cp_value,
        });

        let resp = bridge
            .client
            .post_json(
                "/deliver",
                &payload,
                Some(&idempotency_key),
                BRIDGE_DELIVER_TIMEOUT,
            )
            .await;

        let (status, retry_after_hdr, body) = match resp {
            Ok(v) => v,
            Err(AppError::SsrfBlocked(msg)) => {
                return Ok(DeliveryOutcome::PermanentFailure {
                    status_code: None,
                    checkpoint: ctx.checkpoint.clone(),
                    diagnostic: msg,
                });
            }
            Err(err) => {
                return Ok(DeliveryOutcome::Retry {
                    retry_after_secs: None,
                    status_code: None,
                    checkpoint: ctx.checkpoint.clone(),
                    diagnostic: crate::logging::redact::redact_text(&err.to_string()),
                    cooldown_credential_group: false,
                });
            }
        };

        if status == 429 {
            let preview = crate::logging::redact::redact_text(&String::from_utf8_lossy(&body));
            return Ok(DeliveryOutcome::Retry {
                retry_after_secs: retry_after_hdr,
                status_code: Some(429),
                checkpoint: ctx.checkpoint.clone(),
                diagnostic: format!("Bridge rate limited (429): {preview}"),
                cooldown_credential_group: true,
            });
        }
        if status >= 500 {
            let preview = crate::logging::redact::redact_text(&String::from_utf8_lossy(&body));
            return Ok(DeliveryOutcome::Retry {
                retry_after_secs: retry_after_hdr,
                status_code: Some(i32::from(status)),
                checkpoint: ctx.checkpoint.clone(),
                diagnostic: format!("Bridge server error ({status}): {preview}"),
                cooldown_credential_group: false,
            });
        }
        if !(200..300).contains(&status) {
            let preview = crate::logging::redact::redact_text(&String::from_utf8_lossy(&body));
            return Ok(DeliveryOutcome::PermanentFailure {
                status_code: Some(i32::from(status)),
                checkpoint: ctx.checkpoint.clone(),
                diagnostic: format!("Bridge rejected delivery ({status}): {preview}"),
            });
        }

        let parsed: BridgeDeliverResponse = match serde_json::from_slice(&body) {
            Ok(p) => p,
            Err(err) => {
                return Ok(DeliveryOutcome::Retry {
                    retry_after_secs: None,
                    status_code: Some(i32::from(status)),
                    checkpoint: ctx.checkpoint.clone(),
                    diagnostic: format!("Invalid bridge /deliver JSON response: {err}"),
                    cooldown_credential_group: false,
                });
            }
        };

        let next_checkpoint = match parsed.checkpoint {
            Some(cp_val) if !cp_val.is_null() => {
                let encoded = serde_json::to_string(&cp_val).unwrap_or_default();
                if encoded.len() > BRIDGE_MAX_CHECKPOINT_BYTES {
                    return Ok(DeliveryOutcome::PermanentFailure {
                        status_code: Some(i32::from(status)),
                        checkpoint: ctx.checkpoint.clone(),
                        diagnostic: format!(
                            "Bridge returned checkpoint exceeding 16 KiB limit ({} bytes)",
                            encoded.len()
                        ),
                    });
                }
                Some(DeliveryCheckpoint::Plugin { state: cp_val })
            }
            _ => ctx.checkpoint.clone(),
        };

        let mut diag_parts = Vec::new();
        if let Some(d) = parsed
            .diagnostic
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            diag_parts.push(crate::logging::redact::redact_text(d));
        }
        if let Some(ext) = parsed
            .external_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            diag_parts.push(format!(
                "external_id={}",
                crate::logging::redact::redact_text(ext)
            ));
        }
        for w in &ctx.overrides.warnings {
            diag_parts.push(w.clone());
        }
        let combined_diag = if diag_parts.is_empty() {
            None
        } else {
            Some(diag_parts.join(" | "))
        };

        match parsed.outcome.trim().to_ascii_lowercase().as_str() {
            "delivered" => Ok(DeliveryOutcome::Delivered {
                status_code: Some(i32::from(status)),
                checkpoint: next_checkpoint,
                diagnostic: combined_diag,
                updated_capabilities: None,
            }),
            "retry" => Ok(DeliveryOutcome::Retry {
                retry_after_secs: parsed.retry_after_secs.or(retry_after_hdr),
                status_code: Some(i32::from(status)),
                checkpoint: next_checkpoint,
                diagnostic: combined_diag
                    .unwrap_or_else(|| "Bridge requested transient retry".to_string()),
                cooldown_credential_group: parsed.retry_after_secs.is_some(),
            }),
            "deferred" => {
                let next_at = parsed.next_window_at.unwrap_or_else(|| {
                    now_unix().saturating_add(
                        i64::try_from(parsed.retry_after_secs.unwrap_or(300)).unwrap_or(300),
                    )
                });
                Ok(DeliveryOutcome::Deferred {
                    next_window_at: next_at,
                    checkpoint: next_checkpoint,
                    diagnostic: combined_diag
                        .unwrap_or_else(|| "Bridge deferred delivery".to_string()),
                })
            }
            "permanent_failure" => Ok(DeliveryOutcome::PermanentFailure {
                status_code: Some(i32::from(status)),
                checkpoint: next_checkpoint,
                diagnostic: combined_diag
                    .unwrap_or_else(|| "Bridge reported permanent delivery failure".to_string()),
            }),
            other => Ok(DeliveryOutcome::Retry {
                retry_after_secs: None,
                status_code: Some(i32::from(status)),
                checkpoint: next_checkpoint,
                diagnostic: format!("Bridge returned unknown outcome '{other}'"),
                cooldown_credential_group: false,
            }),
        }
    }

    async fn send_test(
        &self,
        channel_config: &ChannelConfig,
        destination_config: &DestinationConfig,
        _use_proxy: bool,
    ) -> AppResult<String> {
        let ChannelConfig::Plugin(acct_cfg) = channel_config else {
            return Err(AppError::Validation(
                "BridgeChannelSender::send_test expected ChannelConfig::Plugin".to_string(),
            ));
        };
        let DestinationConfig::Plugin(dest_cfg) = destination_config else {
            return Err(AppError::Validation(
                "BridgeChannelSender::send_test expected DestinationConfig::Plugin".to_string(),
            ));
        };
        let bridge = self
            .registry
            .get_bridge(&acct_cfg.plugin_id)
            .ok_or_else(|| {
                AppError::NotFound(format!(
                    "Channel plugin '{}' is not installed",
                    acct_cfg.plugin_id
                ))
            })?;
        if bridge.manifest.kind != PluginKind::Channel {
            return Err(AppError::Validation(format!(
                "Plugin '{}' is a '{}' plugin, not a channel plugin",
                acct_cfg.plugin_id,
                bridge.manifest.kind.as_str()
            )));
        }
        if !bridge.compatible {
            return Err(AppError::Validation(format!(
                "Channel plugin '{}' is incompatible with current core: {}",
                acct_cfg.plugin_id,
                bridge
                    .compatibility_reason
                    .as_deref()
                    .unwrap_or("version mismatch")
            )));
        }
        if !bridge.enabled {
            return Err(AppError::Validation(format!(
                "Channel plugin '{}' is disabled",
                acct_cfg.plugin_id
            )));
        }
        bridge
            .client
            .test_remote(&json!({
                "scope": "destination",
                "account": acct_cfg.config,
                "destination": dest_cfg.config,
            }))
            .await
    }
}
