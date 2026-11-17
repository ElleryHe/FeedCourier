//! Telegram Bot API 10.4 delivery adapter (`feedcourier.md` Section 五.2).
//!
//! Supports private chats, supergroup forum topics (`message_thread_id`), channels, and
//! custom Local Bot API Server endpoints across three per-route presentation modes:
//! 1. `RichMessage` (`sendRichMessage` via `telegram-rich`, with capability probing and
//!    automatic runtime fallback to `CompactCard` on older `<10.2` servers).
//! 2. `CompactCard` (`sendMessage` HTML summary card bounded to `<= 4096` UTF-16 units).
//! 3. `MediaMode` (strict 3-line minimalist caption, 10-item batching with mid-flight
//!    `CheckpointSink` persistence, URL-first -> zero-disk streaming `multipart/form-data`
//!    anti-hotlink fallback, global concurrency semaphore, and 10 MiB / 50 MiB byte caps).

use std::net::IpAddr;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue, REFERER, USER_AGENT};
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use telegram_rich::html::{ParseOptions, normalize};
use telegram_rich::types::InputRichMessage;
use telegram_rich::{Block, FitOptions};
use tokio::sync::Semaphore;
use url::Url;

use crate::channels::{
    ChannelConfig, ChannelKind, ChannelSender, DeliveryCheckpoint, DeliveryContext,
    DeliveryOutcome, TelegramCapabilities, TelegramChannelConfig, TelegramDeliveryMode,
};
use crate::db::models::{AppLocale, Entry, Feed, MediaAttachment, MediaAttachmentKind, now_unix};
use crate::error::{AppError, AppResult};

/// Default official Telegram Bot API base URL.
pub const DEFAULT_TELEGRAM_API_BASE_URL: &str = "https://api.telegram.org";

/// Global cap on concurrent in-memory media fallback streams/uploads (`feedcourier.md` §五.2).
pub const MAX_CONCURRENT_MEDIA_UPLOADS: usize = 2;

/// Maximum photo upload size (`10 MiB`) enforced by Telegram Bot API.
pub const MAX_PHOTO_BYTES: usize = 10 * 1024 * 1024;

/// Maximum video/audio/document upload size (`50 MiB`) enforced by standard Bot API.
pub const MAX_MEDIA_BYTES: usize = 50 * 1024 * 1024;

/// Maximum Telegram text message length in UTF-16 code units.
pub const MAX_MESSAGE_UTF16_UNITS: usize = 4096;

/// Maximum Telegram media caption length in UTF-16 code units.
pub const MAX_CAPTION_UTF16_UNITS: usize = 1024;

/// Maximum items allowed in a single Telegram `sendMediaGroup` call.
pub const MEDIA_GROUP_BATCH_SIZE: usize = 10;

/// Maximum redirect hops followed when streaming fallback media.
const MAX_MEDIA_REDIRECT_HOPS: usize = 5;

/// Default HTTP timeout for Bot API requests and media streams.
const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(45);

/// Process-wide semaphore capping concurrent media fallback uploads at `2`.
static GLOBAL_MEDIA_UPLOAD_SEMAPHORE: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_CONCURRENT_MEDIA_UPLOADS)));

/// In-memory streamed media buffer returned by [`MediaStreamFetcher`].
#[derive(Debug, Clone)]
pub struct FetchedMedia {
    pub bytes: Bytes,
    pub content_type: Option<String>,
    pub filename: String,
}

/// Extracted video dimensions and duration for Telegram `sendVideo` / `InputMediaVideo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VideoMetadata {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_secs: Option<u32>,
}

/// Injectable streaming media downloader used when Telegram fails to fetch an anti-hotlinked URL.
///
/// Wave 3 injects `ProtectedHttpClient` via [`TelegramSender::with_media_fetcher`].
#[async_trait]
pub trait MediaStreamFetcher: Send + Sync {
    async fn fetch_media(
        &self,
        url: &str,
        max_bytes: usize,
        use_proxy: bool,
    ) -> AppResult<FetchedMedia>;
}

/// Default `reqwest`-backed [`MediaStreamFetcher`] with scheme/DNS SSRF checks,
/// hop-by-hop redirect validation, origin `Referer` injection, and streaming byte caps.
#[derive(Debug, Clone)]
pub struct DefaultMediaStreamFetcher {
    direct_client: reqwest::Client,
    proxy_url: Option<String>,
    allow_private_for_tests: bool,
}

impl Default for DefaultMediaStreamFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultMediaStreamFetcher {
    /// Construct a default SSRF-guarded media stream fetcher.
    pub fn new() -> Self {
        Self::build(None, false)
    }

    /// Construct a fetcher with an optional outbound HTTP/SOCKS5 proxy URL.
    pub fn with_proxy(proxy_url: Option<String>) -> Self {
        Self::build(proxy_url, false)
    }

    /// Override private/loopback IP blocking for local integration tests (metadata `169.254.169.254`
    /// remains unconditionally blocked).
    pub fn with_allow_private_for_tests(mut self, allow: bool) -> Self {
        self.allow_private_for_tests = allow;
        self
    }

    fn build(proxy_url: Option<String>, allow_private_for_tests: bool) -> Self {
        let direct_client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(DEFAULT_HTTP_TIMEOUT)
            .connect_timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client builder must succeed");
        Self {
            direct_client,
            proxy_url,
            allow_private_for_tests,
        }
    }

    fn client_for(&self, use_proxy: bool) -> AppResult<reqwest::Client> {
        if use_proxy
            && let Some(ref raw_proxy) = self.proxy_url
            && !raw_proxy.trim().is_empty()
        {
            let proxy = reqwest::Proxy::all(raw_proxy.trim())
                .map_err(|err| AppError::Config(format!("invalid outbound proxy URL: {err}")))?;
            return reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(DEFAULT_HTTP_TIMEOUT)
                .connect_timeout(Duration::from_secs(15))
                .proxy(proxy)
                .build()
                .map_err(|err| AppError::Upstream(format!("proxy client build error: {err}")));
        }
        Ok(self.direct_client.clone())
    }

    async fn validate_media_url(&self, parsed: &Url) -> AppResult<()> {
        match parsed.scheme() {
            "http" | "https" => {}
            other => {
                return Err(AppError::SsrfBlocked(format!(
                    "unsupported media URL scheme '{other}'"
                )));
            }
        }

        let Some(host) = parsed.host_str() else {
            return Err(AppError::Validation("media URL missing host".to_owned()));
        };
        let normalized_host = host.trim_start_matches('[').trim_end_matches(']');

        if let Ok(ip) = normalized_host.parse::<IpAddr>() {
            check_ip_allowed(ip, self.allow_private_for_tests)?;
            return Ok(());
        }

        let port = parsed.port_or_known_default().unwrap_or(80);
        let addrs = tokio::net::lookup_host((normalized_host, port))
            .await
            .map_err(|err| {
                AppError::Upstream(format!(
                    "DNS resolution failed for '{normalized_host}': {err}"
                ))
            })?;

        let mut resolved_any = false;
        for sock_addr in addrs {
            resolved_any = true;
            check_ip_allowed(sock_addr.ip(), self.allow_private_for_tests)?;
        }
        if !resolved_any {
            return Err(AppError::Upstream(format!(
                "DNS resolution returned no addresses for '{normalized_host}'"
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl MediaStreamFetcher for DefaultMediaStreamFetcher {
    async fn fetch_media(
        &self,
        url: &str,
        max_bytes: usize,
        use_proxy: bool,
    ) -> AppResult<FetchedMedia> {
        let mut current = Url::parse(url.trim())?;
        let client = self.client_for(use_proxy)?;

        for hop in 0..=MAX_MEDIA_REDIRECT_HOPS {
            self.validate_media_url(&current).await?;

            let mut headers = HeaderMap::new();
            headers.insert(
                USER_AGENT,
                HeaderValue::from_static("Mozilla/5.0 (compatible; FeedCourier/0.1)"),
            );
            headers.insert(
                ACCEPT,
                HeaderValue::from_static("image/*,video/*,audio/*,*/*;q=0.8"),
            );
            if let Some(host) = current.host_str() {
                let origin = match current.port() {
                    Some(port) => format!("{}://{host}:{port}/", current.scheme()),
                    None => format!("{}://{host}/", current.scheme()),
                };
                if let Ok(referer) = HeaderValue::from_str(&origin) {
                    headers.insert(REFERER, referer);
                }
            }

            let resp = client
                .get(current.clone())
                .headers(headers)
                .send()
                .await
                .map_err(|err| AppError::Upstream(format!("media fetch failed: {err}")))?;

            let status = resp.status();
            if matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
                if hop == MAX_MEDIA_REDIRECT_HOPS {
                    return Err(AppError::Upstream(format!(
                        "media fetch exceeded {MAX_MEDIA_REDIRECT_HOPS} redirect hops"
                    )));
                }
                let loc = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| {
                        AppError::Upstream("redirect response missing Location header".to_owned())
                    })?;
                current = current.join(loc)?;
                continue;
            }

            if !status.is_success() {
                return Err(AppError::Upstream(format!(
                    "media origin returned HTTP {}",
                    status.as_u16()
                )));
            }

            if let Some(declared_len) = resp.content_length()
                && declared_len > max_bytes as u64
            {
                return Err(AppError::Validation(format!(
                    "media Content-Length ({declared_len} bytes) exceeds cap ({max_bytes} bytes)"
                )));
            }

            let content_type = resp
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|ct| {
                    ct.split(';')
                        .next()
                        .unwrap_or(ct)
                        .trim()
                        .to_ascii_lowercase()
                })
                .filter(|ct| !ct.is_empty());

            let filename = derive_filename(&current, content_type.as_deref());
            let initial_cap = resp
                .content_length()
                .and_then(|len| usize::try_from(len).ok())
                .map(|len| len.min(max_bytes))
                .unwrap_or(64 * 1024);

            let mut buf = BytesMut::with_capacity(initial_cap);
            let mut stream = resp.bytes_stream();
            while let Some(chunk_res) = stream.next().await {
                let chunk = chunk_res
                    .map_err(|err| AppError::Upstream(format!("media stream read error: {err}")))?;
                if buf.len().saturating_add(chunk.len()) > max_bytes {
                    return Err(AppError::Validation(format!(
                        "streamed media exceeded cap of {max_bytes} bytes"
                    )));
                }
                buf.extend_from_slice(&chunk);
            }

            if buf.is_empty() {
                return Err(AppError::Upstream(
                    "streamed media body is empty".to_owned(),
                ));
            }

            return Ok(FetchedMedia {
                bytes: buf.freeze(),
                content_type,
                filename,
            });
        }

        Err(AppError::Upstream("media redirect loop".to_owned()))
    }
}

/// Telegram Bot API 10.4 channel adapter.
#[derive(Clone)]
pub struct TelegramSender {
    api_client: reqwest::Client,
    proxy_url: Option<String>,
    media_fetcher: Arc<dyn MediaStreamFetcher>,
    upload_semaphore: Arc<Semaphore>,
    locale: AppLocale,
}

impl std::fmt::Debug for TelegramSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramSender")
            .field("has_proxy", &self.proxy_url.is_some())
            .field("locale", &self.locale)
            .field(
                "available_upload_permits",
                &self.upload_semaphore.available_permits(),
            )
            .finish()
    }
}

impl Default for TelegramSender {
    fn default() -> Self {
        Self::new()
    }
}

impl TelegramSender {
    /// Create a `TelegramSender` using [`DefaultMediaStreamFetcher`] and the global 2-permit semaphore.
    pub fn new() -> Self {
        Self::with_media_fetcher(Arc::new(DefaultMediaStreamFetcher::new()))
    }

