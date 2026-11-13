//! Protected outbound HTTP client, connect-time DNS/SSRF guard, hop-by-hop redirect validation,
//! response body size caps, and optional HTTP/SOCKS5 proxy routing (`feedcourier.md` Section 四.1).

pub mod ssrf;

use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use reqwest::header::{self, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, Proxy, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::config::{APP_NAME, AppConfig, REPO_URL, VERSION};
use crate::error::{AppError, AppResult};
use crate::net::ssrf::{CustomDnsLookup, SsrfDnsResolver, SsrfPolicy, find_ssrf_blocked_in_error};

/// Default maximum response body size when fetching RSS/Atom/JSON Feeds (`10 MiB`).
pub const DEFAULT_MAX_FEED_BYTES: usize = 10 * 1024 * 1024;

/// Default maximum response body size when fetching article HTML for Readability (`5 MiB`).
pub const DEFAULT_MAX_ARTICLE_BYTES: usize = 5 * 1024 * 1024;

/// Default maximum HTTP redirect hops validated individually against the SSRF guard.
pub const DEFAULT_MAX_REDIRECT_HOPS: usize = 5;

/// Default TCP/TLS connect timeout (`10s`).
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Default overall request timeout (`30s`).
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Runtime-mutable outbound network settings (`PUT /api/v1/settings`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetSettings {
    /// Optional global outbound proxy URL (`http://`, `https://`, `socks5://`, `socks5h://`).
    #[serde(default)]
    pub outbound_proxy_url: Option<String>,
    /// CIDR blocks (e.g. `10.0.0.0/8`, `127.0.0.1/32`) and internal hostnames (e.g. `rsshub.internal`, `*.lan`).
    #[serde(default)]
    pub ssrf_allowlist: Vec<String>,
}

impl NetSettings {
    /// Construct initial [`NetSettings`] from [`AppConfig`].
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            outbound_proxy_url: config.outbound_proxy.clone(),
            ssrf_allowlist: config.ssrf_allowlist.clone(),
        }
    }
}

/// Request parameters for an SSRF-protected outbound HTTP `GET` operation.
#[derive(Debug, Clone)]
pub struct OutboundRequest {
    pub url: String,
    pub max_bytes: usize,
    pub use_proxy: bool,
    pub if_none_match: Option<String>,
    pub if_modified_since: Option<String>,
    pub headers: HeaderMap,
    pub timeout: Option<Duration>,
    pub max_redirects: usize,
}

impl OutboundRequest {
    /// Create a request targeting `url` with [`DEFAULT_MAX_FEED_BYTES`] (`10 MiB`) and
    /// [`DEFAULT_MAX_REDIRECT_HOPS`] (`5`).
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            max_bytes: DEFAULT_MAX_FEED_BYTES,
            use_proxy: false,
            if_none_match: None,
            if_modified_since: None,
            headers: HeaderMap::new(),
            timeout: None,
            max_redirects: DEFAULT_MAX_REDIRECT_HOPS,
        }
    }

    pub fn max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    pub fn use_proxy(mut self, use_proxy: bool) -> Self {
        self.use_proxy = use_proxy;
        self
    }

    pub fn if_none_match(mut self, etag: Option<String>) -> Self {
        self.if_none_match = etag.filter(|s| !s.trim().is_empty());
        self
    }

    pub fn if_modified_since(mut self, last_modified: Option<String>) -> Self {
        self.if_modified_since = last_modified.filter(|s| !s.trim().is_empty());
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn max_redirects(mut self, max_redirects: usize) -> Self {
        self.max_redirects = max_redirects;
        self
    }

    pub fn header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }
}

