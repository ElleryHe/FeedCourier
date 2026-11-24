//! In-process pure-Rust JavaScript plugin sandbox (`boa_engine`) implementing FeedCourier
//! Plugin API v1 (`channel` + `processor` + `on_delivered`).
//!
//! Security & isolation guarantees:
//! - Every plugin is a single self-contained synchronous ES Module (`<= 512 KiB`).
//! - All `import` statements (static and dynamic) are unconditionally disabled via
//!   [`boa_engine::module::IdleModuleLoader`].
//! - Each execution runs in a fresh isolated `boa_engine::Context` inside
//!   `tokio::task::spawn_blocking` with hard [`boa_engine::vm::RuntimeLimits`]
//!   (`loop_iteration_limit = 200_000`, `recursion_limit = 128`, `stack_size_limit = 64 KiB`)
//!   and an outer wall-clock `tokio::time::timeout`.
//! - No filesystem, environment variables, timers, or raw sockets are exposed. The sole host
//!   capability object is `globalThis.fc`, whose `fc.fetch()` enforces a dual allowlist:
//!   1. Cloud metadata (`169.254.169.254`, `100.100.100.200`, `fd00:ec2::254`, `0.0.0.0`, `::`,
//!      `metadata.google.internal`) and HTTP 3xx redirects are ALWAYS blocked.
//!   2. Exact `(scheme, host, port)` origins extracted from admin-configured `format: "uri"`
//!      fields bypass private/LAN IP blocks (allowing self-hosted LAN/Docker servers out of the
//!      box while still honoring `use_proxy` and blocking metadata IPs).
//!   3. Plugin-declared `meta.allowed_hosts` are routed through [`ProtectedHttpClient`] with full
//!      DNS-rebinding SSRF protection and `use_proxy` fail-closed semantics.
//!   4. All other hosts fail immediately with [`AppError::SsrfBlocked`].
//! - HTML returned by `onRender` is always re-sanitized through `ammonia`.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use boa_engine::builtins::promise::PromiseState;
use boa_engine::module::IdleModuleLoader;
use boa_engine::object::ObjectInitializer;
use boa_engine::object::builtins::JsPromise;
use boa_engine::property::Attribute;
use boa_engine::{
    Context, JsArgs, JsError, JsNativeError, JsResult, JsValue, Module, NativeFunction, Source,
    js_string,
};
use hmac::{Hmac, KeyInit as HmacKeyInit, Mac};
use reqwest::Method;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use url::Url;

use crate::channels::{
    ChannelConfig, ChannelKind, ChannelSender, DeliveryCheckpoint, DeliveryContext,
    DeliveryOutcome, DestinationConfig, mask_secret,
};
use crate::crypto::{md5_hex, sha256_hex};
use crate::db::Database;
use crate::db::models::now_unix;
use crate::error::{AppError, AppResult};
use crate::ingest::html::{html_to_plain_text, sanitize_and_extract_html};
use crate::net::ssrf::check_hard_blocked_plugin_url;
use crate::net::{PluginHttpRequest, ProtectedHttpClient};
use crate::schedule::format_local_timestamp_compact;

use super::{
    HookKind, PLUGIN_API_VERSION, Plugin, PluginKind, PluginManifest, PluginPermission,
    PluginRegistry, PluginResult, PluginRuntime, RenderContext, RenderOverrides,
    check_plugin_compatibility, parse_semver_triple, sanitize_http_url,
};

type HmacSha256 = Hmac<Sha256>;

/// Maximum accepted UTF-8 byte length for a single `plugin.js` source file (`512 KiB`).
pub const JS_MAX_SCRIPT_BYTES: usize = 512 * 1024;
/// Hard Boa VM loop iteration budget per invocation.
pub const JS_LOOP_ITERATION_LIMIT: u64 = 200_000;
/// Hard Boa VM call recursion depth limit.
pub const JS_RECURSION_LIMIT: usize = 128;
/// Hard Boa VM value stack size limit.
pub const JS_STACK_SIZE_LIMIT: usize = 64 * 1024;
/// Maximum HTTP response payload bytes returned by `fc.fetch` (`1 MiB`).
pub const JS_MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// Maximum JSON byte size for a channel plugin delivery checkpoint (`16 KiB`).
pub const JS_MAX_CHECKPOINT_BYTES: usize = 16 * 1024;
/// Maximum `fc.fetch` calls allowed per `deliver` / `validate` / `test` / `onDelivered` invocation.
pub const JS_MAX_FETCH_CALLS_DEFAULT: usize = 4;
/// Maximum `fc.fetch` calls allowed per `onRender` invocation.
pub const JS_MAX_FETCH_CALLS_ON_RENDER: usize = 10;
/// Default `fc.fetch` timeout (`10s`) and hard per-request ceiling (`15s`).
pub const JS_FETCH_DEFAULT_TIMEOUT_MS: u64 = 10_000;
pub const JS_FETCH_MAX_TIMEOUT_MS: u64 = 15_000;

/// Outer wall-clock timeouts for sandbox invocations.
pub const JS_COMPILE_TIMEOUT: Duration = Duration::from_secs(5);
pub const JS_HOOK_DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
pub const JS_HOOK_ON_RENDER_TIMEOUT: Duration = Duration::from_secs(15);
pub const JS_HOOK_DELIVER_TIMEOUT: Duration = Duration::from_secs(30);

const SSRF_ERR_PREFIX: &str = "SSRF_BLOCKED: ";
const PROXY_ERR_PREFIX: &str = "PROXY_UNCONFIGURED: ";

static DEBUG_NONCE_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_debug_nonce() -> String {
    let seq = DEBUG_NONCE_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{}:{seq}", chrono::Utc::now().timestamp_millis())
}

// =============================================================================
// Plugin ID, Allowed Hosts & JSON Schema Helpers
// =============================================================================

/// Validate a plugin ID (`^[a-z0-9][a-z0-9_-]{1,63}$`).
pub fn validate_plugin_id(id: &str) -> AppResult<()> {
    let bytes = id.as_bytes();
    let ok = bytes.len() >= 2
        && bytes.len() <= 64
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
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

/// Normalized `(scheme, host, port)` extracted from an admin-configured `format: "uri"` field.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AllowedOrigin {
    pub scheme: String,
    pub host: String,
    pub port: u16,
}

impl AllowedOrigin {
    pub fn from_url(url: &Url) -> Option<Self> {
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        let host = url
            .host_str()?
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if host.is_empty() {
            return None;
        }
        let port = url.port_or_known_default()?;
        Some(Self {
            scheme: url.scheme().to_ascii_lowercase(),
            host,
            port,
        })
    }
}

/// Normalize and validate a single `meta.allowed_hosts` pattern (exact host or `*.domain`).
pub fn normalize_allowed_host(raw: &str) -> AppResult<String> {
    let trimmed = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if trimmed.is_empty() {
        return Err(AppError::Validation(
            "allowed_hosts entry must not be empty".to_string(),
        ));
    }
    if trimmed == "*"
        || trimmed.contains("://")
        || trimmed.contains('/')
        || trimmed.contains('@')
        || trimmed.contains(':')
        || trimmed.chars().any(char::is_whitespace)
    {
        return Err(AppError::Validation(format!(
            "invalid allowed_hosts entry '{raw}': expected a bare hostname or '*.domain' pattern"
        )));
    }
    if let Some(rest) = trimmed.strip_prefix("*.") {
        if rest.is_empty() || rest.contains('*') || !rest.contains('.') {
            return Err(AppError::Validation(format!(
                "invalid wildcard allowed_hosts entry '{raw}': expected '*.example.com'"
            )));
        }
    } else if trimmed.contains('*') {
        return Err(AppError::Validation(format!(
            "invalid wildcard allowed_hosts entry '{raw}'"
        )));
    }

    if matches!(
        trimmed.as_str(),
        "169.254.169.254"
            | "100.100.100.200"
            | "0.0.0.0"
            | "metadata.google.internal"
            | "metadata.goog"
            | "instance-data"
            | "instance-data.ec2.internal"
    ) {
        return Err(AppError::Validation(format!(
            "allowed_hosts must not include cloud metadata or unspecified host '{raw}'"
        )));
    }
    Ok(trimmed)
}