    /// Create a `TelegramSender` injecting a custom [`MediaStreamFetcher`] (e.g. `ProtectedHttpClient`
    /// in Wave 3 or deterministic mocks in tests).
    pub fn with_media_fetcher(media_fetcher: Arc<dyn MediaStreamFetcher>) -> Self {
        Self::with_proxy_and_fetcher(None, media_fetcher)
    }

    /// Create a `TelegramSender` with an optional outbound proxy URL and custom [`MediaStreamFetcher`].
    pub fn with_proxy_and_fetcher(
        proxy_url: Option<String>,
        media_fetcher: Arc<dyn MediaStreamFetcher>,
    ) -> Self {
        let api_client = reqwest::Client::builder()
            .timeout(DEFAULT_HTTP_TIMEOUT)
            .connect_timeout(Duration::from_secs(15))
            .build()
            .expect("Telegram reqwest client must build");
        Self {
            api_client,
            proxy_url,
            media_fetcher,
            upload_semaphore: Arc::clone(&GLOBAL_MEDIA_UPLOAD_SEMAPHORE),
            locale: AppLocale::En,
        }
    }

    /// Set the delivery template locale on this `TelegramSender` (builder style).
    pub fn with_locale(mut self, locale: AppLocale) -> Self {
        self.locale = locale;
        self
    }

    /// Update the active delivery template locale.
    pub fn set_locale(&mut self, locale: AppLocale) {
        self.locale = locale;
    }

    /// Current delivery template locale.
    pub const fn locale(&self) -> AppLocale {
        self.locale
    }

    fn bot_api_client(&self, use_proxy: bool) -> AppResult<reqwest::Client> {
        if use_proxy
            && let Some(ref raw_proxy) = self.proxy_url
            && !raw_proxy.trim().is_empty()
        {
            let proxy = reqwest::Proxy::all(raw_proxy.trim())
                .map_err(|err| AppError::Config(format!("invalid Telegram proxy URL: {err}")))?;
            return reqwest::Client::builder()
                .timeout(DEFAULT_HTTP_TIMEOUT)
                .connect_timeout(Duration::from_secs(15))
                .proxy(proxy)
                .build()
                .map_err(|err| AppError::Upstream(format!("Telegram proxy client failed: {err}")));
        }
        Ok(self.api_client.clone())
    }

    /// Probe a Bot API endpoint (`getMe` + dry-run `sendRichMessage` `{}` check) without sending a chat message.
    pub async fn probe_capabilities(
        &self,
        cfg: &TelegramChannelConfig,
        use_proxy: bool,
    ) -> AppResult<TelegramCapabilities> {
        let client = self.bot_api_client(use_proxy)?;
        let get_me_url = api_method_url(cfg, "getMe");

        let me_resp =
            client.get(&get_me_url).send().await.map_err(|err| {
                AppError::Upstream(format!("Telegram getMe request failed: {err}"))
            })?;

        let me_status = me_resp.status().as_u16();
        let me_body: TgApiResponse<TgUser> = me_resp.json().await.map_err(|err| {
            AppError::Upstream(format!(
                "invalid Telegram getMe JSON (HTTP {me_status}): {err}"
            ))
        })?;

        if !me_body.ok {
            let code = me_body.error_code.unwrap_or(i32::from(me_status));
            let desc = me_body
                .description
                .unwrap_or_else(|| format!("HTTP {me_status}"));
            return match code {
                401 => Err(AppError::Unauthorized(format!(
                    "Telegram bot token unauthorized: {desc}"
                ))),
                429 => Err(AppError::RateLimited {
                    message: format!("Telegram getMe rate limited: {desc}"),
                    retry_after_secs: me_body.parameters.and_then(|p| p.retry_after),
                }),
                _ => Err(AppError::Upstream(format!(
                    "Telegram getMe failed ({code}): {desc}"
                ))),
            };
        }

        let bot_username = me_body.result.and_then(|u| u.username);

        // Probe `sendRichMessage` with empty JSON `{}`:
        // - Bot API 10.2+ / 10.4 validates parameters and replies `400 Bad Request: chat_id is empty`
        // - Older Local Bot API Server `<10.2` replies `404 Not Found` / unknown method
        let rich_url = api_method_url(cfg, "sendRichMessage");
        let probe_resp = client
            .post(&rich_url)
            .json(&json!({}))
            .send()
            .await
            .map_err(|err| {
                AppError::Upstream(format!("Telegram sendRichMessage probe failed: {err}"))
            })?;

        let probe_status = probe_resp.status().as_u16();
        let probe_json: Option<TgApiResponse<Value>> = probe_resp.json().await.ok();
        let err_code = probe_json
            .as_ref()
            .and_then(|b| b.error_code)
            .unwrap_or(i32::from(probe_status));
        let desc = probe_json
            .as_ref()
            .and_then(|b| b.description.clone())
            .unwrap_or_else(|| format!("HTTP {probe_status}"));

        if err_code == 429 {
            return Err(AppError::RateLimited {
                message: format!("Telegram probe rate limited: {desc}"),
                retry_after_secs: probe_json
                    .and_then(|b| b.parameters)
                    .and_then(|p| p.retry_after),
            });
        }
        if err_code == 401 {
            return Err(AppError::Unauthorized(format!(
                "Telegram bot token rejected: {desc}"
            )));
        }

        let (supports_rich_message, bot_api_version, fallback_reason) =
            if is_rich_method_unsupported(err_code, &desc) {
                (
                    false,
                    Some("<10.2".to_owned()),
                    Some(format!(
                        "Target Bot API server does not support sendRichMessage ({desc}); rich_message routes will degrade to compact_card."
                    )),
                )
            } else {
                (true, Some("10.4".to_owned()), None)
            };

        Ok(TelegramCapabilities {
            supports_rich_message,
            bot_api_version,
            bot_username,
            probed_at: Some(now_unix()),
            fallback_reason,
        })
    }

    async fn send_rich_mode(
        &self,
        ctx: &DeliveryContext<'_>,
        cfg: &TelegramChannelConfig,
    ) -> AppResult<DeliveryOutcome> {
        let existing = extract_telegram_checkpoint(&ctx.checkpoint);
        if !existing.sent_message_ids.is_empty() {
            return Ok(DeliveryOutcome::Delivered {
                status_code: Some(200),
                checkpoint: Some(existing.into_checkpoint()),
                diagnostic: Some("Telegram message already recorded in checkpoint".to_owned()),
                updated_capabilities: None,
            });
        }

        let caps_val = ctx.channel.capabilities_value();
        let cached_unsupported = caps_val.get("supports_rich_message") == Some(&Value::Bool(false));

        if cached_unsupported || existing.degraded_to_compact {
            return self.send_compact_mode(ctx, cfg, true, None).await;
        }

        let client = self.bot_api_client(ctx.channel.use_proxy)?;
        let rich_message = build_rich_message_payload_with_locale(ctx.feed, ctx.entry, self.locale);

        let mut payload = json!({
            "chat_id": cfg.chat_id.trim(),
            "rich_message": rich_message,
        });
        if let Some(thread_id) = cfg.message_thread_id {
            payload["message_thread_id"] = json!(thread_id);
        }

        match self
            .call_json_single_message(&client, cfg, "sendRichMessage", &payload)
            .await
        {
            Ok(msg_id) => {
                let cp = DeliveryCheckpoint::Telegram {
                    next_batch_offset: 1,
                    sent_message_ids: vec![msg_id],
                    degraded_to_compact: false,
                };
                if let Some(sink) = ctx.checkpoint_sink {
                    sink.save_checkpoint(&cp).await?;
                }
                Ok(DeliveryOutcome::Delivered {
                    status_code: Some(200),
                    checkpoint: Some(cp),
                    diagnostic: Some(format!("Telegram sendRichMessage delivered (id={msg_id})")),
                    updated_capabilities: None,
                })
            }
            Err(api_err) if api_err.is_rich_method_unsupported() => {
                let bot_username = caps_val
                    .get("bot_username")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let updated = TelegramCapabilities {
                    supports_rich_message: false,
                    bot_api_version: Some("<10.2".to_owned()),
                    bot_username,
                    probed_at: Some(now_unix()),
                    fallback_reason: Some(format!(
                        "Auto-degraded to compact_card because sendRichMessage returned {}: {}",
                        api_err.status_code.unwrap_or(404),
                        api_err.description
                    )),
                };
                let updated_val = serde_json::to_value(updated)?;
                self.send_compact_mode(ctx, cfg, true, Some(updated_val))
                    .await
            }
            Err(api_err) => Ok(classify_telegram_error(
                api_err,
                Some(existing.into_checkpoint()),
            )),
        }
    }

    async fn send_compact_mode(
        &self,
        ctx: &DeliveryContext<'_>,
        cfg: &TelegramChannelConfig,
        degraded_to_compact: bool,
        updated_capabilities: Option<Value>,
    ) -> AppResult<DeliveryOutcome> {
        let existing = extract_telegram_checkpoint(&ctx.checkpoint);
        if !existing.sent_message_ids.is_empty() {
            return Ok(DeliveryOutcome::Delivered {
                status_code: Some(200),
                checkpoint: Some(existing.into_checkpoint()),
                diagnostic: Some("Telegram compact card already delivered".to_owned()),
                updated_capabilities,
            });
        }

        let client = self.bot_api_client(ctx.channel.use_proxy)?;
        let html_text = format_compact_card_html_with_locale(ctx.feed, ctx.entry, self.locale);
        let canonical_url = ctx
            .entry
            .canonical_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let mut link_preview_options = json!({
            "is_disabled": !ctx.route_options.link_preview,
        });
        if let Some(url) = canonical_url {
            link_preview_options["url"] = json!(url);
        }

        let mut payload = json!({
            "chat_id": cfg.chat_id.trim(),
            "text": html_text,
            "parse_mode": "HTML",
            "link_preview_options": link_preview_options,
        });
        if let Some(thread_id) = cfg.message_thread_id {
            payload["message_thread_id"] = json!(thread_id);
        }

        let prior_cp = DeliveryCheckpoint::Telegram {
            next_batch_offset: 0,
            sent_message_ids: Vec::new(),
            degraded_to_compact,
        };

        match self
            .call_json_single_message(&client, cfg, "sendMessage", &payload)
            .await
        {
            Ok(msg_id) => {
                let cp = DeliveryCheckpoint::Telegram {
                    next_batch_offset: 1,
                    sent_message_ids: vec![msg_id],
                    degraded_to_compact,
                };
                if let Some(sink) = ctx.checkpoint_sink {
                    sink.save_checkpoint(&cp).await?;
                }
                let diag = if degraded_to_compact {
                    format!("Telegram degraded to compact_card and delivered (id={msg_id})")
                } else {
                    format!("Telegram compact_card delivered (id={msg_id})")
                };
                Ok(DeliveryOutcome::Delivered {
                    status_code: Some(200),
                    checkpoint: Some(cp),
                    diagnostic: Some(diag),
                    updated_capabilities,
                })
            }
            Err(api_err) => Ok(classify_telegram_error(api_err, Some(prior_cp))),
        }
    }