/// Final response returned by [`ProtectedHttpClient::get_bytes`].
#[derive(Debug, Clone)]
pub struct FetchedResponse {
    pub status: StatusCode,
    pub final_url: Url,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub retry_after_secs: Option<u64>,
    pub content_type: Option<String>,
    pub bytes: Bytes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProxyDnsMode {
    /// Proxy resolves hostnames remotely (`socks5h`, `socks4a`, `http`, `https`).
    Remote,
    /// Client resolves hostnames locally before SOCKS handshake (`socks5`, `socks4`).
    Local,
}

struct ClientSet {
    settings: NetSettings,
    /// Direct client for untrusted URLs with connect-time `SsrfDnsResolver` and `no_proxy()`.
    ssrf_direct_client: reqwest::Client,
    /// Optional proxied client for untrusted URLs (`redirect::Policy::none()`).
    ssrf_proxied_client: Option<(reqwest::Client, ProxyDnsMode)>,
    /// Direct client for trusted downstream APIs (Telegram Bot API, Notion API).
    trusted_direct_client: reqwest::Client,
    /// Optional proxied client for trusted downstream APIs.
    trusted_proxied_client: Option<reqwest::Client>,
}

struct ProtectedHttpInner {
    policy: Arc<RwLock<SsrfPolicy>>,
    resolver: SsrfDnsResolver,
    state: RwLock<ClientSet>,
}

/// Unified SSRF-guarded HTTP client (`Clone` via `Arc`) shared across feed polling, Readability,
/// anti-hotlink media streaming, and live feed previews.
#[derive(Clone)]
pub struct ProtectedHttpClient {
    inner: Arc<ProtectedHttpInner>,
}

impl fmt::Debug for ProtectedHttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let settings = self.settings();
        f.debug_struct("ProtectedHttpClient")
            .field("outbound_proxy_url", &settings.outbound_proxy_url)
            .field("ssrf_allowlist", &settings.ssrf_allowlist)
            .finish()
    }
}

impl Default for ProtectedHttpClient {
    fn default() -> Self {
        Self::with_defaults()
    }
}

impl ProtectedHttpClient {
    /// Construct a new [`ProtectedHttpClient`] from [`NetSettings`].
    pub fn new(settings: NetSettings) -> AppResult<Self> {
        let policy = Arc::new(RwLock::new(SsrfPolicy::from_allowlist(
            &settings.ssrf_allowlist,
        )?));
        let resolver = SsrfDnsResolver::new(Arc::clone(&policy));
        let client_set = build_client_set(settings, &resolver)?;

        Ok(Self {
            inner: Arc::new(ProtectedHttpInner {
                policy,
                resolver,
                state: RwLock::new(client_set),
            }),
        })
    }

    /// Construct a [`ProtectedHttpClient`] with empty default settings (blocks all private/internal IPs).
    pub fn with_defaults() -> Self {
        Self::new(NetSettings::default())
            .expect("default NetSettings must build ProtectedHttpClient")
    }

    /// Construct a [`ProtectedHttpClient`] with a custom DNS lookup hook (for deterministic rebinding tests).
    pub fn with_custom_dns(settings: NetSettings, lookup: CustomDnsLookup) -> AppResult<Self> {
        let policy = Arc::new(RwLock::new(SsrfPolicy::from_allowlist(
            &settings.ssrf_allowlist,
        )?));
        let resolver = SsrfDnsResolver::with_custom_lookup(Arc::clone(&policy), lookup);
        let client_set = build_client_set(settings, &resolver)?;

        Ok(Self {
            inner: Arc::new(ProtectedHttpInner {
                policy,
                resolver,
                state: RwLock::new(client_set),
            }),
        })
    }

    /// Snapshot current [`NetSettings`].
    pub fn settings(&self) -> NetSettings {
        self.inner
            .state
            .read()
            .expect("ProtectedHttpClient state RwLock poisoned")
            .settings
            .clone()
    }

    /// Hot-reload [`NetSettings`] (`outbound_proxy_url` and `ssrf_allowlist`) without process restart.
    pub fn update_settings(&self, settings: NetSettings) -> AppResult<()> {
        let new_policy = SsrfPolicy::from_allowlist(&settings.ssrf_allowlist)?;
        let new_client_set = build_client_set(settings, &self.inner.resolver)?;

        {
            let mut policy_guard = self
                .inner
                .policy
                .write()
                .expect("SSRF policy RwLock poisoned");
            *policy_guard = new_policy;
        }
        {
            let mut state_guard = self
                .inner
                .state
                .write()
                .expect("ProtectedHttpClient state RwLock poisoned");
            *state_guard = new_client_set;
        }
        Ok(())
    }