fn host_matches_allowed(host: &str, allowed_hosts: &[String]) -> bool {
    let norm = host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if norm.is_empty() {
        return false;
    }
    for pattern in allowed_hosts {
        if let Some(suffix) = pattern.strip_prefix("*.") {
            let dot_suffix = format!(".{suffix}");
            if norm.len() > dot_suffix.len() && norm.ends_with(&dot_suffix) {
                return true;
            }
        } else if norm == *pattern {
            return true;
        }
    }
    false
}

/// Extract all exact `(scheme, host, port)` origins from fields declared as `format: "uri"` in
/// `schema` whose values are present in `cfg` (or fall back to schema `default` when absent).
pub fn extract_uri_origins(schema: &Value, cfg: &Value) -> Vec<AllowedOrigin> {
    let mut out = Vec::new();
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return out;
    };
    let cfg_obj = cfg.as_object();
    for (key, prop) in props {
        if prop.get("format").and_then(Value::as_str) != Some("uri") {
            continue;
        }
        let raw_val = cfg_obj
            .and_then(|o| o.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                prop.get("default")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            });
        if let Some(raw_url) = raw_val
            && let Ok(parsed) = Url::parse(raw_url)
            && check_hard_blocked_plugin_url(&parsed).is_ok()
            && let Some(origin) = AllowedOrigin::from_url(&parsed)
            && !out.contains(&origin)
        {
            out.push(origin);
        }
    }
    out
}

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
                                check_hard_blocked_plugin_url(&parsed)?;
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
/// when omitted or empty, clearing optional fields when explicitly set to `null`.
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
// Boa Host State & `globalThis.fc` Bindings
// =============================================================================

#[derive(Clone)]
pub struct FcFetchPolicy {
    pub http: ProtectedHttpClient,
    pub rt_handle: tokio::runtime::Handle,
    pub allowed_hosts: Vec<String>,
    pub admin_uri_origins: Vec<AllowedOrigin>,
    pub use_proxy: bool,
    pub max_calls: usize,
}

struct FcHostState {
    fetch: Option<FcFetchPolicy>,
    fetch_calls: Cell<usize>,
    last_ssrf_error: RefCell<Option<String>>,
    last_proxy_error: RefCell<Option<String>>,
}

impl FcHostState {
    fn new(fetch: Option<FcFetchPolicy>) -> Self {
        Self {
            fetch,
            fetch_calls: Cell::new(0),
            last_ssrf_error: RefCell::new(None),
            last_proxy_error: RefCell::new(None),
        }
    }
}

fn js_err(msg: impl Into<String>) -> JsError {
    JsNativeError::error().with_message(msg.into()).into()
}

fn js_type_err(msg: impl Into<String>) -> JsError {
    JsNativeError::typ().with_message(msg.into()).into()
}

fn require_string_arg(
    args: &[JsValue],
    idx: usize,
    name: &str,
    ctx: &mut Context,
) -> JsResult<String> {
    let val = args.get_or_undefined(idx);
    if val.is_undefined() || val.is_null() {
        return Err(js_type_err(format!(
            "missing required string argument '{name}'"
        )));
    }
    Ok(val.to_string(ctx)?.to_std_string_escaped())
}

fn optional_string_arg(
    args: &[JsValue],
    idx: usize,
    ctx: &mut Context,
) -> JsResult<Option<String>> {
    let val = args.get_or_undefined(idx);
    if val.is_undefined() || val.is_null() {
        return Ok(None);
    }
    Ok(Some(val.to_string(ctx)?.to_std_string_escaped()))
}

fn encode_digest(bytes: &[u8], encoding: Option<&str>) -> JsResult<String> {
    match encoding
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("hex")
        .to_ascii_lowercase()
        .as_str()
    {
        "hex" => Ok(hex::encode(bytes)),
        "base64" => Ok(base64::engine::general_purpose::STANDARD.encode(bytes)),
        "base64url" => Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)),
        other => Err(js_type_err(format!(
            "unsupported digest encoding '{other}' (expected 'hex', 'base64', or 'base64url')"
        ))),
    }
}

fn fc_fetch(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let req_val = args.get_or_undefined(0);
    let req_json = req_val
        .to_json(ctx)?
        .ok_or_else(|| js_type_err("fc.fetch(req) requires a request object"))?;
    let req_obj = req_json
        .as_object()
        .ok_or_else(|| js_type_err("fc.fetch(req) argument must be an object"))?;

    let url_str = req_obj
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| js_type_err("fc.fetch requires a non-empty 'url' string"))?
        .to_string();

    let method_str = req_obj
        .get("method")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("GET")
        .to_ascii_uppercase();
    let method = match method_str.as_str() {
        "GET" => Method::GET,
        "POST" => Method::POST,
        "PUT" => Method::PUT,
        "PATCH" => Method::PATCH,
        "DELETE" => Method::DELETE,
        "HEAD" => Method::HEAD,
        other => {
            return Err(js_type_err(format!(
                "unsupported HTTP method '{other}' in fc.fetch"
            )));
        }
    };

    let mut headers = HeaderMap::new();
    if let Some(hdrs_val) = req_obj.get("headers")
        && !hdrs_val.is_null()
    {
        let hdrs_obj = hdrs_val
            .as_object()
            .ok_or_else(|| js_type_err("fc.fetch 'headers' must be an object"))?;
        if hdrs_obj.len() > 64 {
            return Err(js_type_err("fc.fetch supports at most 64 request headers"));
        }
        for (k, v) in hdrs_obj {
            let v_str = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Null => continue,
                _ => {
                    return Err(js_type_err(format!(
                        "fc.fetch header '{k}' must be a string, number, or boolean"
                    )));
                }
            };
            let h_name = HeaderName::from_bytes(k.trim().as_bytes())
                .map_err(|_| js_type_err(format!("invalid HTTP header name '{k}'")))?;
            let h_val = HeaderValue::from_str(&v_str)
                .map_err(|_| js_type_err(format!("invalid HTTP header value for '{k}'")))?;
            headers.insert(h_name, h_val);
        }
    }

    let body = match req_obj.get("body") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            if s.len() > JS_MAX_RESPONSE_BYTES {
                return Err(js_type_err("fc.fetch request body exceeds 1 MiB limit"));
            }
            Some(s.as_bytes().to_vec())
        }
        Some(_) => {
            return Err(js_type_err(
                "fc.fetch 'body' must be a string (use JSON.stringify for JSON payloads)",
            ));
        }
    };

    let timeout_ms = req_obj
        .get("timeoutMs")
        .and_then(Value::as_u64)
        .unwrap_or(JS_FETCH_DEFAULT_TIMEOUT_MS)
        .clamp(100, JS_FETCH_MAX_TIMEOUT_MS);

    let (policy, parsed_url, allow_private_origin) = {
        let state = ctx
            .get_data::<FcHostState>()
            .ok_or_else(|| js_err("fc.fetch host state is missing"))?;
        let Some(ref policy) = state.fetch else {
            return Err(js_err(
                "fc.fetch() is not available during module compilation",
            ));
        };

        let next_count = state.fetch_calls.get().saturating_add(1);
        if next_count > policy.max_calls {
            return Err(js_err(format!(
                "fc.fetch call budget exceeded (max {} calls per hook)",
                policy.max_calls
            )));
        }
        state.fetch_calls.set(next_count);

        let parsed = Url::parse(&url_str)
            .map_err(|err| js_type_err(format!("invalid fc.fetch URL '{url_str}': {err}")))?;
        if let Err(err) = check_hard_blocked_plugin_url(&parsed) {
            let msg = err.to_string();
            *state.last_ssrf_error.borrow_mut() = Some(msg.clone());
            return Err(js_err(format!("{SSRF_ERR_PREFIX}{msg}")));
        }

        let origin = AllowedOrigin::from_url(&parsed)
            .ok_or_else(|| js_type_err(format!("invalid fc.fetch URL origin '{url_str}'")))?;
        let allow_private = if policy.admin_uri_origins.contains(&origin) {
            true
        } else if host_matches_allowed(&origin.host, &policy.allowed_hosts) {
            false
        } else {
            let msg = format!(
                "Host '{}' is not permitted by plugin allowed_hosts or admin-configured URI fields",
                origin.host
            );
            *state.last_ssrf_error.borrow_mut() = Some(msg.clone());
            return Err(js_err(format!("{SSRF_ERR_PREFIX}{msg}")));
        };

        (policy.clone(), parsed, allow_private)
    };

    let http_req = PluginHttpRequest {
        method,
        url: parsed_url.into(),
        headers,
        body,
        timeout: Duration::from_millis(timeout_ms),
        max_bytes: JS_MAX_RESPONSE_BYTES,
        allow_private_origin,
        use_proxy: policy.use_proxy,
    };

    let res = policy
        .rt_handle
        .block_on(policy.http.send_plugin_http(http_req));

    match res {
        Ok(resp) => {
            let headers_val = Value::Object(
                resp.headers
                    .into_iter()
                    .map(|(k, v)| (k, Value::String(v)))
                    .collect(),
            );
            let out = json!({
                "status": resp.status,
                "ok": (200..300).contains(&resp.status),
                "headers": headers_val,
                "body": resp.body,
            });
            JsValue::from_json(&out, ctx)
        }
        Err(AppError::SsrfBlocked(msg)) => {
            if let Some(state) = ctx.get_data::<FcHostState>() {
                *state.last_ssrf_error.borrow_mut() = Some(msg.clone());
            }
            Err(js_err(format!("{SSRF_ERR_PREFIX}{msg}")))
        }
        Err(AppError::Validation(msg)) if msg.contains("OUTBOUND_PROXY") => {
            if let Some(state) = ctx.get_data::<FcHostState>() {
                *state.last_proxy_error.borrow_mut() = Some(msg.clone());
            }
            Err(js_err(format!("{PROXY_ERR_PREFIX}{msg}")))
        }
        Err(err) => Err(js_err(err.to_string())),
    }
}