    async fn send_media_mode(
        &self,
        ctx: &DeliveryContext<'_>,
        cfg: &TelegramChannelConfig,
    ) -> AppResult<DeliveryOutcome> {
        let mut state = extract_telegram_checkpoint(&ctx.checkpoint);
        let client = self.bot_api_client(ctx.channel.use_proxy)?;

        let media_items: Vec<MediaAttachment> = ctx
            .entry
            .media()
            .into_iter()
            .filter(|m| !m.url.trim().is_empty())
            .collect();

        // 0 Media Fallback: send clean 3-line text via `sendMessage`.
        if media_items.is_empty() {
            if !state.sent_message_ids.is_empty() {
                return Ok(DeliveryOutcome::Delivered {
                    status_code: Some(200),
                    checkpoint: Some(state.into_checkpoint()),
                    diagnostic: Some("Telegram media_mode fallback already delivered".to_owned()),
                    updated_capabilities: None,
                });
            }

            let text = format_media_mode_caption_with_locale(
                ctx.feed,
                ctx.entry,
                MAX_MESSAGE_UTF16_UNITS,
                self.locale,
            );
            let msg_id = match self
                .send_plain_text_fallback(&client, cfg, ctx, &text)
                .await
            {
                Ok(id) => id,
                Err(api_err) => {
                    return Ok(classify_telegram_error(
                        api_err,
                        Some(state.into_checkpoint()),
                    ));
                }
            };

            state.next_batch_offset = 1;
            state.sent_message_ids.push(msg_id);
            let cp = state.into_checkpoint();
            if let Some(sink) = ctx.checkpoint_sink {
                sink.save_checkpoint(&cp).await?;
            }
            return Ok(DeliveryOutcome::Delivered {
                status_code: Some(200),
                checkpoint: Some(cp),
                diagnostic: Some(format!(
                    "Telegram media_mode (0-media text fallback) delivered (id={msg_id})"
                )),
                updated_capabilities: None,
            });
        }

        let batches: Vec<&[MediaAttachment]> = media_items.chunks(MEDIA_GROUP_BATCH_SIZE).collect();
        if state.next_batch_offset >= batches.len() {
            return Ok(DeliveryOutcome::Delivered {
                status_code: Some(200),
                checkpoint: Some(state.into_checkpoint()),
                diagnostic: Some("All Telegram media batches already delivered".to_owned()),
                updated_capabilities: None,
            });
        }

        let base_caption = format_media_mode_caption_with_locale(
            ctx.feed,
            ctx.entry,
            MAX_CAPTION_UTF16_UNITS,
            self.locale,
        );

        for (batch_idx, batch) in batches.iter().enumerate().skip(state.next_batch_offset) {
            let caption_for_batch = if batch_idx == 0 {
                Some(base_caption.as_str())
            } else {
                None
            };

            let batch_ids = match self
                .send_single_media_batch(&client, cfg, ctx, batch_idx, batch, caption_for_batch)
                .await
            {
                Ok(ids) => ids,
                Err(api_err) => {
                    return Ok(classify_telegram_error(
                        api_err,
                        Some(state.into_checkpoint()),
                    ));
                }
            };

            state.sent_message_ids.extend(batch_ids);
            state.next_batch_offset = batch_idx + 1;

            let cp = state.clone().into_checkpoint();
            if let Some(sink) = ctx.checkpoint_sink {
                sink.save_checkpoint(&cp).await?;
            }
        }

        let final_cp = state.clone().into_checkpoint();
        Ok(DeliveryOutcome::Delivered {
            status_code: Some(200),
            checkpoint: Some(final_cp),
            diagnostic: Some(format!(
                "Telegram media_mode delivered {} message(s) across {} batch(es)",
                state.sent_message_ids.len(),
                batches.len()
            )),
            updated_capabilities: None,
        })
    }

    async fn send_single_media_batch(
        &self,
        client: &reqwest::Client,
        cfg: &TelegramChannelConfig,
        ctx: &DeliveryContext<'_>,
        batch_idx: usize,
        batch: &[MediaAttachment],
        base_caption: Option<&str>,
    ) -> Result<Vec<i64>, TgCallError> {
        let mut eligible: Vec<&MediaAttachment> = Vec::with_capacity(batch.len());
        let mut degraded_urls: Vec<String> = Vec::new();

        for item in batch {
            let trimmed_url = item.url.trim();
            let valid_scheme =
                trimmed_url.starts_with("http://") || trimmed_url.starts_with("https://");
            let max_bytes = max_bytes_for_kind(item.kind);
            let declared_over_cap = item.length_bytes.is_some_and(|len| len > max_bytes as u64);

            if !valid_scheme || declared_over_cap {
                degraded_urls.push(trimmed_url.to_owned());
            } else {
                eligible.push(item);
            }
        }

        if eligible.is_empty() {
            let text = compose_degraded_text_message(
                ctx.feed,
                ctx.entry,
                batch_idx,
                &degraded_urls,
                self.locale,
            );
            let id = self
                .send_plain_text_fallback(client, cfg, ctx, &text)
                .await?;
            return Ok(vec![id]);
        }

        // Step 1: For photo/audio/explicit-document batches, try passing external URLs directly to
        // Telegram first. Videos and extensionless/ambiguous attachments skip Step 1 and go straight
        // to Step 2 so we can magic-byte sniff, normalize `.mp4` + `video/mp4`, and attach `width`,
        // `height`, `duration`, and `supports_streaming: true` (preventing Telegram from silently
        // downgrading video URLs to unpreviewable `Document` files).
        if eligible.iter().all(|item| can_try_direct_url_pass(item)) {
            let (initial_caption, overflow_urls, next_media_idx) = append_degraded_links_to_caption(
                base_caption,
                &degraded_urls,
                ctx.entry,
                self.locale,
            );

            match self
                .send_eligible_by_url(client, cfg, &eligible, initial_caption.as_deref())
                .await
            {
                Ok(mut ids) => {
                    if !overflow_urls.is_empty() {
                        let overflow_text =
                            format_overflow_links(&overflow_urls, next_media_idx, self.locale);
                        let extra_id = self
                            .send_plain_text_fallback(client, cfg, ctx, &overflow_text)
                            .await?;
                        ids.push(extra_id);
                    }
                    return Ok(ids);
                }
                Err(err) if err.is_url_media_fetch_error() => {
                    // Fall through to streaming multipart upload below!
                }
                Err(err) => return Err(err),
            }
        }

        // Step 2: Stream attachments into memory under semaphore & byte caps, sniff magic bytes,
        // extract MP4 dimensions/duration, and upload via `multipart/form-data`.
        let mut uploaded: Vec<PreparedUploadItem> = Vec::with_capacity(eligible.len());

        for item in eligible {
            let fetch_cap = max_bytes_for_kind(item.kind);
            let _permit = self
                .upload_semaphore
                .acquire()
                .await
                .expect("media upload semaphore is never closed");

            match self
                .media_fetcher
                .fetch_media(item.url.trim(), fetch_cap, ctx.channel.use_proxy)
                .await
            {
                Ok(fetched) if !fetched.bytes.is_empty() && fetched.bytes.len() <= fetch_cap => {
                    let prepared = normalize_and_inspect_media(item, fetched);
                    let final_cap = max_bytes_for_kind(prepared.kind);
                    if prepared.bytes.len() <= final_cap {
                        uploaded.push(prepared);
                    } else {
                        degraded_urls.push(item.url.trim().to_owned());
                    }
                }
                Ok(_) | Err(_) => {
                    degraded_urls.push(item.url.trim().to_owned());
                }
            }
        }

        if uploaded.is_empty() {
            let text = compose_degraded_text_message(
                ctx.feed,
                ctx.entry,
                batch_idx,
                &degraded_urls,
                self.locale,
            );
            let id = self
                .send_plain_text_fallback(client, cfg, ctx, &text)
                .await?;
            return Ok(vec![id]);
        }

        let (multipart_caption, multipart_overflow, next_media_idx) =
            append_degraded_links_to_caption(base_caption, &degraded_urls, ctx.entry, self.locale);

        let mut ids = self
            .send_uploaded_multipart(client, cfg, &uploaded, multipart_caption.as_deref())
            .await?;

        if !multipart_overflow.is_empty() {
            let overflow_text =
                format_overflow_links(&multipart_overflow, next_media_idx, self.locale);
            let extra_id = self
                .send_plain_text_fallback(client, cfg, ctx, &overflow_text)
                .await?;
            ids.push(extra_id);
        }

        Ok(ids)
    }

    async fn send_eligible_by_url(
        &self,
        client: &reqwest::Client,
        cfg: &TelegramChannelConfig,
        items: &[&MediaAttachment],
        caption: Option<&str>,
    ) -> Result<Vec<i64>, TgCallError> {
        let mut all_ids = Vec::with_capacity(items.len());
        let sub_groups = partition_compatible_media(items);

        for (group_idx, group) in sub_groups.into_iter().enumerate() {
            let group_caption = if group_idx == 0 { caption } else { None };
            if group.len() == 1 {
                let item = group[0];
                let (method, field_name) = single_media_method_and_field(item.kind);
                let mut payload = json!({
                    "chat_id": cfg.chat_id.trim(),
                    field_name: item.url.trim(),
                });
                if item.kind == MediaAttachmentKind::Video {
                    payload["supports_streaming"] = json!(true);
                }
                if let Some(thread_id) = cfg.message_thread_id {
                    payload["message_thread_id"] = json!(thread_id);
                }
                if let Some(cap) = group_caption.filter(|s| !s.is_empty()) {
                    payload["caption"] = json!(cap);
                    payload["parse_mode"] = json!("HTML");
                }
                let id = self
                    .call_json_single_message(client, cfg, method, &payload)
                    .await?;
                all_ids.push(id);
            } else {
                let mut media_arr = Vec::with_capacity(group.len());
                for (idx, item) in group.iter().enumerate() {
                    let mut entry_obj = json!({
                        "type": media_group_type_str(item.kind),
                        "media": item.url.trim(),
                    });
                    if item.kind == MediaAttachmentKind::Video {
                        entry_obj["supports_streaming"] = json!(true);
                    }
                    if idx == 0
                        && let Some(cap) = group_caption.filter(|s| !s.is_empty())
                    {
                        entry_obj["caption"] = json!(cap);
                        entry_obj["parse_mode"] = json!("HTML");
                    }
                    media_arr.push(entry_obj);
                }

                let mut payload = json!({
                    "chat_id": cfg.chat_id.trim(),
                    "media": media_arr,
                });
                if let Some(thread_id) = cfg.message_thread_id {
                    payload["message_thread_id"] = json!(thread_id);
                }
                let ids = self.call_json_media_group(client, cfg, &payload).await?;
                all_ids.extend(ids);
            }
        }

        Ok(all_ids)
    }