    /// Return a `reqwest::Client` for **trusted** admin-configured APIs (e.g. Telegram Bot API,
    /// Notion API), honoring `use_proxy` when a global proxy is configured.
    ///
    /// **Never** use this method for user/feed-supplied URLs; use [`Self::get_bytes`] instead.
    pub fn trusted_client(&self, use_proxy: bool) -> reqwest::Client {
        let guard = self
            .inner
            .state
            .read()
            .expect("ProtectedHttpClient state RwLock poisoned");
        if use_proxy && let Some(proxied) = &guard.trusted_proxied_client {
            return proxied.clone();
        }
        guard.trusted_direct_client.clone()
    }

    /// Helper for executing JSON requests against trusted downstream APIs with optional proxy routing.
    pub async fn execute_trusted_json<Req, Resp>(
        &self,
        method: Method,
        url: &str,
        body: Option<&Req>,
        headers: Option<HeaderMap>,
        use_proxy: bool,
    ) -> AppResult<Resp>
    where
        Req: Serialize + ?Sized,
        Resp: DeserializeOwned,
    {
        let client = self.trusted_client(use_proxy);
        let mut builder = client.request(method, url);
        if let Some(hdrs) = headers {
            builder = builder.headers(hdrs);
        }
        if let Some(payload) = body {
            builder = builder.json(payload);
        }
        let resp = builder
            .send()
            .await
            .map_err(|err| AppError::Upstream(format!("upstream API request failed: {err}")))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(AppError::Upstream(format!(
                "upstream API returned HTTP {status}: {text}"
            )));
        }
        resp.json::<Resp>().await.map_err(|err| {
            AppError::Upstream(format!(
                "failed to decode upstream API JSON response: {err}"
            ))
        })
    }

    /// Fetch an untrusted URL with connect-time SSRF validation, manual hop-by-hop redirect
    /// inspection, conditional `ETag` / `Last-Modified` headers, and streaming `max_bytes` enforcement.
    pub async fn get_bytes(&self, req: OutboundRequest) -> AppResult<FetchedResponse> {
        let mut current_url = Url::parse(req.url.trim())?;
        let max_bytes = req.max_bytes;

        let (client, proxy_dns_mode) = {
            let guard = self
                .inner
                .state
                .read()
                .expect("ProtectedHttpClient state RwLock poisoned");
            if req.use_proxy
                && let Some((proxied, mode)) = &guard.ssrf_proxied_client
            {
                (proxied.clone(), Some(*mode))
            } else {
                (guard.ssrf_direct_client.clone(), None)
            }
        };

        for hop in 0..=req.max_redirects {
            // 1. Validate scheme, host, and any literal IP against current SSRF policy on EVERY hop.
            {
                let policy = self
                    .inner
                    .policy
                    .read()
                    .expect("SSRF policy RwLock poisoned")
                    .clone();
                let remote_proxy_dns = matches!(proxy_dns_mode, Some(ProxyDnsMode::Remote));
                policy.validate_url_hop(&current_url, remote_proxy_dns)?;
            }

            // When using a local-DNS SOCKS proxy (`socks5://`), explicitly validate resolved IPs too.
            if matches!(proxy_dns_mode, Some(ProxyDnsMode::Local))
                && let Some(host_str) = current_url.host_str()
            {
                self.inner.resolver.resolve_and_validate(host_str).await?;
            }

            // 2. Assemble GET request (only safe headers are carried across redirect hops).
            let mut rb = client.get(current_url.clone());
            if let Some(timeout) = req.timeout {
                rb = rb.timeout(timeout);
            }
            for (k, v) in &req.headers {
                if hop == 0 || is_redirect_safe_header(k) {
                    rb = rb.header(k, v);
                }
            }
            if let Some(etag) = &req.if_none_match {
                rb = rb.header(header::IF_NONE_MATCH, etag);
            }
            if let Some(lm) = &req.if_modified_since {
                rb = rb.header(header::IF_MODIFIED_SINCE, lm);
            }

            let resp = rb
                .send()
                .await
                .map_err(|err| map_reqwest_error(&current_url, err))?;
            let status = resp.status();

            // 3. Manual hop-by-hop redirect handling (301, 302, 303, 307, 308).
            if matches!(
                status,
                StatusCode::MOVED_PERMANENTLY
                    | StatusCode::FOUND
                    | StatusCode::SEE_OTHER
                    | StatusCode::TEMPORARY_REDIRECT
                    | StatusCode::PERMANENT_REDIRECT
            ) {
                if hop == req.max_redirects {
                    return Err(AppError::Upstream(format!(
                        "exceeded maximum redirect hops ({}) at '{current_url}'",
                        req.max_redirects
                    )));
                }

                let location_val = resp.headers().get(header::LOCATION).ok_or_else(|| {
                    AppError::Upstream(format!(
                        "HTTP {status} redirect from '{current_url}' missing Location header"
                    ))
                })?;
                let location_str = location_val.to_str().map_err(|_| {
                    AppError::Upstream(format!(
                        "HTTP {status} redirect from '{current_url}' has non-ASCII Location header"
                    ))
                })?;
                current_url = current_url.join(location_str.trim())?;
                continue;
            }

            // 4. Extract response metadata headers.
            let etag = header_string(resp.headers(), header::ETAG);
            let last_modified = header_string(resp.headers(), header::LAST_MODIFIED);
            let content_type = header_string(resp.headers(), header::CONTENT_TYPE);
            let retry_after_secs = resp
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_retry_after_secs);

            if status == StatusCode::NOT_MODIFIED {
                return Ok(FetchedResponse {
                    status,
                    final_url: current_url,
                    etag,
                    last_modified,
                    retry_after_secs,
                    content_type,
                    bytes: Bytes::new(),
                });
            }

            // 5. Fast-fail on declared Content-Length exceeding `max_bytes`.
            if let Some(declared_len) = resp.content_length()
                && declared_len > max_bytes as u64
            {
                return Err(AppError::Upstream(format!(
                    "response Content-Length ({declared_len} bytes) from '{current_url}' exceeds maximum allowed size ({max_bytes} bytes)"
                )));
            }

            // 6. Stream body chunks and abort immediately if cumulative size exceeds `max_bytes`.
            let initial_cap = resp
                .content_length()
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(8 * 1024)
                .min(max_bytes)
                .min(64 * 1024);
            let mut buf = BytesMut::with_capacity(initial_cap);
            let mut stream = resp.bytes_stream();

            while let Some(chunk_res) = stream.next().await {
                let chunk = chunk_res.map_err(|err| map_reqwest_error(&current_url, err))?;
                if buf.len().saturating_add(chunk.len()) > max_bytes {
                    return Err(AppError::Upstream(format!(
                        "streamed response body from '{current_url}' exceeded maximum allowed size ({max_bytes} bytes)"
                    )));
                }
                buf.extend_from_slice(&chunk);
            }

            return Ok(FetchedResponse {
                status,
                final_url: current_url,
                etag,
                last_modified,
                retry_after_secs,
                content_type,
                bytes: buf.freeze(),
            });
        }

        Err(AppError::Upstream(format!(
            "exceeded maximum redirect hops ({})",
            req.max_redirects
        )))
    }
}