fn fc_sha256(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let input = require_string_arg(args, 0, "input", ctx)?;
    let enc = optional_string_arg(args, 1, ctx)?;
    let digest = Sha256::digest(input.as_bytes());
    let encoded = encode_digest(&digest, enc.as_deref())?;
    Ok(JsValue::from(js_string!(encoded)))
}

fn fc_md5(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let input = require_string_arg(args, 0, "input", ctx)?;
    Ok(JsValue::from(js_string!(md5_hex(input.as_bytes()))))
}

fn fc_hmac_sha256(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let msg = require_string_arg(args, 0, "message", ctx)?;
    let secret = require_string_arg(args, 1, "secret", ctx)?;
    let enc = optional_string_arg(args, 2, ctx)?;
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(msg.as_bytes());
    let encoded = encode_digest(&mac.finalize().into_bytes(), enc.as_deref())?;
    Ok(JsValue::from(js_string!(encoded)))
}

fn fc_base64_encode(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let input = require_string_arg(args, 0, "input", ctx)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(input.as_bytes());
    Ok(JsValue::from(js_string!(encoded)))
}

fn fc_base64_decode(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let input = require_string_arg(args, 0, "input", ctx)?;
    let trimmed = input.trim();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(trimmed)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(trimmed))
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(trimmed))
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(trimmed))
        .map_err(|err| js_err(format!("invalid base64 input: {err}")))?;
    let text = String::from_utf8(bytes)
        .map_err(|err| js_err(format!("base64 payload is not valid UTF-8: {err}")))?;
    Ok(JsValue::from(js_string!(text)))
}

fn fc_base64_url_encode(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let input = require_string_arg(args, 0, "input", ctx)?;
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input.as_bytes());
    Ok(JsValue::from(js_string!(encoded)))
}

fn fc_utf8_byte_length(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let input = require_string_arg(args, 0, "input", ctx)?;
    Ok(JsValue::from(input.len() as f64))
}

/// Truncate `text` to at most `max_bytes` UTF-8 bytes without ever splitting a Unicode scalar value.
pub fn truncate_utf8_bytes_str(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    if max_bytes == 0 {
        return "";
    }
    let mut end = 0usize;
    for (idx, ch) in text.char_indices() {
        let next = idx + ch.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    &text[..end]
}

fn fc_truncate_utf8_bytes(
    _this: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let input = require_string_arg(args, 0, "input", ctx)?;
    let max_val = args.get_or_undefined(1);
    let max_f = max_val.to_number(ctx)?;
    if !max_f.is_finite() || max_f < 0.0 {
        return Err(js_type_err(
            "fc.truncateUtf8Bytes 'maxBytes' must be a non-negative finite number",
        ));
    }
    let max_bytes = (max_f.floor() as u64).min(usize::MAX as u64) as usize;
    let truncated = truncate_utf8_bytes_str(&input, max_bytes);
    Ok(JsValue::from(js_string!(truncated)))
}

fn fc_html_to_text(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let html = require_string_arg(args, 0, "html", ctx)?;
    Ok(JsValue::from(js_string!(html_to_plain_text(&html))))
}

fn fc_unix_now(_this: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(now_unix() as f64))
}

fn fc_uuid(_this: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let mut b = rand::random::<[u8; 16]>();
    b[6] = (b[6] & 0x0f) | 0x40; // RFC 4122 version 4
    b[8] = (b[8] & 0x3f) | 0x80; // RFC 4122 variant 1
    let uuid = format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0],
        b[1],
        b[2],
        b[3],
        b[4],
        b[5],
        b[6],
        b[7],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    );
    Ok(JsValue::from(js_string!(uuid)))
}