    async fn send_uploaded_multipart(
        &self,
        client: &reqwest::Client,
        cfg: &TelegramChannelConfig,
        uploaded: &[PreparedUploadItem],
        caption: Option<&str>,
    ) -> Result<Vec<i64>, TgCallError> {
        let mut all_ids = Vec::with_capacity(uploaded.len());
        let sub_groups = partition_compatible_uploaded(uploaded);

        for (group_idx, group) in sub_groups.into_iter().enumerate() {
            let group_caption = if group_idx == 0 { caption } else { None };
            if group.len() == 1 {
                let item = group[0];
                let (method, field_name) = single_media_method_and_field(item.kind);
                let mut form = Form::new().text("chat_id", cfg.chat_id.trim().to_owned());
                if let Some(thread_id) = cfg.message_thread_id {
                    form = form.text("message_thread_id", thread_id.to_string());
                }
                if let Some(cap) = group_caption.filter(|s| !s.is_empty()) {
                    form = form
                        .text("caption", cap.to_owned())
                        .text("parse_mode", "HTML".to_owned());
                }
                if item.kind == MediaAttachmentKind::Video {
                    form = form.text("supports_streaming", "true".to_owned());
                    if let Some(w) = item.video_meta.width {
                        form = form.text("width", w.to_string());
                    }
                    if let Some(h) = item.video_meta.height {
                        form = form.text("height", h.to_string());
                    }
                    if let Some(d) = item.video_meta.duration_secs {
                        form = form.text("duration", d.to_string());
                    }
                }
                let part = build_prepared_multipart_part(item)?;
                form = form.part(field_name, part);

                let id = self
                    .call_multipart_single_message(client, cfg, method, form)
                    .await?;
                all_ids.push(id);
            } else {
                let mut form = Form::new().text("chat_id", cfg.chat_id.trim().to_owned());
                if let Some(thread_id) = cfg.message_thread_id {
                    form = form.text("message_thread_id", thread_id.to_string());
                }

                let mut media_arr = Vec::with_capacity(group.len());
                for (idx, item) in group.iter().enumerate() {
                    let attach_name = format!("file{idx}");
                    let mut media_obj = json!({
                        "type": media_group_type_str(item.kind),
                        "media": format!("attach://{attach_name}"),
                    });
                    if item.kind == MediaAttachmentKind::Video {
                        media_obj["supports_streaming"] = json!(true);
                        if let Some(w) = item.video_meta.width {
                            media_obj["width"] = json!(w);
                        }
                        if let Some(h) = item.video_meta.height {
                            media_obj["height"] = json!(h);
                        }
                        if let Some(d) = item.video_meta.duration_secs {
                            media_obj["duration"] = json!(d);
                        }
                    }
                    if idx == 0
                        && let Some(cap) = group_caption.filter(|s| !s.is_empty())
                    {
                        media_obj["caption"] = json!(cap);
                        media_obj["parse_mode"] = json!("HTML");
                    }
                    media_arr.push(media_obj);

                    let part = build_prepared_multipart_part(item)?;
                    form = form.part(attach_name, part);
                }

                let media_json = serde_json::to_string(&media_arr).map_err(|err| TgCallError {
                    status_code: None,
                    description: format!("failed to serialize sendMediaGroup JSON: {err}"),
                    retry_after_secs: None,
                    network_error: false,
                })?;
                form = form.text("media", media_json);

                let ids = self.call_multipart_media_group(client, cfg, form).await?;
                all_ids.extend(ids);
            }
        }

        Ok(all_ids)
    }

    async fn send_plain_text_fallback(
        &self,
        client: &reqwest::Client,
        cfg: &TelegramChannelConfig,
        ctx: &DeliveryContext<'_>,
        text: &str,
    ) -> Result<i64, TgCallError> {
        let mut link_preview_options = json!({
            "is_disabled": !ctx.route_options.link_preview,
        });
        if let Some(url) = ctx
            .entry
            .canonical_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            link_preview_options["url"] = json!(url);
        }

        let mut payload = json!({
            "chat_id": cfg.chat_id.trim(),
            "text": text,
            "parse_mode": "HTML",
            "link_preview_options": link_preview_options,
        });
        if let Some(thread_id) = cfg.message_thread_id {
            payload["message_thread_id"] = json!(thread_id);
        }

        self.call_json_single_message(client, cfg, "sendMessage", &payload)
            .await
    }

    async fn call_json_single_message(
        &self,
        client: &reqwest::Client,
        cfg: &TelegramChannelConfig,
        method: &str,
        payload: &Value,
    ) -> Result<i64, TgCallError> {
        let url = api_method_url(cfg, method);
        let resp = client
            .post(&url)
            .json(payload)
            .send()
            .await
            .map_err(TgCallError::from_reqwest)?;
        parse_single_message_response(method, resp).await
    }

    async fn call_json_media_group(
        &self,
        client: &reqwest::Client,
        cfg: &TelegramChannelConfig,
        payload: &Value,
    ) -> Result<Vec<i64>, TgCallError> {
        let url = api_method_url(cfg, "sendMediaGroup");
        let resp = client
            .post(&url)
            .json(payload)
            .send()
            .await
            .map_err(TgCallError::from_reqwest)?;
        parse_media_group_response(resp).await
    }

    async fn call_multipart_single_message(
        &self,
        client: &reqwest::Client,
        cfg: &TelegramChannelConfig,
        method: &str,
        form: Form,
    ) -> Result<i64, TgCallError> {
        let url = api_method_url(cfg, method);
        let resp = client
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(TgCallError::from_reqwest)?;
        parse_single_message_response(method, resp).await
    }

    async fn call_multipart_media_group(
        &self,
        client: &reqwest::Client,
        cfg: &TelegramChannelConfig,
        form: Form,
    ) -> Result<Vec<i64>, TgCallError> {
        let url = api_method_url(cfg, "sendMediaGroup");
        let resp = client
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(TgCallError::from_reqwest)?;
        parse_media_group_response(resp).await
    }
}

#[async_trait]
impl ChannelSender for TelegramSender {
    fn kind(&self) -> ChannelKind {
        ChannelKind::Telegram
    }

    async fn send(&self, ctx: &DeliveryContext<'_>) -> AppResult<DeliveryOutcome> {
        let ChannelConfig::Telegram(cfg) = ctx.channel_config else {
            return Err(AppError::Validation(format!(
                "TelegramSender received incompatible channel config kind '{}'",
                ctx.channel_config.kind()
            )));
        };

        match ctx.route_options.telegram_mode {
            TelegramDeliveryMode::RichMessage => self.send_rich_mode(ctx, cfg).await,
            TelegramDeliveryMode::CompactCard => {
                self.send_compact_mode(ctx, cfg, false, None).await
            }
            TelegramDeliveryMode::MediaMode => self.send_media_mode(ctx, cfg).await,
        }
    }

    async fn send_test(&self, config: &ChannelConfig, use_proxy: bool) -> AppResult<String> {
        let ChannelConfig::Telegram(cfg) = config else {
            return Err(AppError::Validation(format!(
                "TelegramSender::send_test expected telegram config, got '{}'",
                config.kind()
            )));
        };
        config.validate()?;

        let caps = self.probe_capabilities(cfg, use_proxy).await?;
        let client = self.bot_api_client(use_proxy)?;
        let bot_label = caps
            .bot_username
            .as_deref()
            .map(|u| format!("@{u}"))
            .unwrap_or_else(|| "bot".to_owned());

        let locale = self.locale;
        if caps.supports_rich_message {
            let repo_btn_label = if locale.is_zh() {
                "FeedCourier 源码仓库"
            } else {
                "FeedCourier Repository"
            };
            let fit = FitOptions {
                tail: vec![Block::link_button(repo_btn_label, crate::config::REPO_URL)],
                ..FitOptions::default()
            };
            let rich_body = if locale.is_zh() {
                format!(
                    "<h1>FeedCourier 已连接</h1><p>已通过 <b>{}</b> 验证向会话 <code>{}</code> 的 Telegram 富文本消息投递。</p>",
                    escape_html(&bot_label),
                    escape_html(cfg.chat_id.trim())
                )
            } else {
                format!(
                    "<h1>FeedCourier Connected</h1><p>Verified Telegram Bot API 10.4 rich message delivery via <b>{}</b> to chat <code>{}</code>.</p>",
                    escape_html(&bot_label),
                    escape_html(cfg.chat_id.trim())
                )
            };
            let rich =
                normalize(&rich_body, &ParseOptions::default(), &fit).into_input_rich_message();

            let mut payload = json!({
                "chat_id": cfg.chat_id.trim(),
                "rich_message": rich,
            });
            if let Some(thread_id) = cfg.message_thread_id {
                payload["message_thread_id"] = json!(thread_id);
            }

            if let Ok(msg_id) = self
                .call_json_single_message(&client, cfg, "sendRichMessage", &payload)
                .await
            {
                return Ok(format!(
                    "Telegram rich test message delivered (id={msg_id}) via {bot_label}; sendRichMessage supported"
                ));
            }
        }

        let compact_test_text = if locale.is_zh() {
            format!(
                "<b>FeedCourier 已连接</b>\n已通过 <b>{}</b> 验证向会话 <code>{}</code> 的 Telegram 消息投递。\n<i>模式：快讯卡片（富文本支持：{}）</i>",
                escape_html(&bot_label),
                escape_html(cfg.chat_id.trim()),
                caps.supports_rich_message
            )
        } else {
            format!(
                "<b>FeedCourier Connected</b>\nVerified Telegram delivery via <b>{}</b> to chat <code>{}</code>.\n<i>Mode: Compact Card (sendRichMessage supported: {})</i>",
                escape_html(&bot_label),
                escape_html(cfg.chat_id.trim()),
                caps.supports_rich_message
            )
        };

        let mut payload = json!({
            "chat_id": cfg.chat_id.trim(),
            "text": compact_test_text,
            "parse_mode": "HTML",
        });
        if let Some(thread_id) = cfg.message_thread_id {
            payload["message_thread_id"] = json!(thread_id);
        }

        let msg_id = self
            .call_json_single_message(&client, cfg, "sendMessage", &payload)
            .await
            .map_err(|err| err.into_app_error())?;

        Ok(format!(
            "Telegram test message delivered (id={msg_id}) via {bot_label} (sendRichMessage={})",
            caps.supports_rich_message
        ))
    }
}