fn default_user_agent() -> String {
    format!("{APP_NAME}/{VERSION} (+{REPO_URL})")
}

fn build_client_set(settings: NetSettings, resolver: &SsrfDnsResolver) -> AppResult<ClientSet> {
    let ua = default_user_agent();

    let ssrf_direct_client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(Arc::new(resolver.clone()))
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
        .timeout(DEFAULT_REQUEST_TIMEOUT)
        .user_agent(&ua)
        .build()
        .map_err(|err| AppError::Config(format!("failed to build direct HTTP client: {err}")))?;

    let trusted_direct_client = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
        .timeout(DEFAULT_REQUEST_TIMEOUT)
        .user_agent(&ua)
        .build()
        .map_err(|err| AppError::Config(format!("failed to build trusted HTTP client: {err}")))?;

    let proxy_raw = settings
        .outbound_proxy_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let (ssrf_proxied_client, trusted_proxied_client) = if let Some(proxy_str) = proxy_raw {
        let proxy_url = Url::parse(proxy_str).map_err(|err| {
            AppError::Validation(format!("invalid outbound_proxy_url '{proxy_str}': {err}"))
        })?;
        let dns_mode = match proxy_url.scheme() {
            "socks5" | "socks4" => ProxyDnsMode::Local,
            "socks5h" | "socks4a" | "http" | "https" => ProxyDnsMode::Remote,
            other => {
                return Err(AppError::Validation(format!(
                    "unsupported outbound_proxy_url scheme '{other}' (expected http, https, socks5, or socks5h)"
                )));
            }
        };

        let proxy_ssrf = Proxy::all(proxy_str).map_err(|err| {
            AppError::Validation(format!("invalid outbound_proxy_url '{proxy_str}': {err}"))
        })?;
        let proxy_trusted = Proxy::all(proxy_str).map_err(|err| {
            AppError::Validation(format!("invalid outbound_proxy_url '{proxy_str}': {err}"))
        })?;

        let ssrf_proxied = reqwest::Client::builder()
            .no_proxy()
            .proxy(proxy_ssrf)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .user_agent(&ua)
            .build()
            .map_err(|err| {
                AppError::Config(format!("failed to build proxied HTTP client: {err}"))
            })?;

        let trusted_proxied = reqwest::Client::builder()
            .no_proxy()
            .proxy(proxy_trusted)
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .user_agent(&ua)
            .build()
            .map_err(|err| {
                AppError::Config(format!(
                    "failed to build trusted proxied HTTP client: {err}"
                ))
            })?;

        (Some((ssrf_proxied, dns_mode)), Some(trusted_proxied))
    } else {
        (None, None)
    };

    let normalized_proxy = proxy_raw.map(ToOwned::to_owned);
    let mut normalized_settings = settings;
    normalized_settings.outbound_proxy_url = normalized_proxy;

    Ok(ClientSet {
        settings: normalized_settings,
        ssrf_direct_client,
        ssrf_proxied_client,
        trusted_direct_client,
        trusted_proxied_client,
    })
}

