//! Public unauthenticated SMTP compliance endpoints (`/u/{token}` and `/v/{token}`).
//!
//! Security & RFC 8058 compliance guarantees:
//! - `GET /u/{token}` and `GET /v/{token}` are strictly read-only confirmation pages so corporate
//!   email link scanners (Safe Links, Proofpoint, Mimecast) never trigger accidental unsubscribe
//!   or double-opt-in confirmation on prefetch.
//! - `POST /u/{token}` handles both browser form confirmation and RFC 8058 one-click POST
//!   (`List-Unsubscribe=One-Click`) idempotently without requiring login.
//! - `POST /v/{token}` confirms double-opt-in subscription, marks the recipient `verified`, and
//!   immediately wakes any deferred outbox tasks for that destination.
//! - Tokens are HMAC-SHA256 authenticated under domain-separated subkeys derived from `MasterKey`
//!   and contain zero plaintext PII.
//! - Every endpoint enforces an in-memory per-IP rate limit (`30 req / 60s`) and attaches
//!   `X-Robots-Tag: noindex, nofollow`, strict CSP, `Referrer-Policy: no-referrer`, and `Cache-Control: no-store`.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, REFERRER_POLICY};
use axum::http::{Extensions, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::api::AppState;
use crate::channels::DestinationConfig;
use crate::crypto::{constant_time_eq, decode_first_8_hex_bytes, mask_email_address};
use crate::db::models::{AppLocale, SmtpRecipientStatus, now_unix};

const PUBLIC_RATE_LIMIT_MAX: usize = 30;
const PUBLIC_RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);
const PUBLIC_RATE_LIMIT_MAX_BUCKETS: usize = 8_192;
const PUBLIC_RATE_LIMIT_MAX_KEY_BYTES: usize = 64;

struct RateBucket {
    hits: VecDeque<Instant>,
    last_seen: Instant,
}

static PUBLIC_IP_LIMITER: LazyLock<Mutex<HashMap<String, RateBucket>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn sanitize_ip_key(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() <= PUBLIC_RATE_LIMIT_MAX_KEY_BYTES {
        trimmed.to_string()
    } else {
        trimmed
            .chars()
            .take(PUBLIC_RATE_LIMIT_MAX_KEY_BYTES)
            .collect()
    }
}

fn extract_client_ip(trust_proxy: bool, headers: &HeaderMap, peer_ip: Option<IpAddr>) -> String {
    if trust_proxy {
        // Prefer the RIGHTMOST non-empty hop across all `X-Forwarded-For` headers (scanning from
        // the last header backwards) because proxies like Caddy, Traefik, Envoy, AWS ALB, and
        // standard Nginx append the real client IP to `X-Forwarded-For` while often passing any
        // spoofed client `X-Real-IP` through untouched.
        for val in headers.get_all("x-forwarded-for").iter().rev() {
            if let Ok(raw) = val.to_str()
                && let Some(xff_rightmost) = raw.rsplit(',').map(str::trim).find(|s| !s.is_empty())
            {
                return sanitize_ip_key(xff_rightmost);
            }
        }
        // Only fall back to `X-Real-IP` when no `X-Forwarded-For` header is present at all.
        for val in headers.get_all("x-real-ip").iter().rev() {
            if let Ok(raw) = val.to_str() {
                let real_ip = raw.trim();
                if !real_ip.is_empty() {
                    return sanitize_ip_key(real_ip);
                }
            }
        }
    }
    if let Some(ip) = peer_ip {
        return ip.to_string();
    }
    "direct".to_string()
}