/// Build a Bot API 10.4 [`InputRichMessage`] using `telegram-rich` normalization and limits fitting.
pub fn build_rich_message_payload(feed: &Feed, entry: &Entry) -> InputRichMessage {
    build_rich_message_payload_with_locale(feed, entry, AppLocale::En)
}

/// Build a Bot API 10.4 [`InputRichMessage`] localized to `locale`.
pub fn build_rich_message_payload_with_locale(
    feed: &Feed,
    entry: &Entry,
    locale: AppLocale,
) -> InputRichMessage {
    let default_title = if locale.is_zh() {
        "无标题"
    } else {
        "Untitled"
    };
    let title = entry
        .title
        .as_deref()
        .map(collapse_single_line)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_title.to_owned());

    let canonical_url = entry
        .canonical_url
        .as_deref()
        .map(str::trim)
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"));

    let fallback_url = canonical_url.or_else(|| {
        feed.site_url
            .as_deref()
            .map(str::trim)
            .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
    });

    let source_meta = format_source_author_line(feed, entry);

    let mut combined_html = String::new();
    if let Some(url) = canonical_url {
        combined_html.push_str("<h1><a href=\"");
        combined_html.push_str(&escape_html_attr(url));
        combined_html.push_str("\">");
        combined_html.push_str(&escape_html(&title));
        combined_html.push_str("</a></h1>");
    } else {
        combined_html.push_str("<h1>");
        combined_html.push_str(&escape_html(&title));
        combined_html.push_str("</h1>");
    }

    if !source_meta.is_empty() {
        combined_html.push_str("<p><i>");
        combined_html.push_str(&escape_html(&source_meta));
        combined_html.push_str("</i></p>");
    }

    if let Some(body_html) = entry
        .content_html
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        combined_html.push_str(body_html);
    } else if let Some(summary) = entry
        .summary_text
        .as_deref()
        .or(entry.first_sentence.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        for para in summary
            .split("\n\n")
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            combined_html.push_str("<p>");
            combined_html.push_str(&escape_html(para));
            combined_html.push_str("</p>");
        }
    }

    let parse_opts = match fallback_url.and_then(|u| Url::parse(u).ok()) {
        Some(base) => ParseOptions::with_base_url(base),
        None => ParseOptions::default(),
    };

    let (read_original_label, truncation_text) = if locale.is_zh() {
        ("阅读原文", "文章较长已截断，点击下方按钮阅读全文。")
    } else {
        (
            "Read original",
            "Article truncated — tap below to read the full post.",
        )
    };

    let tail = match fallback_url {
        Some(url) => vec![Block::link_button(read_original_label, url)],
        None => Vec::new(),
    };

    let fit_opts = FitOptions {
        tail,
        truncation_notice: vec![Block::paragraph(truncation_text)],
        ..FitOptions::default()
    };

    normalize(&combined_html, &parse_opts, &fit_opts).into_input_rich_message()
}

/// Format `TelegramDeliveryMode::CompactCard` HTML strictly bounded to `<= 4096` UTF-16 code units.
pub fn format_compact_card_html(feed: &Feed, entry: &Entry) -> String {
    format_compact_card_html_with_locale(feed, entry, AppLocale::En)
}

/// Format `TelegramDeliveryMode::CompactCard` HTML localized to `locale`.
pub fn format_compact_card_html_with_locale(
    feed: &Feed,
    entry: &Entry,
    locale: AppLocale,
) -> String {
    let default_title = if locale.is_zh() {
        "无标题"
    } else {
        "Untitled"
    };
    let raw_title = entry
        .title
        .as_deref()
        .map(collapse_single_line)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_title.to_owned());
    let title = truncate_utf16(&raw_title, 300, "…");

    let raw_meta = format_source_author_line(feed, entry);
    let meta = truncate_utf16(&raw_meta, 200, "…");

    let canonical_url = entry
        .canonical_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty());

    let mut header = format!("<b>{}</b>", escape_html(&title));
    if !meta.is_empty() {
        header.push('\n');
        header.push_str("<i>");
        header.push_str(&escape_html(&meta));
        header.push_str("</i>");
    }

    let read_original_label = if locale.is_zh() {
        "阅读原文"
    } else {
        "Read original"
    };

    let footer = match canonical_url {
        Some(url) => format!(
            "\n\n<a href=\"{}\">{read_original_label}</a>",
            escape_html_attr(url)
        ),
        None => String::new(),
    };

    let raw_summary = extract_plain_summary(entry);
    if raw_summary.is_empty() {
        let combined = format!("{header}{footer}");
        if utf16_len(&combined) <= MAX_MESSAGE_UTF16_UNITS {
            return combined;
        }
        return format!("<b>{}</b>", escape_html(&truncate_utf16(&title, 256, "…")));
    }

    let fixed_units = utf16_len(&header) + 2 + utf16_len(&footer);
    let mut summary_budget = MAX_MESSAGE_UTF16_UNITS.saturating_sub(fixed_units + 16);

    for _ in 0..8 {
        let clipped_summary = truncate_utf16(&raw_summary, summary_budget, "…");
        let escaped_summary = escape_html(&clipped_summary);
        let candidate = if escaped_summary.is_empty() {
            format!("{header}{footer}")
        } else {
            format!("{header}\n\n{escaped_summary}{footer}")
        };
        if utf16_len(&candidate) <= MAX_MESSAGE_UTF16_UNITS
            && candidate.chars().count() <= MAX_MESSAGE_UTF16_UNITS
        {
            return candidate;
        }
        let over = utf16_len(&candidate).saturating_sub(MAX_MESSAGE_UTF16_UNITS);
        summary_budget = summary_budget.saturating_sub(over + 32);
        if summary_budget == 0 {
            break;
        }
    }

    format!("{header}{footer}")
}

/// Format `TelegramDeliveryMode::MediaMode` strict 3-line minimalist HTML caption (`AppLocale::En` default):
/// 1. Author (fallback to `feed.title`), HTML-escaped
/// 2. `entry.first_sentence` without trailing `.` or `。` (fallback to `entry.title`), HTML-escaped
/// 3. `<a href="{entry.canonical_url}">Link</a>` (`链接` when `AppLocale::ZhCn`; omitted when `None`)
pub fn format_media_mode_caption(feed: &Feed, entry: &Entry, max_utf16_units: usize) -> String {
    format_media_mode_caption_with_locale(feed, entry, max_utf16_units, AppLocale::En)
}

/// Format `TelegramDeliveryMode::MediaMode` 3-line minimalist HTML caption localized to `locale`.
pub fn format_media_mode_caption_with_locale(
    feed: &Feed,
    entry: &Entry,
    max_utf16_units: usize,
    locale: AppLocale,
) -> String {
    let line1_raw = entry
        .author
        .as_deref()
        .map(collapse_single_line)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let ft = collapse_single_line(&feed.title);
            (!ft.is_empty()).then_some(ft)
        })
        .unwrap_or_else(|| "Feed".to_owned());
    let line1_plain = truncate_utf16(&line1_raw, 160.min(max_utf16_units / 3), "…");
    let line1 = escape_html(&line1_plain);

    let sentence_source = entry
        .first_sentence
        .as_deref()
        .map(collapse_single_line)
        .map(|s| strip_trailing_sentence_period(&s))
        .filter(|s| !s.is_empty())
        .or_else(|| {
            entry
                .title
                .as_deref()
                .map(collapse_single_line)
                .map(|t| strip_trailing_sentence_period(&t))
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "Update".to_owned());

    let canonical_url = entry
        .canonical_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let link_label = if locale.is_zh() { "链接" } else { "Link" };
    let line3_html =
        canonical_url.map(|url| format!("<a href=\"{}\">{link_label}</a>", escape_html_attr(url)));

    let visible_reserved = utf16_len(&line1_plain)
        + 1
        + canonical_url
            .map(|_| 1 + utf16_len(link_label))
            .unwrap_or(0);
    let line2_budget = max_utf16_units.saturating_sub(visible_reserved).max(16);
    let mut line2_plain = truncate_utf16(&sentence_source, line2_budget, "…");
    line2_plain = strip_trailing_sentence_period(&line2_plain);
    let line2 = escape_html(&line2_plain);

    match line3_html {
        Some(link_tag) if visible_reserved <= max_utf16_units => {
            format!("{line1}\n{line2}\n{link_tag}")
        }
        _ => format!("{line1}\n{line2}"),
    }
}

fn strip_trailing_sentence_period(s: &str) -> String {
    s.trim()
        .trim_end_matches(|c: char| matches!(c, '.' | '。') || c.is_whitespace())
        .to_owned()
}

fn max_bytes_for_kind(kind: MediaAttachmentKind) -> usize {
    match kind {
        MediaAttachmentKind::Image => MAX_PHOTO_BYTES,
        MediaAttachmentKind::Video | MediaAttachmentKind::Audio | MediaAttachmentKind::Document => {
            MAX_MEDIA_BYTES
        }
    }
}

fn single_media_method_and_field(kind: MediaAttachmentKind) -> (&'static str, &'static str) {
    match kind {
        MediaAttachmentKind::Image => ("sendPhoto", "photo"),
        MediaAttachmentKind::Video => ("sendVideo", "video"),
        MediaAttachmentKind::Audio => ("sendAudio", "audio"),
        MediaAttachmentKind::Document => ("sendDocument", "document"),
    }
}

fn media_group_type_str(kind: MediaAttachmentKind) -> &'static str {
    match kind {
        MediaAttachmentKind::Image => "photo",
        MediaAttachmentKind::Video => "video",
        MediaAttachmentKind::Audio => "audio",
        MediaAttachmentKind::Document => "document",
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MediaGroupBucket {
    Visual,
    Audio,
    Document,
}

fn bucket_for(kind: MediaAttachmentKind) -> MediaGroupBucket {
    match kind {
        MediaAttachmentKind::Image | MediaAttachmentKind::Video => MediaGroupBucket::Visual,
        MediaAttachmentKind::Audio => MediaGroupBucket::Audio,
        MediaAttachmentKind::Document => MediaGroupBucket::Document,
    }
}

fn partition_compatible_media<'a>(items: &[&'a MediaAttachment]) -> Vec<Vec<&'a MediaAttachment>> {
    let mut groups: Vec<Vec<&'a MediaAttachment>> = Vec::new();
    for &item in items {
        let b = bucket_for(item.kind);
        if let Some(last) = groups.last_mut()
            && bucket_for(last[0].kind) == b
        {
            last.push(item);
        } else {
            groups.push(vec![item]);
        }
    }
    groups
}

#[derive(Debug, Clone)]
struct PreparedUploadItem {
    kind: MediaAttachmentKind,
    bytes: Bytes,
    mime_type: String,
    filename: String,
    video_meta: VideoMetadata,
}