fn is_redirect_safe_header(name: &HeaderName) -> bool {
    !matches!(
        *name,
        header::AUTHORIZATION | header::PROXY_AUTHORIZATION | header::COOKIE | header::HOST
    )
}

fn header_string(headers: &HeaderMap, name: HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

/// Parse an HTTP `Retry-After` header (either integer delta-seconds or IMF-fixdate / RFC 2822).
pub fn parse_retry_after_secs(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    if let Ok(secs) = trimmed.parse::<u64>() {
        return Some(secs);
    }
    if let Ok(dt) = DateTime::parse_from_rfc2822(trimmed) {
        let diff = dt.timestamp() - Utc::now().timestamp();
        return Some(u64::try_from(diff.max(0)).unwrap_or(0));
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        let diff = dt.timestamp() - Utc::now().timestamp();
        return Some(u64::try_from(diff.max(0)).unwrap_or(0));
    }
    None
}

fn map_reqwest_error(url: &Url, err: reqwest::Error) -> AppError {
    if let Some(ssrf_detail) = find_ssrf_blocked_in_error(&err) {
        return AppError::SsrfBlocked(ssrf_detail);
    }
    if err.is_timeout() {
        return AppError::Upstream(format!("request timed out fetching '{url}': {err}"));
    }
    AppError::Upstream(format!("HTTP request failed for '{url}': {err}"))
}