fn check_public_rate_limit(
    trust_proxy: bool,
    headers: &HeaderMap,
    extensions: &Extensions,
) -> bool {
    let peer_ip = extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip());
    let key = extract_client_ip(trust_proxy, headers, peer_ip);
    let now = Instant::now();
    let mut guard = PUBLIC_IP_LIMITER
        .lock()
        .expect("public rate limiter mutex poisoned");

    if !guard.contains_key(&key) && guard.len() >= PUBLIC_RATE_LIMIT_MAX_BUCKETS {
        // First evict expired buckets.
        guard.retain(|_, bucket| {
            while let Some(&front) = bucket.hits.front() {
                if now.duration_since(front) >= PUBLIC_RATE_LIMIT_WINDOW {
                    bucket.hits.pop_front();
                } else {
                    break;
                }
            }
            !bucket.hits.is_empty()
        });
        // Hard capacity cap: if still at capacity inside the 60s window, evict the oldest bucket.
        if guard.len() >= PUBLIC_RATE_LIMIT_MAX_BUCKETS
            && let Some(oldest_key) = guard
                .iter()
                .min_by_key(|(_, b)| b.last_seen)
                .map(|(k, _)| k.clone())
        {
            guard.remove(&oldest_key);
        }
    }

    let bucket = guard.entry(key).or_insert_with(|| RateBucket {
        hits: VecDeque::new(),
        last_seen: now,
    });
    while let Some(&front) = bucket.hits.front() {
        if now.duration_since(front) >= PUBLIC_RATE_LIMIT_WINDOW {
            bucket.hits.pop_front();
        } else {
            break;
        }
    }
    bucket.last_seen = now;
    if bucket.hits.len() >= PUBLIC_RATE_LIMIT_MAX {
        return false;
    }
    bucket.hits.push_back(now);
    true
}

/// Public 404 handler for empty `/u`, `/u/`, `/v`, `/v/` paths so they never expose the admin SPA.
pub async fn public_not_found(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let locale = state.runtime().locale();
    if !check_public_rate_limit(state.config().trust_proxy, &headers, &extensions) {
        return rate_limited_response(locale);
    }
    not_found_response(locale)
}

/// Clear the in-memory public rate limiter state (test helper).
pub fn reset_public_rate_limiter_for_tests() {
    if let Ok(mut guard) = PUBLIC_IP_LIMITER.lock() {
        guard.clear();
    }
}