fn can_try_direct_url_pass(item: &MediaAttachment) -> bool {
    match item.kind {
        // Videos must go through our stream fetcher + MP4 metadata parser + multipart upload:
        // Telegram's URL mode silently downgrades videos to unpreviewable `Document` files (with
        // HTTP 200 OK) whenever the origin CDN omits a `.mp4` extension, serves `octet-stream`,
        // or lacks streaming/dimension hints.
        MediaAttachmentKind::Video => false,
        MediaAttachmentKind::Image | MediaAttachmentKind::Audio => true,
        // Extensionless or generic `<enclosure>` URLs are classified as `Document` at ingest;
        // only use direct URL mode when the attachment is explicitly a known document type.
        MediaAttachmentKind::Document => is_explicit_document_attachment(item),
    }
}

fn is_explicit_document_attachment(item: &MediaAttachment) -> bool {
    if let Some(mime) = item.mime_type.as_deref().map(str::trim) {
        let lower = mime.to_ascii_lowercase();
        if lower.starts_with("application/pdf")
            || lower.starts_with("application/zip")
            || lower.starts_with("application/x-rar")
            || lower.starts_with("application/x-7z")
            || lower.starts_with("application/gzip")
            || lower.starts_with("application/x-tar")
            || lower.starts_with("application/msword")
            || lower.starts_with("application/vnd.")
            || lower.starts_with("application/epub+zip")
            || lower.starts_with("application/rtf")
            || lower.starts_with("text/")
        {
            return true;
        }
    }

    let path = Url::parse(&item.url)
        .ok()
        .map(|u| u.path().to_ascii_lowercase())
        .unwrap_or_else(|| item.url.to_ascii_lowercase());

    [
        ".pdf", ".zip", ".rar", ".7z", ".gz", ".tar", ".tgz", ".bz2", ".xz", ".doc", ".docx",
        ".xls", ".xlsx", ".ppt", ".pptx", ".txt", ".csv", ".tsv", ".epub", ".rtf", ".json", ".xml",
    ]
    .iter()
    .any(|ext| path.ends_with(ext))
}

fn normalize_and_inspect_media(
    item: &MediaAttachment,
    fetched: FetchedMedia,
) -> PreparedUploadItem {
    let sniffed = sniff_media_bytes(&fetched.bytes);

    let kind = match (item.kind, sniffed) {
        (MediaAttachmentKind::Document, Some((detected_kind, _, _))) => detected_kind,
        (MediaAttachmentKind::Image, Some((MediaAttachmentKind::Video, _, _))) => {
            MediaAttachmentKind::Video
        }
        (original, _) => original,
    };

    let (mime_type, canonical_ext) = match kind {
        MediaAttachmentKind::Video => {
            if let Some((MediaAttachmentKind::Video, m, ext)) = sniffed {
                (m.to_owned(), Some(ext))
            } else if fetched
                .content_type
                .as_deref()
                .or(item.mime_type.as_deref())
                .is_some_and(|m| m.eq_ignore_ascii_case("video/webm"))
            {
                ("video/webm".to_owned(), Some("webm"))
            } else {
                // Always normalize ISO-BMFF / QuickTime / generic video streams to `video/mp4` + `.mp4`
                // so Telegram renders an inline streaming player instead of a raw file attachment.
                ("video/mp4".to_owned(), Some("mp4"))
            }
        }
        MediaAttachmentKind::Image => {
            if let Some((MediaAttachmentKind::Image, m, ext)) = sniffed {
                (m.to_owned(), Some(ext))
            } else {
                match fetched
                    .content_type
                    .as_deref()
                    .or(item.mime_type.as_deref())
                {
                    Some("image/png") => ("image/png".to_owned(), Some("png")),
                    Some("image/webp") => ("image/webp".to_owned(), Some("webp")),
                    Some("image/gif") => ("image/gif".to_owned(), Some("gif")),
                    _ => ("image/jpeg".to_owned(), Some("jpg")),
                }
            }
        }
        MediaAttachmentKind::Audio => {
            if let Some((MediaAttachmentKind::Audio, m, ext)) = sniffed {
                (m.to_owned(), Some(ext))
            } else {
                match fetched
                    .content_type
                    .as_deref()
                    .or(item.mime_type.as_deref())
                {
                    Some("audio/ogg") => ("audio/ogg".to_owned(), Some("ogg")),
                    Some("audio/mp4" | "audio/x-m4a") => ("audio/mp4".to_owned(), Some("m4a")),
                    _ => ("audio/mpeg".to_owned(), Some("mp3")),
                }
            }
        }
        MediaAttachmentKind::Document => {
            if let Some((MediaAttachmentKind::Document, m, ext)) = sniffed {
                (m.to_owned(), Some(ext))
            } else {
                let m = fetched
                    .content_type
                    .as_deref()
                    .or(item.mime_type.as_deref())
                    .unwrap_or("application/octet-stream")
                    .to_owned();
                (m, None)
            }
        }
    };

    let filename = match canonical_ext {
        Some(ext) => ensure_filename_extension(&fetched.filename, ext),
        None => fetched.filename,
    };

    let video_meta = if kind == MediaAttachmentKind::Video {
        parse_mp4_metadata(&fetched.bytes)
    } else {
        VideoMetadata::default()
    };

    PreparedUploadItem {
        kind,
        bytes: fetched.bytes,
        mime_type,
        filename,
        video_meta,
    }
}

fn ensure_filename_extension(raw_filename: &str, canonical_ext: &str) -> String {
    let expected_suffix = format!(".{canonical_ext}");
    if raw_filename
        .to_ascii_lowercase()
        .ends_with(&expected_suffix)
    {
        return raw_filename.to_owned();
    }

    let stem = raw_filename
        .rsplit_once('.')
        .map(|(base, _)| base)
        .filter(|base| !base.is_empty())
        .unwrap_or(raw_filename);
    let clean_stem = if stem.is_empty() { "media" } else { stem };
    format!("{clean_stem}.{canonical_ext}")
}

/// Inspect magic bytes at the start of a downloaded media buffer to detect actual container format.
pub fn sniff_media_bytes(
    bytes: &[u8],
) -> Option<(MediaAttachmentKind, &'static str, &'static str)> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some((MediaAttachmentKind::Image, "image/jpeg", "jpg"));
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some((MediaAttachmentKind::Image, "image/png", "png"));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some((MediaAttachmentKind::Image, "image/gif", "gif"));
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some((MediaAttachmentKind::Image, "image/webp", "webp"));
    }

    // ISO Base Media File Format (`ftyp` box at offset 4..8): MP4, M4V, MOV, M4A, 3GP
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        let major_brand = &bytes[8..12];
        if matches!(major_brand, b"M4A " | b"M4B ") {
            return Some((MediaAttachmentKind::Audio, "audio/mp4", "m4a"));
        }
        if !matches!(major_brand, b"avif" | b"avis" | b"heic" | b"heix" | b"mif1") {
            return Some((MediaAttachmentKind::Video, "video/mp4", "mp4"));
        }
    }
    if bytes.len() >= 8 && matches!(&bytes[4..8], b"moov" | b"wide") {
        let meta = parse_mp4_metadata(bytes);
        if meta.width.is_some() || meta.duration_secs.is_some() {
            return Some((MediaAttachmentKind::Video, "video/mp4", "mp4"));
        }
    }

    // WebM / Matroska EBML header
    if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Some((MediaAttachmentKind::Video, "video/webm", "webm"));
    }

    // MP3 (ID3v2 tag or MPEG ADTS sync word)
    if bytes.starts_with(b"ID3")
        || (bytes.len() >= 2 && bytes[0] == 0xFF && (bytes[1] & 0xE0) == 0xE0)
    {
        return Some((MediaAttachmentKind::Audio, "audio/mpeg", "mp3"));
    }
    if bytes.starts_with(b"OggS") {
        return Some((MediaAttachmentKind::Audio, "audio/ogg", "ogg"));
    }
    if bytes.starts_with(b"%PDF-") {
        return Some((MediaAttachmentKind::Document, "application/pdf", "pdf"));
    }

    None
}

/// Lightweight, zero-dependency ISO-BMFF (`MP4` / `MOV` / `M4V`) parser that extracts
/// display `width`, `height`, and `duration_secs` from `moov -> mvhd / tkhd / stsd`.
pub fn parse_mp4_metadata(bytes: &[u8]) -> VideoMetadata {
    let mut meta = VideoMetadata::default();
    let Some(moov_payload) = find_mp4_box_payload(bytes, b"moov") else {
        return meta;
    };

    if let Some(mvhd) = find_mp4_box_payload(moov_payload, b"mvhd") {
        meta.duration_secs = parse_mvhd_or_mdhd_duration(mvhd);
    }

    for (box_type, trak_payload) in Mp4BoxIter::new(moov_payload) {
        if &box_type != b"trak" {
            continue;
        }

        if meta.duration_secs.is_none()
            && let Some(mdia) = find_mp4_box_payload(trak_payload, b"mdia")
            && let Some(mdhd) = find_mp4_box_payload(mdia, b"mdhd")
        {
            meta.duration_secs = parse_mvhd_or_mdhd_duration(mdhd);
        }

        if meta.width.is_none() || meta.height.is_none() {
            let mut tkhd_dims: Option<(u32, u32, bool)> = None;
            if let Some(tkhd) = find_mp4_box_payload(trak_payload, b"tkhd") {
                tkhd_dims = parse_tkhd_dimensions(tkhd);
            }

            let rotated_90_or_270 = tkhd_dims.map(|(_, _, r)| r).unwrap_or(false);
            if let Some((w, h, _)) = tkhd_dims
                && w > 0
                && h > 0
            {
                meta.width = Some(w);
                meta.height = Some(h);
                continue;
            }

            if let Some(mdia) = find_mp4_box_payload(trak_payload, b"mdia")
                && let Some(minf) = find_mp4_box_payload(mdia, b"minf")
                && let Some(stbl) = find_mp4_box_payload(minf, b"stbl")
                && let Some(stsd) = find_mp4_box_payload(stbl, b"stsd")
                && let Some((w, h)) = parse_stsd_dimensions(stsd)
            {
                let (final_w, final_h) = if rotated_90_or_270 { (h, w) } else { (w, h) };
                meta.width = Some(final_w);
                meta.height = Some(final_h);
            }
        }
    }

    meta
}

struct Mp4BoxIter<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Mp4BoxIter<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }
}

impl<'a> Iterator for Mp4BoxIter<'a> {
    type Item = ([u8; 4], &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset.checked_add(8)? > self.data.len() {
            return None;
        }
        let size32 =
            u32::from_be_bytes(self.data[self.offset..self.offset + 4].try_into().ok()?) as usize;
        let box_type: [u8; 4] = self.data[self.offset + 4..self.offset + 8]
            .try_into()
            .ok()?;

        let (header_len, total_size) = if size32 == 1 {
            if self.offset.checked_add(16)? > self.data.len() {
                return None;
            }
            let size64 = u64::from_be_bytes(
                self.data[self.offset + 8..self.offset + 16]
                    .try_into()
                    .ok()?,
            );
            let total = usize::try_from(size64).ok()?;
            if total < 16 {
                return None;
            }
            (16usize, total)
        } else if size32 == 0 {
            (8usize, self.data.len() - self.offset)
        } else if size32 >= 8 {
            (8usize, size32)
        } else {
            return None;
        };

        let end = self.offset.checked_add(total_size)?;
        if end > self.data.len() {
            self.offset = self.data.len();
            return None;
        }
        let payload = &self.data[self.offset + header_len..end];
        self.offset = end;
        Some((box_type, payload))
    }
}