fn build_sandbox_context(fetch: Option<FcFetchPolicy>) -> AppResult<Context> {
    let mut ctx = Context::builder()
        .module_loader(Rc::new(IdleModuleLoader))
        .build()
        .map_err(|err| AppError::Internal(anyhow::anyhow!("failed to init JS sandbox: {err}")))?;

    {
        let limits = ctx.runtime_limits_mut();
        limits.set_loop_iteration_limit(JS_LOOP_ITERATION_LIMIT);
        limits.set_recursion_limit(JS_RECURSION_LIMIT);
        limits.set_stack_size_limit(JS_STACK_SIZE_LIMIT);
    }

    ctx.insert_data(FcHostState::new(fetch));

    let fc_obj = ObjectInitializer::new(&mut ctx)
        .function(
            NativeFunction::from_fn_ptr(fc_fetch),
            js_string!("fetch"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(fc_sha256),
            js_string!("sha256"),
            2,
        )
        .function(NativeFunction::from_fn_ptr(fc_md5), js_string!("md5"), 1)
        .function(
            NativeFunction::from_fn_ptr(fc_hmac_sha256),
            js_string!("hmacSha256"),
            3,
        )
        .function(
            NativeFunction::from_fn_ptr(fc_base64_encode),
            js_string!("base64Encode"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(fc_base64_decode),
            js_string!("base64Decode"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(fc_base64_url_encode),
            js_string!("base64UrlEncode"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(fc_utf8_byte_length),
            js_string!("utf8ByteLength"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(fc_truncate_utf8_bytes),
            js_string!("truncateUtf8Bytes"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(fc_html_to_text),
            js_string!("htmlToText"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(fc_unix_now),
            js_string!("unixNow"),
            0,
        )
        .function(NativeFunction::from_fn_ptr(fc_uuid), js_string!("uuid"), 0)
        .build();

    ctx.register_global_property(js_string!("fc"), fc_obj, Attribute::READONLY)
        .map_err(|err| {
            AppError::Internal(anyhow::anyhow!("failed to register global fc: {err}"))
        })?;

    Ok(ctx)
}

fn format_js_error(err: &JsError, ctx: &mut Context) -> String {
    let raw = err
        .try_native(ctx)
        .map(|n| n.message().to_string())
        .unwrap_or_else(|_| err.to_string());
    crate::logging::redact::redact_text(&raw)
}

fn evaluate_plugin_module(script_js: &str, ctx: &mut Context) -> AppResult<Module> {
    let module =
        Module::parse(Source::from_bytes(script_js.as_bytes()), None, ctx).map_err(|err| {
            AppError::Validation(format!(
                "JS plugin syntax error: {}",
                format_js_error(&err, ctx)
            ))
        })?;

    let promise = module.load_link_evaluate(ctx);
    ctx.run_jobs().map_err(|err| {
        AppError::Validation(format!(
            "JS plugin module evaluation error: {}",
            format_js_error(&err, ctx)
        ))
    })?;

    match promise.state() {
        PromiseState::Fulfilled(_) => Ok(module),
        PromiseState::Rejected(reason) => {
            let err = JsError::from_opaque(reason);
            Err(AppError::Validation(format!(
                "JS plugin module evaluation failed: {}",
                format_js_error(&err, ctx)
            )))
        }
        PromiseState::Pending => Err(AppError::Validation(
            "JS plugin module must evaluate synchronously (top-level await / unsettled Promises are not supported)".to_string(),
        )),
    }
}

fn map_hook_js_error(
    plugin_id: &str,
    export_name: &str,
    err: JsError,
    ctx: &mut Context,
) -> AppError {
    let msg = format_js_error(&err, ctx);
    if let Some(rest) = msg.strip_prefix(SSRF_ERR_PREFIX) {
        return AppError::SsrfBlocked(rest.to_string());
    }
    if let Some(rest) = msg.strip_prefix(PROXY_ERR_PREFIX) {
        return AppError::Validation(rest.to_string());
    }
    if let Some(state) = ctx.get_data::<FcHostState>() {
        if let Some(ssrf_msg) = state.last_ssrf_error.borrow().as_ref()
            && msg.contains(ssrf_msg)
        {
            return AppError::SsrfBlocked(ssrf_msg.clone());
        }
        if let Some(proxy_msg) = state.last_proxy_error.borrow().as_ref()
            && msg.contains(proxy_msg)
        {
            return AppError::Validation(proxy_msg.clone());
        }
    }
    AppError::Upstream(format!(
        "JS plugin '{plugin_id}' {export_name}() failed: {msg}"
    ))
}

// =============================================================================
// Module Compilation & Manifest Extraction
// =============================================================================

/// Result of compiling and validating a `plugin.js` source file in the Boa sandbox.
#[derive(Debug, Clone)]
pub struct CompiledJsPlugin {
    pub manifest: PluginManifest,
    pub script_sha256: String,
    pub has_validate: bool,
    pub has_test: bool,
    pub has_deliver: bool,
    pub has_on_render: bool,
    pub has_on_delivered: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawMetaDescription {
    Plain(String),
    I18n(BTreeMap<String, String>),
}

#[derive(Debug, Deserialize)]
struct RawJsMeta {
    id: String,
    name: String,
    version: String,
    #[serde(default = "default_api_v1")]
    api_version: String,
    #[serde(default)]
    kind: PluginKind,
    #[serde(default)]
    min_core: Option<String>,
    #[serde(default)]
    category: String,
    #[serde(default)]
    icon: String,
    #[serde(default)]
    description: Option<RawMetaDescription>,
    #[serde(default)]
    description_i18n: BTreeMap<String, String>,
    #[serde(default)]
    homepage: String,
    #[serde(default)]
    allowed_hosts: Vec<String>,
    #[serde(default)]
    hooks: Vec<HookKind>,
    #[serde(default)]
    permissions: Vec<PluginPermission>,
    #[serde(default = "default_empty_obj")]
    instance_schema: Value,
    #[serde(default = "default_empty_obj")]
    route_option_schema: Value,
    #[serde(default = "default_empty_obj")]
    account_schema: Value,
    #[serde(default = "default_empty_obj")]
    destination_schema: Value,
    #[serde(default = "default_empty_obj")]
    capabilities: Value,
}

fn default_api_v1() -> String {
    PLUGIN_API_VERSION.to_string()
}

fn default_empty_obj() -> Value {
    json!({})
}

fn is_callable_export(module: &Module, name: &str, ctx: &mut Context) -> bool {
    match module.get_value(js_string!(name), ctx) {
        Ok(val) => val.is_callable(),
        Err(_) => false,
    }
}

/// Synchronously compile a `plugin.js` script in an isolated Boa sandbox (no network access),
/// validate `export const meta` + exported hook functions, and compute `script_sha256`.
pub fn compile_js_plugin_sync(script_js: &str) -> AppResult<CompiledJsPlugin> {
    if script_js.trim().is_empty() {
        return Err(AppError::Validation(
            "JS plugin source must not be empty".to_string(),
        ));
    }
    if script_js.len() > JS_MAX_SCRIPT_BYTES {
        return Err(AppError::Validation(format!(
            "JS plugin source ({} bytes) exceeds 512 KiB maximum size",
            script_js.len()
        )));
    }

    let script_sha256 = sha256_hex(script_js.as_bytes());
    let mut ctx = build_sandbox_context(None)?;
    let module = evaluate_plugin_module(script_js, &mut ctx)?;

    let meta_val = module
        .get_value(js_string!("meta"), &mut ctx)
        .map_err(|err| {
            AppError::Validation(format!(
                "JS plugin failed reading export 'meta': {}",
                format_js_error(&err, &mut ctx)
            ))
        })?;
    if meta_val.is_undefined() || meta_val.is_null() {
        return Err(AppError::Validation(
            "JS plugin must export a 'meta' object (`export const meta = { ... }`)".to_string(),
        ));
    }
    let meta_json = meta_val
        .to_json(&mut ctx)
        .map_err(|err| {
            AppError::Validation(format!(
                "JS plugin 'meta' export must be JSON-serializable: {}",
                format_js_error(&err, &mut ctx)
            ))
        })?
        .ok_or_else(|| {
            AppError::Validation("JS plugin 'meta' export must be a JSON object".to_string())
        })?;

    let raw: RawJsMeta = serde_json::from_value(meta_json)
        .map_err(|err| AppError::Validation(format!("invalid JS plugin 'meta' object: {err}")))?;

    let id = raw.id.trim().to_string();
    validate_plugin_id(&id)?;
    let name = raw.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Validation(
            "JS plugin meta.name must not be empty".to_string(),
        ));
    }
    let version = raw.version.trim().to_string();
    if parse_semver_triple(&version).is_none() {
        return Err(AppError::Validation(format!(
            "JS plugin meta.version '{version}' must be a valid SemVer string (e.g. '0.3.1')"
        )));
    }
    if raw.api_version.trim() != PLUGIN_API_VERSION {
        return Err(AppError::Validation(format!(
            "unsupported JS plugin api_version '{}' (expected '{PLUGIN_API_VERSION}')",
            raw.api_version
        )));
    }

    let mut allowed_hosts = Vec::with_capacity(raw.allowed_hosts.len());
    let mut seen_hosts = HashSet::new();
    for host in &raw.allowed_hosts {
        let norm = normalize_allowed_host(host)?;
        if seen_hosts.insert(norm.clone()) {
            allowed_hosts.push(norm);
        }
    }

    let mut description_i18n = raw.description_i18n;
    let description = match raw.description {
        Some(RawMetaDescription::Plain(s)) => s.trim().to_string(),
        Some(RawMetaDescription::I18n(map)) => {
            for (k, v) in map {
                let k_trim = k.trim().to_string();
                let v_trim = v.trim().to_string();
                if !k_trim.is_empty() && !v_trim.is_empty() {
                    description_i18n.insert(k_trim, v_trim);
                }
            }
            description_i18n
                .get("en")
                .or_else(|| description_i18n.values().next())
                .cloned()
                .unwrap_or_default()
        }
        None => description_i18n
            .get("en")
            .or_else(|| description_i18n.values().next())
            .cloned()
            .unwrap_or_default(),
    };

    let homepage = sanitize_http_url(Some(raw.homepage)).unwrap_or_default();

    let mut permissions = raw.permissions;
    if !allowed_hosts.is_empty() && !permissions.contains(&PluginPermission::Network) {
        permissions.push(PluginPermission::Network);
    }

    let has_validate = is_callable_export(&module, "validate", &mut ctx);
    let has_test = is_callable_export(&module, "test", &mut ctx);
    let has_deliver = is_callable_export(&module, "deliver", &mut ctx);
    let has_on_render = is_callable_export(&module, "onRender", &mut ctx);
    let has_on_delivered = is_callable_export(&module, "onDelivered", &mut ctx);

    match raw.kind {
        PluginKind::Channel => {
            if !has_deliver {
                return Err(AppError::Validation(
                    "channel JS plugin must export a callable `deliver(ctx)` function".to_string(),
                ));
            }
        }
        PluginKind::Processor => {
            if raw.hooks.is_empty() {
                return Err(AppError::Validation(
                    "processor JS plugin must declare at least one hook (e.g. 'on_render' or 'on_delivered')"
                        .to_string(),
                ));
            }
        }
    }
    if raw.hooks.contains(&HookKind::OnRender) && !has_on_render {
        return Err(AppError::Validation(
            "JS plugin declares hook 'on_render' but does not export a callable `onRender(ctx)` function"
                .to_string(),
        ));
    }
    if raw.hooks.contains(&HookKind::OnDelivered) && !has_on_delivered {
        return Err(AppError::Validation(
            "JS plugin declares hook 'on_delivered' but does not export a callable `onDelivered(ctx)` function"
                .to_string(),
        ));
    }

    for (label, schema_val) in [
        ("instance_schema", &raw.instance_schema),
        ("route_option_schema", &raw.route_option_schema),
        ("account_schema", &raw.account_schema),
        ("destination_schema", &raw.destination_schema),
        ("capabilities", &raw.capabilities),
    ] {
        if !schema_val.is_object() {
            return Err(AppError::Validation(format!(
                "JS plugin meta.{label} must be a JSON object"
            )));
        }
    }

    let manifest = PluginManifest {
        id,
        name,
        version,
        api_version: PLUGIN_API_VERSION.to_string(),
        kind: raw.kind,
        runtime: PluginRuntime::Js,
        min_core: raw
            .min_core
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        category: raw.category.trim().to_string(),
        icon: raw.icon.trim().to_string(),
        description,
        description_i18n,
        homepage,
        allowed_hosts,
        hooks: raw.hooks,
        permissions,
        instance_schema: raw.instance_schema,
        route_option_schema: raw.route_option_schema,
        account_schema: raw.account_schema,
        destination_schema: raw.destination_schema,
        capabilities: raw.capabilities,
    };

    Ok(CompiledJsPlugin {
        manifest,
        script_sha256,
        has_validate,
        has_test,
        has_deliver,
        has_on_render,
        has_on_delivered,
    })
}

/// Async wrapper running [`compile_js_plugin_sync`] inside `spawn_blocking` with a 5s timeout.
pub async fn compile_js_plugin(script_js: String) -> AppResult<CompiledJsPlugin> {
    let handle = tokio::task::spawn_blocking(move || compile_js_plugin_sync(&script_js));
    match tokio::time::timeout(JS_COMPILE_TIMEOUT, handle).await {
        Ok(Ok(res)) => res,
        Ok(Err(join_err)) => Err(AppError::Internal(anyhow::anyhow!(
            "JS plugin compiler task panicked: {join_err}"
        ))),
        Err(_) => Err(AppError::Validation(format!(
            "JS plugin compilation timed out after {}s",
            JS_COMPILE_TIMEOUT.as_secs()
        ))),
    }
}

// =============================================================================
// Runtime Execution Handle (`JsPluginRuntime`)
// =============================================================================

/// Compiled in-memory representation of an installed JS plugin (`market` or `custom`).
#[derive(Debug, Clone)]
pub struct JsPluginRuntime {
    pub manifest: PluginManifest,
    pub script_js: Arc<str>,
    pub script_sha256: String,
    pub has_validate: bool,
    pub has_test: bool,
    pub has_deliver: bool,
    pub has_on_render: bool,
    pub has_on_delivered: bool,
}

impl JsPluginRuntime {
    pub fn from_compiled(script_js: String, compiled: CompiledJsPlugin) -> Self {
        Self {
            manifest: compiled.manifest,
            script_js: Arc::from(script_js),
            script_sha256: compiled.script_sha256,
            has_validate: compiled.has_validate,
            has_test: compiled.has_test,
            has_deliver: compiled.has_deliver,
            has_on_render: compiled.has_on_render,
            has_on_delivered: compiled.has_on_delivered,
        }
    }

    async fn invoke_export(
        &self,
        export_name: &'static str,
        arg: Value,
        fetch_policy: Option<FcFetchPolicy>,
        timeout: Duration,
    ) -> AppResult<Option<Value>> {
        let plugin_id = self.manifest.id.clone();
        let script = Arc::clone(&self.script_js);
        let task = tokio::task::spawn_blocking(move || {
            let mut ctx = build_sandbox_context(fetch_policy)?;
            let module = evaluate_plugin_module(&script, &mut ctx)?;
            let func_val = module
                .get_value(js_string!(export_name), &mut ctx)
                .map_err(|err| map_hook_js_error(&plugin_id, export_name, err, &mut ctx))?;
            if func_val.is_undefined() || func_val.is_null() {
                return Ok(None);
            }
            let Some(func_obj) = func_val.as_callable() else {
                return Err(AppError::Validation(format!(
                    "JS plugin '{plugin_id}' export '{export_name}' is not a function"
                )));
            };
            let js_arg = JsValue::from_json(&arg, &mut ctx)
                .map_err(|err| map_hook_js_error(&plugin_id, export_name, err, &mut ctx))?;
            let call_res = func_obj
                .call(&JsValue::undefined(), &[js_arg], &mut ctx)
                .map_err(|err| map_hook_js_error(&plugin_id, export_name, err, &mut ctx))?;

            if call_res
                .as_object()
                .is_some_and(|obj| JsPromise::from_object(obj.clone()).is_ok())
            {
                return Err(AppError::Validation(format!(
                    "JS plugin '{plugin_id}' {export_name}() returned a Promise; Plugin API v1 hooks must be synchronous"
                )));
            }

            if call_res.is_undefined() || call_res.is_null() {
                return Ok(Some(Value::Null));
            }
            let json_res = call_res
                .to_json(&mut ctx)
                .map_err(|err| map_hook_js_error(&plugin_id, export_name, err, &mut ctx))?
                .unwrap_or(Value::Null);
            Ok(Some(json_res))
        });

        match tokio::time::timeout(timeout, task).await {
            Ok(Ok(res)) => res,
            Ok(Err(join_err)) => Err(AppError::Internal(anyhow::anyhow!(
                "JS plugin '{}' {export_name}() task panicked: {join_err}",
                self.manifest.id
            ))),
            Err(_) => Err(AppError::Upstream(format!(
                "JS plugin '{}' {export_name}() timed out after {}s",
                self.manifest.id,
                timeout.as_secs()
            ))),
        }
    }

    fn build_fetch_policy(
        &self,
        http: &ProtectedHttpClient,
        admin_uri_origins: Vec<AllowedOrigin>,
        use_proxy: bool,
        max_calls: usize,
    ) -> FcFetchPolicy {
        FcFetchPolicy {
            http: http.clone(),
            rt_handle: tokio::runtime::Handle::current(),
            allowed_hosts: self.manifest.allowed_hosts.clone(),
            admin_uri_origins,
            use_proxy,
            max_calls,
        }
    }

    /// Validate a config (`account`, `destination`, or `instance`) against declarative JSON Schema
    /// and optionally run the plugin's exported `validate(ctx)` hook.
    pub async fn validate_config(
        &self,
        http: &ProtectedHttpClient,
        scope: &str,
        config: &Value,
        use_proxy: bool,
    ) -> AppResult<Value> {
        let schema = match scope {
            "account" => &self.manifest.account_schema,
            "destination" => &self.manifest.destination_schema,
            "instance" => &self.manifest.instance_schema,
            _ => &self.manifest.instance_schema,
        };
        let validated = validate_config_against_schema(schema, config)?;
        if !self.has_validate {
            return Ok(validated);
        }

        let origins = extract_uri_origins(schema, &validated);
        let policy = self.build_fetch_policy(http, origins, use_proxy, JS_MAX_FETCH_CALLS_DEFAULT);
        let payload = json!({
            "scope": scope,
            "config": validated,
        });
        let Some(res) = self
            .invoke_export("validate", payload, Some(policy), JS_HOOK_DEFAULT_TIMEOUT)
            .await?
        else {
            return Ok(validated);
        };
        if res.is_null() {
            return Ok(validated);
        }
        if res.get("ok").and_then(Value::as_bool) == Some(false) {
            let msg = res
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("configuration rejected by plugin");
            return Err(AppError::Validation(crate::logging::redact::redact_text(
                msg,
            )));
        }
        if let Some(norm) = res.get("normalized_config").filter(|v| v.is_object()) {
            return validate_config_against_schema(schema, norm);
        }
        Ok(validated)
    }

    /// Run the plugin's interactive `test(ctx)` connectivity check (`account`, `destination`, or `instance`).
    #[allow(clippy::too_many_arguments)]
    pub async fn test_config(
        &self,
        http: &ProtectedHttpClient,
        scope: &str,
        account: Option<&Value>,
        destination: Option<&Value>,
        instance: Option<&Value>,
        use_proxy: bool,
        locale: &str,
    ) -> AppResult<String> {
        if !self.has_test {
            return Ok(format!(
                "Plugin '{}' configuration validated",
                self.manifest.name
            ));
        }

        let mut origins = Vec::new();
        if let Some(acct) = account {
            origins.extend(extract_uri_origins(&self.manifest.account_schema, acct));
        }
        if let Some(dest) = destination {
            origins.extend(extract_uri_origins(&self.manifest.destination_schema, dest));
        }
        if let Some(inst) = instance {
            origins.extend(extract_uri_origins(&self.manifest.instance_schema, inst));
        }

        let policy = self.build_fetch_policy(http, origins, use_proxy, JS_MAX_FETCH_CALLS_DEFAULT);
        let mut payload = Map::new();
        payload.insert("scope".to_string(), Value::String(scope.to_string()));
        payload.insert("locale".to_string(), Value::String(locale.to_string()));
        if let Some(acct) = account {
            payload.insert("account".to_string(), acct.clone());
        }
        if let Some(dest) = destination {
            payload.insert("destination".to_string(), dest.clone());
        }
        if let Some(inst) = instance {
            payload.insert("instance".to_string(), inst.clone());
            payload.insert("config".to_string(), inst.clone());
        }

        let Some(res) = self
            .invoke_export(
                "test",
                Value::Object(payload),
                Some(policy),
                JS_HOOK_DEFAULT_TIMEOUT,
            )
            .await?
        else {
            return Ok(format!(
                "Plugin '{}' connectivity verified",
                self.manifest.name
            ));
        };

        if res.get("ok").and_then(Value::as_bool) == Some(false) {
            let msg = res
                .get("error")
                .or_else(|| res.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("plugin reported test failure");
            let clean = crate::logging::redact::redact_text(msg);
            if let Some((_, rest)) = clean.split_once(SSRF_ERR_PREFIX) {
                return Err(AppError::SsrfBlocked(rest.to_string()));
            }
            if let Some((_, rest)) = clean.split_once(PROXY_ERR_PREFIX) {
                return Err(AppError::Validation(rest.to_string()));
            }
            return Err(AppError::Upstream(clean));
        }
        Ok(res
            .get("message")
            .and_then(Value::as_str)
            .map(crate::logging::redact::redact_text)
            .unwrap_or_else(|| format!("Plugin '{}' connectivity verified", self.manifest.name)))
    }

    /// Invoke `onDelivered(ctx)` for a queued `plugin_events` row.
    pub async fn dispatch_on_delivered(
        &self,
        http: &ProtectedHttpClient,
        idempotency_key: &str,
        payload: &Value,
        admin_uri_origins: Vec<AllowedOrigin>,
        use_proxy: bool,
    ) -> AppResult<()> {
        if !self.has_on_delivered {
            return Ok(());
        }
        let policy = self.build_fetch_policy(
            http,
            admin_uri_origins,
            use_proxy,
            JS_MAX_FETCH_CALLS_DEFAULT,
        );
        let arg = json!({
            "event": "on_delivered",
            "idempotency_key": idempotency_key,
            "payload": payload,
        });
        let _ = self
            .invoke_export("onDelivered", arg, Some(policy), JS_HOOK_DEFAULT_TIMEOUT)
            .await?;
        Ok(())
    }
}

// =============================================================================
// JS Processor Plugin Adapter (`Plugin` trait)
// =============================================================================

#[derive(Debug, Deserialize)]
struct JsOnRenderResponse {
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

/// In-process JS processor plugin implementing FeedCourier's [`Plugin`] trait.
pub struct JsProcessorPlugin {
    runtime: Arc<JsPluginRuntime>,
}

impl JsProcessorPlugin {
    pub fn new(runtime: Arc<JsPluginRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl Plugin for JsProcessorPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.runtime.manifest
    }

    fn validate_instance_config(&self, cfg: &Value) -> AppResult<Value> {
        validate_config_against_schema(&self.runtime.manifest.instance_schema, cfg)
    }

    fn merge_instance_config(&self, existing: &Value, patch: &Value) -> AppResult<Value> {
        merge_config_against_schema(&self.runtime.manifest.instance_schema, existing, patch)
    }

    fn redact_instance_config(&self, cfg: &Value) -> Value {
        redact_config_with_schema(&self.runtime.manifest.instance_schema, cfg)
    }

    async fn validate_route_option(&self, db: &Database, option: &Value) -> AppResult<()> {
        if option.is_null() {
            return Ok(());
        }
        let obj = option.as_object().ok_or_else(|| {
            AppError::Validation(format!(
                "plugin '{}' route option must be a JSON object",
                self.runtime.manifest.id
            ))
        })?;
        if let Some(enabled_val) = obj.get("enabled")
            && !enabled_val.is_null()
            && !enabled_val.is_boolean()
        {
            return Err(AppError::Validation(format!(
                "plugin '{}' route option 'enabled' must be a boolean",
                self.runtime.manifest.id
            )));
        }
        if let Some(strict_val) = obj.get("strict_mode")
            && !strict_val.is_null()
            && !strict_val.is_boolean()
        {
            return Err(AppError::Validation(format!(
                "plugin '{}' route option 'strict_mode' must be a boolean",
                self.runtime.manifest.id
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
                    self.runtime.manifest.id
                ))
            })?;
            if inst.plugin_id != self.runtime.manifest.id {
                return Err(AppError::Validation(format!(
                    "plugin instance {pid} does not belong to plugin '{}'",
                    self.runtime.manifest.id
                )));
            }
        }
        if obj.get("enabled").and_then(Value::as_bool) != Some(false) {
            validate_config_against_schema(&self.runtime.manifest.route_option_schema, option)?;
        }
        Ok(())
    }

    async fn test_instance(&self, http: &ProtectedHttpClient, cfg: &Value) -> AppResult<String> {
        let use_proxy = cfg
            .get("use_proxy")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let normalized = self
            .runtime
            .validate_config(http, "instance", cfg, use_proxy)
            .await?;
        self.runtime
            .test_config(
                http,
                "instance",
                None,
                None,
                Some(&normalized),
                use_proxy,
                "en",
            )
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
            && inst.plugin_id == self.runtime.manifest.id
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
        if !self.runtime.manifest.hooks.contains(&HookKind::OnRender) || !self.runtime.has_on_render
        {
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
        let mut use_proxy = false;

        if let Some(pid) = route_opt.get("provider_id").and_then(Value::as_i64) {
            match ctx.db.get_plugin_instance(pid).await? {
                Some(inst) if inst.plugin_id == self.runtime.manifest.id && inst.enabled => {
                    let decrypted: Value = ctx.master_key.decrypt_json(&inst.encrypted_config)?;
                    if decrypted.get("strict_mode").and_then(Value::as_bool) == Some(true) {
                        strict_mode = true;
                    }
                    if decrypted.get("use_proxy").and_then(Value::as_bool) == Some(true) {
                        use_proxy = true;
                    }
                    instance_cfg = decrypted;
                }
                Some(inst) if !inst.enabled => {
                    out.warnings.push(format!(
                        "Plugin '{}' provider '{}' is disabled; kept original content",
                        self.runtime.manifest.id, inst.name
                    ));
                    return Ok(PluginResult::Continue);
                }
                _ => {
                    out.warnings.push(format!(
                        "Plugin '{}' provider #{pid} not found; kept original content",
                        self.runtime.manifest.id
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
                self.runtime.manifest.id,
                ctx.route.id,
                ctx.entry.fingerprint_sha256,
                next_debug_nonce()
            )
        } else {
            format!(
                "on_render:{}:{}:{}",
                self.runtime.manifest.id, ctx.route.id, ctx.entry.fingerprint_sha256
            )
        };
        let idempotency_key = sha256_hex(idempotency_input.as_bytes());

        let payload = json!({
            "idempotency_key": idempotency_key,
            "plugin_id": self.runtime.manifest.id,
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
                "site_url": ctx.feed.site_url,
                "favicon_url": ctx.feed.favicon_url,
            },
            "route": {
                "id": ctx.route.id,
                "name": ctx.route.name,
                "locale": ctx.route_options.locale.map(|l| l.as_str()),
                "timezone": ctx.route_options.timezone,
            },
            "write_cache": ctx.write_cache,
        });

        let origins = extract_uri_origins(&self.runtime.manifest.instance_schema, &instance_cfg);
        let policy = self.runtime.build_fetch_policy(
            ctx.http,
            origins,
            use_proxy,
            JS_MAX_FETCH_CALLS_ON_RENDER,
        );

        let raw_val = match self
            .runtime
            .invoke_export("onRender", payload, Some(policy), JS_HOOK_ON_RENDER_TIMEOUT)
            .await
        {
            Ok(Some(v)) if !v.is_null() => v,
            Ok(_) => return Ok(PluginResult::Continue),
            Err(err) => {
                let msg = format!(
                    "Plugin '{}' onRender error: {}",
                    self.runtime.manifest.id,
                    crate::logging::redact::redact_text(&err.to_string())
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

        let parsed: JsOnRenderResponse = match serde_json::from_value(raw_val) {
            Ok(p) => p,
            Err(err) => {
                let msg = format!(
                    "Plugin '{}' returned invalid object from onRender(): {err}",
                    self.runtime.manifest.id
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
                .map(|d| crate::logging::redact::redact_text(&d))
                .unwrap_or_else(|| {
                    format!("Plugin '{}' requested retry", self.runtime.manifest.id)
                });
            if strict_mode {
                return Ok(PluginResult::Retry {
                    diagnostic: diag,
                    retry_after_secs: parsed.retry_after_secs,
                });
            }
            out.warnings.push(diag);
            return Ok(PluginResult::Continue);
        }

        // Security invariant: HTML returned from a JS plugin MUST pass through ammonia!
        if let Some(new_html) = parsed.content_html {
            let base_url = primary_link.and_then(|u| Url::parse(u).ok());
            let sanitized = sanitize_and_extract_html(&new_html, base_url.as_ref());
            out.content_html = Some(sanitized.content_html);
        }

        if let Some(new_link) = parsed.primary_link {
            if let Some(safe) = sanitize_http_url(Some(new_link)) {
                out.primary_link = Some(safe);
            } else {
                out.warnings.push(format!(
                    "Plugin '{}' returned non-http(s) primary_link; ignored",
                    self.runtime.manifest.id
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
// JS Channel Plugin Adapter (`ChannelSender` trait for `ChannelKind::Plugin`)
// =============================================================================

#[derive(Debug, Deserialize)]
struct JsDeliverResponse {
    outcome: String,
    #[serde(default)]
    status_code: Option<i32>,
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

/// In-process JS channel delivery adapter implementing [`ChannelSender`] for `ChannelKind::Plugin`.
#[derive(Clone)]
pub struct JsChannelSender {
    registry: PluginRegistry,
    http: ProtectedHttpClient,
}

impl JsChannelSender {
    pub fn new(registry: PluginRegistry, http: ProtectedHttpClient) -> Self {
        Self { registry, http }
    }

    pub async fn send_test_with_locale(
        &self,
        channel_config: &ChannelConfig,
        destination_config: &DestinationConfig,
        use_proxy: bool,
        locale: &str,
    ) -> AppResult<String> {
        let ChannelConfig::Plugin(acct_cfg) = channel_config else {
            return Err(AppError::Validation(
                "JsChannelSender::send_test expected ChannelConfig::Plugin".to_string(),
            ));
        };
        let DestinationConfig::Plugin(dest_cfg) = destination_config else {
            return Err(AppError::Validation(
                "JsChannelSender::send_test expected DestinationConfig::Plugin".to_string(),
            ));
        };
        let entry = self.registry.get_js(&acct_cfg.plugin_id).ok_or_else(|| {
            AppError::NotFound(format!(
                "Channel plugin '{}' is not installed",
                acct_cfg.plugin_id
            ))
        })?;
        if entry.runtime.manifest.kind != PluginKind::Channel {
            return Err(AppError::Validation(format!(
                "Plugin '{}' is a '{}' plugin, not a channel plugin",
                acct_cfg.plugin_id,
                entry.runtime.manifest.kind.as_str()
            )));
        }
        if !entry.compatible {
            return Err(AppError::Validation(format!(
                "Channel plugin '{}' is incompatible with current core: {}",
                acct_cfg.plugin_id,
                entry
                    .compatibility_reason
                    .as_deref()
                    .unwrap_or("version mismatch")
            )));
        }
        if !entry.enabled {
            return Err(AppError::Validation(format!(
                "Channel plugin '{}' is disabled",
                acct_cfg.plugin_id
            )));
        }
        entry
            .runtime
            .test_config(
                &self.http,
                "destination",
                Some(&acct_cfg.config),
                Some(&dest_cfg.config),
                None,
                use_proxy,
                locale,
            )
            .await
    }
}

#[async_trait]
impl ChannelSender for JsChannelSender {
    fn kind(&self) -> ChannelKind {
        ChannelKind::Plugin
    }

    async fn send(&self, ctx: &DeliveryContext<'_>) -> AppResult<DeliveryOutcome> {
        let ChannelConfig::Plugin(acct_cfg) = ctx.channel_config else {
            return Err(AppError::Validation(
                "JsChannelSender expected ChannelConfig::Plugin".to_string(),
            ));
        };
        let DestinationConfig::Plugin(dest_cfg) = ctx.destination_config else {
            return Err(AppError::Validation(
                "JsChannelSender expected DestinationConfig::Plugin".to_string(),
            ));
        };

        let Some(entry) = self.registry.get_js(&acct_cfg.plugin_id) else {
            return Ok(DeliveryOutcome::PermanentFailure {
                status_code: None,
                checkpoint: ctx.checkpoint.clone(),
                diagnostic: format!(
                    "Channel plugin '{}' is no longer installed",
                    acct_cfg.plugin_id
                ),
            });
        };
        let runtime = &entry.runtime;
        if runtime.manifest.kind != PluginKind::Channel {
            return Ok(DeliveryOutcome::PermanentFailure {
                status_code: None,
                checkpoint: ctx.checkpoint.clone(),
                diagnostic: format!(
                    "Plugin '{}' has kind '{}' instead of 'channel'",
                    acct_cfg.plugin_id,
                    runtime.manifest.kind.as_str()
                ),
            });
        }
        if !entry.compatible {
            return Ok(DeliveryOutcome::Deferred {
                next_window_at: now_unix().saturating_add(300),
                checkpoint: ctx.checkpoint.clone(),
                diagnostic: format!(
                    "Channel plugin '{}' is incompatible with current core: {}",
                    acct_cfg.plugin_id,
                    entry
                        .compatibility_reason
                        .as_deref()
                        .unwrap_or("version mismatch")
                ),
            });
        }
        if !entry.enabled {
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

        let published_local = ctx
            .entry
            .published_at
            .and_then(|ts| format_local_timestamp_compact(ts, ctx.timezone));

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
                "plain_text": plain_text,
                "content_html": effective_html,
                "author": ctx.entry.author,
                "published_at": ctx.entry.published_at,
                "published_local": published_local,
                "media": media,
            },
            "feed": {
                "id": ctx.feed.id,
                "title": ctx.feed.title,
                "url": ctx.feed.url,
                "site_url": ctx.feed.site_url,
                "favicon_url": ctx.feed.favicon_url,
            },
            "route": {
                "id": ctx.route.id,
                "name": ctx.route.name,
                "locale": ctx.route_options.locale.map(|l| l.as_str()),
                "timezone": ctx.timezone.name(),
            },
            "checkpoint": cp_value,
        });

        let mut origins = extract_uri_origins(&runtime.manifest.account_schema, &acct_cfg.config);
        origins.extend(extract_uri_origins(
            &runtime.manifest.destination_schema,
            &dest_cfg.config,
        ));
        let policy = runtime.build_fetch_policy(
            &self.http,
            origins,
            ctx.channel.use_proxy,
            JS_MAX_FETCH_CALLS_DEFAULT,
        );

        let raw_val = match runtime
            .invoke_export("deliver", payload, Some(policy), JS_HOOK_DELIVER_TIMEOUT)
            .await
        {
            Ok(Some(v)) if !v.is_null() => v,
            Ok(_) => {
                return Ok(DeliveryOutcome::Retry {
                    retry_after_secs: None,
                    status_code: None,
                    checkpoint: ctx.checkpoint.clone(),
                    diagnostic: format!(
                        "Channel plugin '{}' deliver() returned empty/null result",
                        acct_cfg.plugin_id
                    ),
                    cooldown_credential_group: false,
                });
            }
            Err(AppError::SsrfBlocked(msg)) => {
                return Ok(DeliveryOutcome::PermanentFailure {
                    status_code: None,
                    checkpoint: ctx.checkpoint.clone(),
                    diagnostic: crate::logging::redact::redact_text(&msg),
                });
            }
            Err(AppError::Validation(msg)) => {
                return Ok(DeliveryOutcome::PermanentFailure {
                    status_code: None,
                    checkpoint: ctx.checkpoint.clone(),
                    diagnostic: crate::logging::redact::redact_text(&msg),
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

        let parsed: JsDeliverResponse = match serde_json::from_value(raw_val) {
            Ok(p) => p,
            Err(err) => {
                return Ok(DeliveryOutcome::Retry {
                    retry_after_secs: None,
                    status_code: None,
                    checkpoint: ctx.checkpoint.clone(),
                    diagnostic: format!("Invalid JS plugin deliver() result: {err}"),
                    cooldown_credential_group: false,
                });
            }
        };

        let next_checkpoint = match parsed.checkpoint {
            Some(cp_val) if !cp_val.is_null() => {
                let encoded = serde_json::to_string(&cp_val).unwrap_or_default();
                if encoded.len() > JS_MAX_CHECKPOINT_BYTES {
                    return Ok(DeliveryOutcome::PermanentFailure {
                        status_code: parsed.status_code,
                        checkpoint: ctx.checkpoint.clone(),
                        diagnostic: format!(
                            "Plugin returned checkpoint exceeding 16 KiB limit ({} bytes)",
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
                status_code: parsed.status_code.or(Some(200)),
                checkpoint: next_checkpoint,
                diagnostic: combined_diag,
                updated_capabilities: None,
            }),
            "retry" => Ok(DeliveryOutcome::Retry {
                retry_after_secs: parsed.retry_after_secs,
                status_code: parsed.status_code,
                checkpoint: next_checkpoint,
                diagnostic: combined_diag
                    .unwrap_or_else(|| "Plugin requested transient retry".to_string()),
                cooldown_credential_group: parsed.retry_after_secs.is_some()
                    || parsed.status_code == Some(429),
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
                        .unwrap_or_else(|| "Plugin deferred delivery".to_string()),
                })
            }
            "permanent_failure" => Ok(DeliveryOutcome::PermanentFailure {
                status_code: parsed.status_code,
                checkpoint: next_checkpoint,
                diagnostic: combined_diag
                    .unwrap_or_else(|| "Plugin reported permanent delivery failure".to_string()),
            }),
            other => Ok(DeliveryOutcome::Retry {
                retry_after_secs: None,
                status_code: parsed.status_code,
                checkpoint: next_checkpoint,
                diagnostic: format!("Plugin returned unknown outcome '{other}'"),
                cooldown_credential_group: false,
            }),
        }
    }

    async fn send_test(
        &self,
        channel_config: &ChannelConfig,
        destination_config: &DestinationConfig,
        use_proxy: bool,
    ) -> AppResult<String> {
        self.send_test_with_locale(channel_config, destination_config, use_proxy, "en")
            .await
    }
}

/// Helper verifying that a newly compiled JS plugin satisfies core version compatibility before
/// persisting it into SQLite (`POST /api/v1/plugins/install` or `POST /api/v1/plugins/upload`).
pub fn ensure_compiled_plugin_compatible(manifest: &PluginManifest) -> AppResult<()> {
    let (compat, reason) =
        check_plugin_compatibility(&manifest.api_version, manifest.min_core.as_deref());
    if !compat {
        return Err(AppError::Validation(reason.unwrap_or_else(|| {
            "plugin is incompatible with this FeedCourier version".to_string()
        })));
    }
    Ok(())
}