fn html_response(status: StatusCode, body: String) -> Response {
    let mut resp = (status, body).into_response();
    let headers = resp.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(
        "x-robots-tag",
        HeaderValue::from_static("noindex, nofollow"),
    );
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'",
        ),
    );
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn render_public_page(locale: AppLocale, title: &str, heading: &str, body_html: &str) -> String {
    let lang = locale.as_str();
    let footer = if locale.is_zh() {
        "由 FeedCourier 自托管投递网关提供"
    } else {
        "Powered by FeedCourier self-hosted delivery gateway"
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="{lang}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1.0">
<meta name="robots" content="noindex,nofollow">
<title>{title}</title>
<style>
  body {{ margin:0; padding:32px 16px; background:#f6f7f9; color:#111827; font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif; line-height:1.6; }}
  .card {{ max-width:480px; margin:40px auto; background:#ffffff; border:1px solid #e5e7eb; border-radius:12px; padding:32px 28px; box-shadow:0 1px 3px rgba(0,0,0,0.06); }}
  h1 {{ margin:0 0 12px 0; font-size:20px; font-weight:700; color:#111827; }}
  p {{ margin:0 0 16px 0; font-size:14px; color:#374151; }}
  .pill {{ display:inline-block; padding:4px 10px; background:#f3f4f6; border-radius:6px; font-family:monospace; font-size:13px; color:#1f2937; }}
  .btn {{ display:inline-block; padding:10px 20px; border:none; border-radius:8px; font-size:14px; font-weight:600; cursor:pointer; background:#2563eb; color:#ffffff; }}
  .btn-danger {{ background:#dc2626; }}
  .foot {{ margin-top:24px; padding-top:14px; border-top:1px solid #f3f4f6; font-size:12px; color:#9ca3af; }}
</style>
</head>
<body>
  <main class="card">
    <h1>{heading}</h1>
    {body_html}
    <div class="foot">{footer}</div>
  </main>
</body>
</html>"#,
        title = escape_html(title),
        heading = escape_html(heading),
    )
}

fn rate_limited_response(locale: AppLocale) -> Response {
    let (title, msg) = if locale.is_zh() {
        ("请求过于频繁", "请求过于频繁，请一分钟后重试。")
    } else {
        (
            "Too Many Requests",
            "Too many requests from this IP. Please try again in a minute.",
        )
    };
    html_response(
        StatusCode::TOO_MANY_REQUESTS,
        render_public_page(locale, title, title, &format!("<p>{msg}</p>")),
    )
}

fn not_found_response(locale: AppLocale) -> Response {
    let (title, msg) = if locale.is_zh() {
        ("链接无效", "该链接无效或对应的订阅记录已不存在。")
    } else {
        (
            "Invalid Link",
            "This link is invalid or no longer associated with an active subscription.",
        )
    };
    html_response(
        StatusCode::NOT_FOUND,
        render_public_page(locale, title, title, &format!("<p>{msg}</p>")),
    )
}

fn resolve_masked_recipient_email(
    state: &AppState,
    dest_encrypted: &[u8],
    address_hmac: &str,
) -> String {
    if let Ok(DestinationConfig::Smtp(smtp_dest)) = state.master_key().decrypt_json(dest_encrypted)
    {
        for addr in &smtp_dest.to_addresses {
            if state.master_key().smtp_recipient_hmac(addr) == address_hmac {
                return mask_email_address(addr);
            }
        }
    }
    "***".to_string()
}

/// `GET /u/{token}` — Read-only unsubscribe confirmation page (never mutates state on GET).
pub async fn get_unsubscribe_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    Path(token): Path<String>,
) -> Response {
    let locale = state.runtime().locale();
    if !check_public_rate_limit(state.config().trust_proxy, &headers, &extensions) {
        return rate_limited_response(locale);
    }

    let Some((dest_id, recipient_id)) = state.master_key().verify_unsubscribe_token(&token) else {
        return not_found_response(locale);
    };
    let Ok(Some(recipient)) = state.db().get_smtp_recipient(recipient_id).await else {
        return not_found_response(locale);
    };
    if recipient.destination_id != dest_id {
        return not_found_response(locale);
    }
    let Ok(Some(dest)) = state.db().get_destination(dest_id).await else {
        return not_found_response(locale);
    };

    let masked =
        resolve_masked_recipient_email(&state, &dest.encrypted_config, &recipient.address_hmac);
    let safe_dest = escape_html(&dest.name);
    let safe_masked = escape_html(&masked);

    if recipient.status == SmtpRecipientStatus::Unsubscribed {
        let (title, body) = if locale.is_zh() {
            (
                "已退订",
                format!(
                    "<p>收件人 <span class=\"pill\">{safe_masked}</span> 已退订 <strong>{safe_dest}</strong>，不会再收到后续邮件。</p>"
                ),
            )
        } else {
            (
                "Already Unsubscribed",
                format!(
                    "<p>Recipient <span class=\"pill\">{safe_masked}</span> is already unsubscribed from <strong>{safe_dest}</strong>.</p>"
                ),
            )
        };
        return html_response(
            StatusCode::OK,
            render_public_page(locale, title, title, &body),
        );
    }

    let (title, body) = if locale.is_zh() {
        (
            "确认退订邮件",
            format!(
                "<p>确认停止向 <span class=\"pill\">{safe_masked}</span> 发送来自 <strong>{safe_dest}</strong> 的订阅邮件吗？</p>\
                 <form method=\"POST\" action=\"\"><button type=\"submit\" class=\"btn btn-danger\">确认退订</button></form>"
            ),
        )
    } else {
        (
            "Confirm Unsubscribe",
            format!(
                "<p>Stop delivering emails from <strong>{safe_dest}</strong> to <span class=\"pill\">{safe_masked}</span>?</p>\
                 <form method=\"POST\" action=\"\"><button type=\"submit\" class=\"btn btn-danger\">Confirm Unsubscribe</button></form>"
            ),
        )
    };

    html_response(
        StatusCode::OK,
        render_public_page(locale, title, title, &body),
    )
}

/// `POST /u/{token}` — Execute unsubscribe (supports both browser confirmation form and RFC 8058
/// `List-Unsubscribe=One-Click` POST idempotently).
pub async fn post_unsubscribe(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    Path(token): Path<String>,
    body: Bytes,
) -> Response {
    let locale = state.runtime().locale();
    if !check_public_rate_limit(state.config().trust_proxy, &headers, &extensions) {
        return rate_limited_response(locale);
    }

    let Some((dest_id, recipient_id)) = state.master_key().verify_unsubscribe_token(&token) else {
        return not_found_response(locale);
    };
    let Ok(Some(recipient)) = state.db().get_smtp_recipient(recipient_id).await else {
        return not_found_response(locale);
    };
    if recipient.destination_id != dest_id {
        return not_found_response(locale);
    }
    let Ok(Some(dest)) = state.db().get_destination(dest_id).await else {
        return not_found_response(locale);
    };

    let body_text = String::from_utf8_lossy(&body);
    let is_one_click = body_text
        .to_ascii_lowercase()
        .contains("list-unsubscribe=one-click");
    let method = if is_one_click { "one_click" } else { "link" };

    if recipient.status != SmtpRecipientStatus::Unsubscribed
        && let Err(err) = state
            .db()
            .mark_smtp_recipient_unsubscribed(recipient_id, method, now_unix())
            .await
    {
        tracing::error!(recipient_id, error = %err, "failed to record SMTP unsubscribe");
        return html_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            render_public_page(locale, "Error", "Error", "<p>Internal error.</p>"),
        );
    }

    let masked =
        resolve_masked_recipient_email(&state, &dest.encrypted_config, &recipient.address_hmac);
    let safe_dest = escape_html(&dest.name);
    let safe_masked = escape_html(&masked);
    let (title, body_html) = if locale.is_zh() {
        (
            "退订成功",
            format!(
                "<p><span class=\"pill\">{safe_masked}</span> 已成功退订 <strong>{safe_dest}</strong>。如需恢复接收，需由管理员重新发起验证。</p>"
            ),
        )
    } else {
        (
            "Unsubscribed",
            format!(
                "<p><span class=\"pill\">{safe_masked}</span> has been unsubscribed from <strong>{safe_dest}</strong>. Re-enabling delivery requires fresh verification.</p>"
            ),
        )
    };
    html_response(
        StatusCode::OK,
        render_public_page(locale, title, title, &body_html),
    )
}

/// `GET /v/{token}` — Read-only double-opt-in verification confirmation page (never mutates on GET).
pub async fn get_verify_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    Path(token): Path<String>,
) -> Response {
    let locale = state.runtime().locale();
    if !check_public_rate_limit(state.config().trust_proxy, &headers, &extensions) {
        return rate_limited_response(locale);
    }

    let Some((dest_id, recipient_id, expires_at, code_prefix)) =
        state.master_key().verify_verify_token(&token)
    else {
        return not_found_response(locale);
    };
    let Ok(Some(recipient)) = state.db().get_smtp_recipient(recipient_id).await else {
        return not_found_response(locale);
    };
    if recipient.destination_id != dest_id {
        return not_found_response(locale);
    }
    let Ok(Some(dest)) = state.db().get_destination(dest_id).await else {
        return not_found_response(locale);
    };

    let masked =
        resolve_masked_recipient_email(&state, &dest.encrypted_config, &recipient.address_hmac);
    let safe_dest = escape_html(&dest.name);
    let safe_masked = escape_html(&masked);

    if recipient.status == SmtpRecipientStatus::Verified {
        let (title, body) = if locale.is_zh() {
            (
                "已完成验证",
                format!(
                    "<p><span class=\"pill\">{safe_masked}</span> 已完成 <strong>{safe_dest}</strong> 的收件验证。</p>"
                ),
            )
        } else {
            (
                "Already Verified",
                format!(
                    "<p><span class=\"pill\">{safe_masked}</span> is already verified for <strong>{safe_dest}</strong>.</p>"
                ),
            )
        };
        return html_response(
            StatusCode::OK,
            render_public_page(locale, title, title, &body),
        );
    }

    let now = now_unix();
    let code_matches = recipient.status == SmtpRecipientStatus::Pending
        && recipient.code_attempts < 5
        && recipient.code_sha256.as_deref().is_some_and(|hex_hash| {
            constant_time_eq(&decode_first_8_hex_bytes(hex_hash), &code_prefix)
        });
    if now > expires_at || !code_matches {
        let (title, body) = if locale.is_zh() {
            (
                "验证链接已失效",
                "<p>该验证链接已过期或已被新的验证码取代，请联系管理员重新发送验证邮件。</p>",
            )
        } else {
            (
                "Verification Link Expired",
                "<p>This verification link has expired or been superseded by a newer code. Please ask your administrator to send a new verification email.</p>",
            )
        };
        return html_response(
            StatusCode::GONE,
            render_public_page(locale, title, title, body),
        );
    }

    let (title, body) = if locale.is_zh() {
        (
            "确认接收订阅邮件",
            format!(
                "<p>确认允许 <strong>{safe_dest}</strong> 向 <span class=\"pill\">{safe_masked}</span> 投递订阅邮件吗？</p>\
                 <form method=\"POST\" action=\"\"><button type=\"submit\" class=\"btn\">确认订阅</button></form>"
            ),
        )
    } else {
        (
            "Confirm Subscription",
            format!(
                "<p>Confirm email deliveries from <strong>{safe_dest}</strong> to <span class=\"pill\">{safe_masked}</span>?</p>\
                 <form method=\"POST\" action=\"\"><button type=\"submit\" class=\"btn\">Confirm Subscription</button></form>"
            ),
        )
    };
    html_response(
        StatusCode::OK,
        render_public_page(locale, title, title, &body),
    )
}

/// `POST /v/{token}` — Execute double-opt-in verification and immediately wake deferred tasks.
pub async fn post_verify(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    Path(token): Path<String>,
) -> Response {
    let locale = state.runtime().locale();
    if !check_public_rate_limit(state.config().trust_proxy, &headers, &extensions) {
        return rate_limited_response(locale);
    }

    let Some((dest_id, recipient_id, expires_at, code_prefix)) =
        state.master_key().verify_verify_token(&token)
    else {
        return not_found_response(locale);
    };
    let Ok(Some(recipient)) = state.db().get_smtp_recipient(recipient_id).await else {
        return not_found_response(locale);
    };
    if recipient.destination_id != dest_id {
        return not_found_response(locale);
    }
    let Ok(Some(dest)) = state.db().get_destination(dest_id).await else {
        return not_found_response(locale);
    };

    let masked =
        resolve_masked_recipient_email(&state, &dest.encrypted_config, &recipient.address_hmac);
    let safe_dest = escape_html(&dest.name);
    let safe_masked = escape_html(&masked);

    if recipient.status == SmtpRecipientStatus::Verified {
        let (title, body) = if locale.is_zh() {
            (
                "验证成功",
                format!(
                    "<p><span class=\"pill\">{safe_masked}</span> 已完成 <strong>{safe_dest}</strong> 的收件验证。</p>"
                ),
            )
        } else {
            (
                "Subscription Verified",
                format!(
                    "<p><span class=\"pill\">{safe_masked}</span> is verified for <strong>{safe_dest}</strong>.</p>"
                ),
            )
        };
        return html_response(
            StatusCode::OK,
            render_public_page(locale, title, title, &body),
        );
    }

    let now = now_unix();
    let code_matches = recipient.status == SmtpRecipientStatus::Pending
        && recipient.code_attempts < 5
        && recipient.code_sha256.as_deref().is_some_and(|hex_hash| {
            constant_time_eq(&decode_first_8_hex_bytes(hex_hash), &code_prefix)
        });
    if now > expires_at || !code_matches {
        let (title, body) = if locale.is_zh() {
            (
                "验证链接已失效",
                "<p>该验证链接已过期或已被新的验证码取代，请联系管理员重新发送验证邮件。</p>",
            )
        } else {
            (
                "Verification Link Expired",
                "<p>This verification link has expired or been superseded by a newer code. Please ask your administrator to send a new verification email.</p>",
            )
        };
        return html_response(
            StatusCode::GONE,
            render_public_page(locale, title, title, body),
        );
    }

    if let Err(err) = state
        .db()
        .mark_smtp_recipient_verified(recipient_id, "link", now)
        .await
    {
        tracing::error!(recipient_id, error = %err, "failed to verify SMTP recipient via link");
        return html_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            render_public_page(locale, "Error", "Error", "<p>Internal error.</p>"),
        );
    }
    let _ = state
        .db()
        .release_deferred_tasks_for_destination(dest_id, now)
        .await;
    state.runtime().notify_outbox();

    let (title, body) = if locale.is_zh() {
        (
            "验证成功",
            format!(
                "<p><span class=\"pill\">{safe_masked}</span> 已确认接收来自 <strong>{safe_dest}</strong> 的订阅邮件。</p>"
            ),
        )
    } else {
        (
            "Subscription Verified",
            format!(
                "<p><span class=\"pill\">{safe_masked}</span> is now verified and will receive deliveries from <strong>{safe_dest}</strong>.</p>"
            ),
        )
    };
    html_response(
        StatusCode::OK,
        render_public_page(locale, title, title, &body),
    )
}