fn find_mp4_box_payload<'a>(data: &'a [u8], target: &[u8; 4]) -> Option<&'a [u8]> {
    for (box_type, payload) in Mp4BoxIter::new(data) {
        if &box_type == target {
            return Some(payload);
        }
    }
    None
}

fn parse_mvhd_or_mdhd_duration(payload: &[u8]) -> Option<u32> {
    let version = *payload.first()?;
    let (timescale, duration) = if version == 0 {
        if payload.len() < 20 {
            return None;
        }
        let ts = u32::from_be_bytes(payload[12..16].try_into().ok()?);
        let dur = u32::from_be_bytes(payload[16..20].try_into().ok()?);
        if dur == u32::MAX {
            return None;
        }
        (ts, u64::from(dur))
    } else if version == 1 {
        if payload.len() < 32 {
            return None;
        }
        let ts = u32::from_be_bytes(payload[20..24].try_into().ok()?);
        let dur = u64::from_be_bytes(payload[24..32].try_into().ok()?);
        if dur == u64::MAX {
            return None;
        }
        (ts, dur)
    } else {
        return None;
    };

    if timescale == 0 || duration == 0 {
        return None;
    }

    let secs = ((duration as f64) / (timescale as f64)).round();
    if !secs.is_finite() || secs <= 0.0 {
        return None;
    }
    u32::try_from(secs as u64).ok().map(|s| s.max(1))
}

fn parse_tkhd_dimensions(payload: &[u8]) -> Option<(u32, u32, bool)> {
    let version = *payload.first()?;
    let (matrix_offset, dims_offset) = match version {
        0 if payload.len() >= 84 => (40usize, 76usize),
        1 if payload.len() >= 96 => (52usize, 88usize),
        _ => return None,
    };

    let a = i32::from_be_bytes(payload[matrix_offset..matrix_offset + 4].try_into().ok()?);
    let b = i32::from_be_bytes(
        payload[matrix_offset + 4..matrix_offset + 8]
            .try_into()
            .ok()?,
    );
    let c = i32::from_be_bytes(
        payload[matrix_offset + 12..matrix_offset + 16]
            .try_into()
            .ok()?,
    );
    let d = i32::from_be_bytes(
        payload[matrix_offset + 16..matrix_offset + 20]
            .try_into()
            .ok()?,
    );

    let rotated_90_or_270 = a == 0 && d == 0 && (b != 0 || c != 0);

    let width_fixed = u32::from_be_bytes(payload[dims_offset..dims_offset + 4].try_into().ok()?);
    let height_fixed =
        u32::from_be_bytes(payload[dims_offset + 4..dims_offset + 8].try_into().ok()?);
    let width = width_fixed >> 16;
    let height = height_fixed >> 16;

    let (final_w, final_h) = if rotated_90_or_270 {
        (height, width)
    } else {
        (width, height)
    };
    Some((final_w, final_h, rotated_90_or_270))
}

fn parse_stsd_dimensions(payload: &[u8]) -> Option<(u32, u32)> {
    if payload.len() < 16 {
        return None;
    }
    for (entry_type, entry_payload) in Mp4BoxIter::new(&payload[8..]) {
        if matches!(
            &entry_type,
            b"avc1" | b"avc3" | b"hvc1" | b"hev1" | b"vp08" | b"vp09" | b"av01" | b"mp4v"
        ) && entry_payload.len() >= 28
        {
            let w = u32::from(u16::from_be_bytes(entry_payload[24..26].try_into().ok()?));
            let h = u32::from(u16::from_be_bytes(entry_payload[26..28].try_into().ok()?));
            if w > 0 && h > 0 {
                return Some((w, h));
            }
        }
    }
    None
}

fn partition_compatible_uploaded<'a>(
    items: &'a [PreparedUploadItem],
) -> Vec<Vec<&'a PreparedUploadItem>> {
    let mut groups: Vec<Vec<&'a PreparedUploadItem>> = Vec::new();
    for item in items {
        let b = bucket_for(item.kind);
        if let Some(last) = groups.last_mut()
            && bucket_for(last[0].kind) == b
        {
            last.push(item);
        } else {
            groups.push(vec![item]);
        }
    }
    groups
}

fn build_prepared_multipart_part(item: &PreparedUploadItem) -> Result<Part, TgCallError> {
    let len = item.bytes.len() as u64;
    let part = Part::stream_with_length(reqwest::Body::from(item.bytes.clone()), len)
        .file_name(item.filename.clone());

    let default_mime = match item.kind {
        MediaAttachmentKind::Image => "image/jpeg",
        MediaAttachmentKind::Video => "video/mp4",
        MediaAttachmentKind::Audio => "audio/mpeg",
        MediaAttachmentKind::Document => "application/octet-stream",
    };
    part.mime_str(&item.mime_type)
        .or_else(|_| {
            Part::stream_with_length(reqwest::Body::from(item.bytes.clone()), len)
                .file_name(item.filename.clone())
                .mime_str(default_mime)
        })
        .map_err(|err| TgCallError {
            status_code: None,
            description: format!("invalid multipart MIME type: {err}"),
            retry_after_secs: None,
            network_error: false,
        })
}

fn format_media_hyperlink(url: &str, index_one_based: usize, locale: AppLocale) -> String {
    let label = if locale.is_zh() {
        format!("媒体 {index_one_based}")
    } else {
        format!("Media {index_one_based}")
    };
    format!("<a href=\"{}\">{label}</a>", escape_html_attr(url))
}

fn append_degraded_links_to_caption(
    base_caption: Option<&str>,
    degraded_urls: &[String],
    entry: &Entry,
    locale: AppLocale,
) -> (Option<String>, Vec<String>, usize) {
    let canonical = entry.canonical_url.as_deref().map(str::trim);
    let mut current = base_caption.unwrap_or("").to_owned();
    let mut overflow = Vec::new();
    let mut has_link_line = current.contains("<a href=");
    let mut media_idx = 0usize;
    let mut first_overflow_idx = 1usize;

    for url in degraded_urls {
        if canonical == Some(url.as_str()) && base_caption.is_some() {
            continue;
        }
        media_idx += 1;
        let tag = format_media_hyperlink(url, media_idx, locale);
        let sep = if current.is_empty() {
            ""
        } else if has_link_line {
            " · "
        } else {
            "\n"
        };
        let needed = utf16_len(sep) + utf16_len(&tag);
        if overflow.is_empty() && utf16_len(&current) + needed <= MAX_CAPTION_UTF16_UNITS {
            current.push_str(sep);
            current.push_str(&tag);
            has_link_line = true;
            first_overflow_idx = media_idx + 1;
        } else {
            overflow.push(url.clone());
        }
    }

    let opt = (!current.is_empty()).then_some(current);
    (opt, overflow, first_overflow_idx)
}

fn compose_degraded_text_message(
    feed: &Feed,
    entry: &Entry,
    batch_idx: usize,
    degraded_urls: &[String],
    locale: AppLocale,
) -> String {
    let canonical = entry.canonical_url.as_deref().map(str::trim);
    let mut out = if batch_idx == 0 {
        format_media_mode_caption_with_locale(feed, entry, MAX_MESSAGE_UTF16_UNITS, locale)
    } else {
        String::new()
    };
    let mut has_link_line = out.contains("<a href=");
    let mut media_idx = 0usize;

    for url in degraded_urls {
        if batch_idx == 0 && canonical == Some(url.as_str()) {
            continue;
        }
        media_idx += 1;
        let tag = format_media_hyperlink(url, media_idx, locale);
        let sep = if out.is_empty() {
            ""
        } else if has_link_line {
            " · "
        } else {
            "\n"
        };
        if utf16_len(&out) + utf16_len(sep) + utf16_len(&tag) <= MAX_MESSAGE_UTF16_UNITS {
            out.push_str(sep);
            out.push_str(&tag);
            has_link_line = true;
        }
    }
    out
}

fn format_overflow_links(urls: &[String], start_media_idx: usize, locale: AppLocale) -> String {
    let mut out = String::new();
    for (idx, url) in urls.iter().enumerate() {
        let tag = format_media_hyperlink(url, start_media_idx + idx, locale);
        let sep = if out.is_empty() { "" } else { " · " };
        if utf16_len(&out) + utf16_len(sep) + utf16_len(&tag) <= MAX_MESSAGE_UTF16_UNITS {
            out.push_str(sep);
            out.push_str(&tag);
        }
    }
    out
}

#[derive(Clone)]
struct TgCheckpointState {
    next_batch_offset: usize,
    sent_message_ids: Vec<i64>,
    degraded_to_compact: bool,
}

impl TgCheckpointState {
    fn into_checkpoint(self) -> DeliveryCheckpoint {
        DeliveryCheckpoint::Telegram {
            next_batch_offset: self.next_batch_offset,
            sent_message_ids: self.sent_message_ids,
            degraded_to_compact: self.degraded_to_compact,
        }
    }
}

fn extract_telegram_checkpoint(cp: &Option<DeliveryCheckpoint>) -> TgCheckpointState {
    match cp {
        Some(DeliveryCheckpoint::Telegram {
            next_batch_offset,
            sent_message_ids,
            degraded_to_compact,
        }) => TgCheckpointState {
            next_batch_offset: *next_batch_offset,
            sent_message_ids: sent_message_ids.clone(),
            degraded_to_compact: *degraded_to_compact,
        },
        _ => TgCheckpointState {
            next_batch_offset: 0,
            sent_message_ids: Vec::new(),
            degraded_to_compact: false,
        },
    }
}

#[derive(Debug, Deserialize)]
struct TgApiResponse<T> {
    ok: bool,
    result: Option<T>,
    #[serde(default)]
    error_code: Option<i32>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Option<TgResponseParameters>,
}

#[derive(Debug, Deserialize)]
struct TgResponseParameters {
    #[serde(default)]
    retry_after: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TgUser {
    #[serde(default)]
    username: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TgSentMessage {
    message_id: i64,
}

#[derive(Debug)]
struct TgCallError {
    status_code: Option<i32>,
    description: String,
    retry_after_secs: Option<u64>,
    network_error: bool,
}

impl TgCallError {
    fn from_reqwest(err: reqwest::Error) -> Self {
        Self {
            status_code: err.status().map(|s| i32::from(s.as_u16())),
            description: format!("Telegram network/HTTP error: {err}"),
            retry_after_secs: None,
            network_error: true,
        }
    }

    fn is_rich_method_unsupported(&self) -> bool {
        match self.status_code {
            Some(code) => is_rich_method_unsupported(code, &self.description),
            None => false,
        }
    }

    fn is_url_media_fetch_error(&self) -> bool {
        if self.status_code != Some(400) {
            return false;
        }
        let lower = self.description.to_ascii_lowercase();
        lower.contains("webpage_curl_failed")
            || lower.contains("webpage_media_empty")
            || lower.contains("failed to get http url content")
            || lower.contains("wrong file identifier/http url specified")
            || lower.contains("wrong type of the web page content")
            || lower.contains("failed to send message #")
    }

    fn into_app_error(self) -> AppError {
        match self.status_code {
            Some(401) => AppError::Unauthorized(self.description),
            Some(403) => AppError::Forbidden(self.description),
            Some(429) => AppError::RateLimited {
                message: self.description,
                retry_after_secs: self.retry_after_secs,
            },
            _ => AppError::Upstream(self.description),
        }
    }
}

fn is_rich_method_unsupported(code: i32, description: &str) -> bool {
    if code == 404 || code == 501 {
        return true;
    }
    let lower = description.to_ascii_lowercase();
    lower.contains("method not found")
        || lower.contains("unknown method")
        || lower.contains("method is not available")
        || (code == 400 && lower.contains("not found") && !lower.contains("chat"))
}

async fn parse_single_message_response(
    method: &str,
    resp: reqwest::Response,
) -> Result<i64, TgCallError> {
    let http_status = i32::from(resp.status().as_u16());
    let header_retry = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok());

    let raw_bytes = resp.bytes().await.map_err(TgCallError::from_reqwest)?;
    let parsed: TgApiResponse<TgSentMessage> = match serde_json::from_slice(&raw_bytes) {
        Ok(val) => val,
        Err(err) => {
            let preview = String::from_utf8_lossy(&raw_bytes);
            return Err(TgCallError {
                status_code: Some(http_status),
                description: format!(
                    "Telegram {method} returned non-JSON HTTP {http_status} ({err}): {}",
                    truncate_utf16(&preview, 200, "…")
                ),
                retry_after_secs: header_retry,
                network_error: false,
            });
        }
    };

    if parsed.ok
        && let Some(msg) = parsed.result
    {
        return Ok(msg.message_id);
    }

    let code = parsed.error_code.unwrap_or(http_status);
    let desc = parsed
        .description
        .unwrap_or_else(|| format!("Telegram {method} failed with code {code}"));
    let retry_after = parsed
        .parameters
        .and_then(|p| p.retry_after)
        .or(header_retry)
        .or_else(|| parse_retry_after_from_description(&desc));

    Err(TgCallError {
        status_code: Some(code),
        description: desc,
        retry_after_secs: retry_after,
        network_error: false,
    })
}

async fn parse_media_group_response(resp: reqwest::Response) -> Result<Vec<i64>, TgCallError> {
    let http_status = i32::from(resp.status().as_u16());
    let header_retry = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok());

    let raw_bytes = resp.bytes().await.map_err(TgCallError::from_reqwest)?;
    let parsed: TgApiResponse<Vec<TgSentMessage>> = match serde_json::from_slice(&raw_bytes) {
        Ok(val) => val,
        Err(err) => {
            let preview = String::from_utf8_lossy(&raw_bytes);
            return Err(TgCallError {
                status_code: Some(http_status),
                description: format!(
                    "Telegram sendMediaGroup returned non-JSON HTTP {http_status} ({err}): {}",
                    truncate_utf16(&preview, 200, "…")
                ),
                retry_after_secs: header_retry,
                network_error: false,
            });
        }
    };

    if parsed.ok
        && let Some(msgs) = parsed.result
    {
        return Ok(msgs.into_iter().map(|m| m.message_id).collect());
    }

    let code = parsed.error_code.unwrap_or(http_status);
    let desc = parsed
        .description
        .unwrap_or_else(|| format!("Telegram sendMediaGroup failed with code {code}"));
    let retry_after = parsed
        .parameters
        .and_then(|p| p.retry_after)
        .or(header_retry)
        .or_else(|| parse_retry_after_from_description(&desc));

    Err(TgCallError {
        status_code: Some(code),
        description: desc,
        retry_after_secs: retry_after,
        network_error: false,
    })
}

fn parse_retry_after_from_description(desc: &str) -> Option<u64> {
    let lower = desc.to_ascii_lowercase();
    let rest = lower.split("retry after ").nth(1)?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u64>().ok().filter(|&v| v > 0)
}

fn classify_telegram_error(
    err: TgCallError,
    checkpoint: Option<DeliveryCheckpoint>,
) -> DeliveryOutcome {
    if err.network_error {
        return DeliveryOutcome::Retry {
            retry_after_secs: None,
            status_code: err.status_code,
            checkpoint,
            diagnostic: err.description,
            cooldown_credential_group: false,
        };
    }

    match err.status_code {
        Some(429) => DeliveryOutcome::Retry {
            retry_after_secs: err.retry_after_secs,
            status_code: Some(429),
            checkpoint,
            diagnostic: format!("Telegram 429: {}", err.description),
            cooldown_credential_group: true,
        },
        Some(code @ 500..=599) => DeliveryOutcome::Retry {
            retry_after_secs: err.retry_after_secs,
            status_code: Some(code),
            checkpoint,
            diagnostic: format!("Telegram {code}: {}", err.description),
            cooldown_credential_group: false,
        },
        Some(code @ (401 | 403)) => DeliveryOutcome::PermanentFailure {
            status_code: Some(code),
            checkpoint,
            diagnostic: format!("Telegram {code}: {}", err.description),
        },
        Some(code) => DeliveryOutcome::PermanentFailure {
            status_code: Some(code),
            checkpoint,
            diagnostic: format!("Telegram {code}: {}", err.description),
        },
        None => DeliveryOutcome::Retry {
            retry_after_secs: None,
            status_code: None,
            checkpoint,
            diagnostic: err.description,
            cooldown_credential_group: false,
        },
    }
}

fn api_method_url(cfg: &TelegramChannelConfig, method: &str) -> String {
    let base = cfg
        .api_base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_TELEGRAM_API_BASE_URL)
        .trim_end_matches('/');
    let token = cfg.bot_token.trim();
    format!("{base}/bot{token}/{method}")
}

fn format_source_author_line(feed: &Feed, entry: &Entry) -> String {
    let feed_title = collapse_single_line(&feed.title);
    let author = entry
        .author
        .as_deref()
        .map(collapse_single_line)
        .filter(|a| !a.is_empty());

    match (!feed_title.is_empty(), author) {
        (true, Some(a)) if !feed_title.eq_ignore_ascii_case(&a) => format!("{feed_title} · {a}"),
        (true, _) => feed_title,
        (false, Some(a)) => a,
        (false, None) => String::new(),
    }
}

fn extract_plain_summary(entry: &Entry) -> String {
    if let Some(summary) = entry
        .summary_text
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return collapse_paragraphs(summary);
    }

    if let Some(html) = entry
        .content_html
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        && let Ok(rendered) = html2text::from_read(html.as_bytes(), 120)
    {
        let cleaned = collapse_paragraphs(&rendered);
        if !cleaned.is_empty() {
            return cleaned;
        }
    }

    entry
        .first_sentence
        .as_deref()
        .map(collapse_single_line)
        .unwrap_or_default()
}

fn collapse_single_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for ch in s.chars() {
        if ch.is_control() || ch.is_whitespace() {
            if !prev_ws && !out.is_empty() {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            out.push(ch);
            prev_ws = false;
        }
    }
    out.trim().to_owned()
}

fn collapse_paragraphs(s: &str) -> String {
    s.lines()
        .map(collapse_single_line)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_html_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn truncate_utf16(s: &str, max_units: usize, ellipsis: &str) -> String {
    if utf16_len(s) <= max_units {
        return s.to_owned();
    }
    if max_units == 0 {
        return String::new();
    }
    let ell_units = utf16_len(ellipsis);
    if max_units <= ell_units {
        let mut out = String::new();
        let mut used = 0;
        for ch in s.chars() {
            let w = ch.len_utf16();
            if used + w > max_units {
                break;
            }
            out.push(ch);
            used += w;
        }
        return out;
    }

    let target = max_units - ell_units;
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let w = ch.len_utf16();
        if used + w > target {
            break;
        }
        out.push(ch);
        used += w;
    }
    let trimmed = out.trim_end();
    format!("{trimmed}{ellipsis}")
}

fn derive_filename(url: &Url, content_type: Option<&str>) -> String {
    let ext_from_mime = match content_type {
        Some("image/jpeg" | "image/jpg") => "jpg",
        Some("image/png") => "png",
        Some("image/webp") => "webp",
        Some("image/gif") => "gif",
        Some("video/mp4") => "mp4",
        Some("video/webm") => "webm",
        Some("audio/mpeg" | "audio/mp3") => "mp3",
        Some("audio/ogg") => "ogg",
        Some("audio/mp4" | "audio/x-m4a") => "m4a",
        Some("application/pdf") => "pdf",
        _ => "bin",
    };

    if let Some(seg) = url
        .path_segments()
        .and_then(|mut s| s.rfind(|part| !part.is_empty()))
    {
        let sanitized: String = seg
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            .take(64)
            .collect();
        let trimmed = sanitized.trim_matches(['.', '-', '_']);
        if !trimmed.is_empty() {
            if trimmed.contains('.') {
                return trimmed.to_owned();
            }
            return format!("{trimmed}.{ext_from_mime}");
        }
    }

    format!("media.{ext_from_mime}")
}

fn check_ip_allowed(ip: IpAddr, allow_private_for_tests: bool) -> AppResult<()> {
    let canonical = match ip {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(mapped) => IpAddr::V4(mapped),
            None => IpAddr::V6(v6),
        },
    };

    // Cloud metadata endpoint `169.254.169.254` is NEVER permitted via generic test flags.
    if canonical == IpAddr::from([169, 254, 169, 254]) {
        return Err(AppError::SsrfBlocked(
            "cloud metadata IP 169.254.169.254 is blocked".to_owned(),
        ));
    }

    if allow_private_for_tests {
        return Ok(());
    }

    let blocked = match canonical {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                // CGNAT 100.64.0.0/10
                || (octets[0] == 100 && (octets[1] & 0b1100_0000) == 64)
                // Benchmarking 198.18.0.0/15
                || (octets[0] == 198 && (octets[1] == 18 || octets[1] == 19))
                // Reserved 240.0.0.0/4
                || octets[0] >= 240
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // Unique Local Address fc00::/7
                || (segments[0] & 0xfe00) == 0xfc00
                // Link-Local fe80::/10
                || (segments[0] & 0xffc0) == 0xfe80
                // Documentation 2001:db8::/32
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        }
    };

    if blocked {
        return Err(AppError::SsrfBlocked(format!(
            "resolved non-public IP address {ip}"
        )));
    }

    Ok(())
}
