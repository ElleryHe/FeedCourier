//! SMTP newsletter delivery adapter (`feedcourier.md` Section 五.1).
//!
//! Preserves `rss2email`'s Force-From anti-spoofing discipline while rendering a clean,
//! responsive single-column inline-CSS email (`minijinja`) paired with a 78-column plain-text
//! fallback (`html2text`) inside `multipart/alternative`, strictly bounded below Gmail's
//! 102 KB clipping threshold.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono_tz::Tz;
use lettre::address::Address;
use lettre::message::header::{HeaderName, HeaderValue};
use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::PoolConfig;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use minijinja::{Environment, context};

use crate::channels::{
    ChannelConfig, ChannelKind, ChannelSender, DeliveryCheckpoint, DeliveryContext,
    DeliveryOutcome, DestinationConfig, SmtpChannelConfig, SmtpDestinationConfig, SmtpSecurity,
};
use crate::crypto::sha256_hex;
use crate::db::models::{AppLocale, Entry, Feed};
use crate::error::{AppError, AppResult};
use crate::plugins::RenderOverrides;
use crate::schedule::format_local_timestamp;

/// Maximum UTF-8 byte budget for the rendered HTML email part (`95 KiB`), keeping the
/// final MIME-encoded message safely below Gmail's `102 KB` clipping threshold.
pub const MAX_EMAIL_HTML_BYTES: usize = 95 * 1024;

/// Line-wrap width for the `text/plain` `multipart/alternative` part.
pub const PLAIN_TEXT_WRAP_WIDTH: usize = 78;

/// Default per-operation SMTP socket/command timeout.
pub const DEFAULT_SMTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Inner usable card width in pixels (`640px - 2 * 24px` horizontal padding).
pub const EMAIL_CARD_INNER_WIDTH_PX: u32 = 592;

const EMAIL_HTML_TEMPLATE: &str = r#"<!DOCTYPE html>
<html lang="{{ html_lang }}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1.0">
<title>{{ subject }}</title>
<style type="text/css">
img { max-width: 100% !important; height: auto !important; }
figure { margin: 16px 0 !important; padding: 0 !important; max-width: 100% !important; }
figcaption { font-size: 13px !important; line-height: 1.5 !important; color: #6b7280 !important; }
table { max-width: 100% !important; }
pre { max-width: 100% !important; overflow-x: auto !important; white-space: pre-wrap !important; word-break: break-word !important; }
</style>
</head>
<body style="margin:0;padding:24px 12px;background-color:#f6f7f9;color:#1a1d21;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,'Helvetica Neue',Arial,sans-serif;line-height:1.65;-webkit-text-size-adjust:100%;">
  <div style="max-width:640px;margin:0 auto;background-color:#ffffff;border:1px solid #e5e7eb;border-radius:8px;padding:28px 24px;">
    <div style="font-size:12px;font-weight:600;letter-spacing:0.03em;text-transform:uppercase;color:#4b5563;margin:0 0 6px 0;">
      {{ feed_title }}
    </div>
    <h1 style="font-size:22px;line-height:1.35;font-weight:700;margin:0 0 10px 0;color:#111827;">
      {% if original_url %}<a href="{{ original_url }}" style="color:#111827;text-decoration:none;">{{ entry_title }}</a>{% else %}{{ entry_title }}{% endif %}
    </h1>
    {% if meta_line %}
    <div style="font-size:13px;color:#6b7280;margin:0 0 20px 0;padding-bottom:14px;border-bottom:1px solid #f3f4f6;">
      {{ meta_line }}
    </div>
    {% endif %}
    <div style="font-size:16px;line-height:1.7;color:#1f2937;word-wrap:break-word;overflow-wrap:break-word;">
      {{ body_html | safe }}
    </div>
    {% if truncated %}
    <div style="margin:22px 0 0 0;padding:14px 16px;background-color:#f9fafb;border-left:3px solid #2563eb;border-radius:4px;font-size:14px;color:#374151;">
      {{ truncation_notice }}
      {% if continue_url %}
      <a href="{{ continue_url }}" style="color:#2563eb;font-weight:600;text-decoration:none;">{{ continue_label }}</a>
      {% else %}
      <span>{{ continue_label }}</span>
      {% endif %}
    </div>
    {% endif %}
    <div style="margin-top:28px;padding-top:16px;border-top:1px solid #e5e7eb;font-size:14px;color:#6b7280;">
      {% if original_url %}
      <p style="margin:0 0 10px 0;">
        <a href="{{ original_url }}" style="color:#2563eb;font-weight:600;text-decoration:none;">{{ read_original_label }}</a>
      </p>
      {% endif %}
      <p style="margin:0;font-size:12px;color:#9ca3af;">{{ delivered_by_text }}</p>
      {% if subscription_reason %}
      <p style="margin:6px 0 0 0;font-size:12px;color:#9ca3af;">{{ subscription_reason }}</p>
      {% endif %}
      {% if unsubscribe_url %}
      <p style="margin:10px 0 0 0;font-size:12px;">
        <a href="{{ unsubscribe_url }}" style="display:inline-block;padding:5px 12px;border:1px solid #d1d5db;border-radius:4px;color:#4b5563;text-decoration:none;font-weight:500;">{{ unsubscribe_label }}</a>
      </p>
      {% elif unsubscribe_hint %}
      <p style="margin:6px 0 0 0;font-size:12px;color:#9ca3af;">{{ unsubscribe_hint }}</p>
      {% endif %}
      {% if sender_identity %}
      <p style="margin:6px 0 0 0;font-size:11px;color:#9ca3af;">{{ sender_identity }}</p>
      {% endif %}
    </div>
  </div>
</body>
</html>"#;

/// Optional per-recipient compliance metadata injected into rendered SMTP emails.
#[derive(Debug, Clone, Default)]
pub struct SmtpComplianceFooter<'a> {
    pub destination_id: Option<i64>,
    pub route_name: Option<&'a str>,
    pub unsubscribe_url: Option<&'a str>,
    pub sender_identity: Option<&'a str>,
    pub include_compliance_headers: bool,
}

/// Built RFC 5322 email message and metadata ready for SMTP transmission or inspection.
#[derive(Debug)]
pub struct BuiltEmail {
    pub message: Message,
    pub message_id: String,
    pub html_body: String,
    pub plain_body: String,
    pub html_truncated: bool,
}

/// Asynchronous SMTP channel adapter backed by pooled `lettre` transports.
#[derive(Clone)]
pub struct SmtpSender {
    timeout: Duration,
    transports: Arc<RwLock<HashMap<String, AsyncSmtpTransport<Tokio1Executor>>>>,
    locale: Arc<RwLock<AppLocale>>,
}

impl std::fmt::Debug for SmtpSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmtpSender")
            .field("timeout", &self.timeout)
            .field("locale", &self.locale())
            .finish()
    }
}

impl Default for SmtpSender {
    fn default() -> Self {
        Self::new()
    }
}

impl SmtpSender {
    /// Construct a new `SmtpSender` with the default 30-second timeout.
    pub fn new() -> Self {
        Self::with_timeout(DEFAULT_SMTP_TIMEOUT)
    }

    /// Construct a new `SmtpSender` with a custom per-command timeout.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout,
            transports: Arc::new(RwLock::new(HashMap::new())),
            locale: Arc::new(RwLock::new(AppLocale::En)),
        }
    }

    /// Set the delivery template locale on this `SmtpSender` (builder style).
    pub fn with_locale(self, locale: AppLocale) -> Self {
        self.set_locale(locale);
        self
    }

    /// Update the active delivery template locale at runtime.
    pub fn set_locale(&self, locale: AppLocale) {
        if let Ok(mut guard) = self.locale.write() {
            *guard = locale;
        }
    }

    /// Current delivery template locale.
    pub fn locale(&self) -> AppLocale {
        self.locale.read().map(|g| *g).unwrap_or(AppLocale::En)
    }

    /// Clone sharing the pooled transports but with an independent default `locale`
    /// (used for per-request localized channel test messages).
    pub fn for_locale(&self, locale: AppLocale) -> Self {
        Self {
            timeout: self.timeout,
            transports: Arc::clone(&self.transports),
            locale: Arc::new(RwLock::new(locale)),
        }
    }

    /// Obtain or create a pooled `AsyncSmtpTransport` matching `cfg`.
    fn transport_for(
        &self,
        cfg: &SmtpChannelConfig,
    ) -> Result<AsyncSmtpTransport<Tokio1Executor>, lettre::transport::smtp::Error> {
        let cache_key = sha256_hex(
            format!(
                "{}:{}:{:?}:{}:{}",
                cfg.host.trim().to_ascii_lowercase(),
                cfg.port,
                cfg.security,
                cfg.username.as_deref().unwrap_or(""),
                cfg.password.as_deref().unwrap_or(""),
            )
            .as_bytes(),
        );

        if let Ok(guard) = self.transports.read()
            && let Some(existing) = guard.get(&cache_key)
        {
            return Ok(existing.clone());
        }

        let host = cfg.host.trim();
        let mut builder = match cfg.security {
            SmtpSecurity::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)?,
            SmtpSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(host)?,
            SmtpSecurity::Plain => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host),
        };

        builder = builder
            .port(cfg.port)
            .timeout(Some(self.timeout))
            .pool_config(
                PoolConfig::new()
                    .max_size(2)
                    .idle_timeout(Duration::from_secs(60)),
            );

        if let Some(username) = cfg.username.as_deref().map(str::trim)
            && !username.is_empty()
        {
            let password = cfg.password.clone().unwrap_or_default();
            builder = builder.credentials(Credentials::new(username.to_owned(), password));
        }

        let transport = builder.build();
        if let Ok(mut guard) = self.transports.write() {
            guard.insert(cache_key, transport.clone());
        }
        Ok(transport)
    }

    /// Test SMTP server reachability and authentication without sending an email.
    pub async fn test_account(&self, cfg: &SmtpChannelConfig) -> AppResult<String> {
        let transport = self
            .transport_for(cfg)
            .map_err(|err| AppError::Upstream(format!("SMTP transport setup failed: {err}")))?;
        let ok = transport.test_connection().await.map_err(|err| {
            let status = err.status().map(u16::from);
            if status == Some(535) {
                AppError::Unauthorized(format!("SMTP authentication failed (535): {err}"))
            } else {
                AppError::Upstream(format!("SMTP connection test failed: {err}"))
            }
        })?;
        if !ok {
            return Err(AppError::Upstream(format!(
                "SMTP server {}:{} rejected connection test",
                cfg.host.trim(),
                cfg.port
            )));
        }
        Ok(format!(
            "SMTP account {} ({}:{}, {:?}) verified",
            cfg.from_address.trim(),
            cfg.host.trim(),
            cfg.port,
            cfg.security
        ))
    }

    /// Render and assemble an RFC 5322 `multipart/alternative` email for a feed entry (UTC default).
    pub fn build_entry_email(
        cfg: &SmtpChannelConfig,
        dest: &SmtpDestinationConfig,
        feed: &Feed,
        entry: &Entry,
        route_id: i64,
        existing_message_id: Option<&str>,
    ) -> AppResult<BuiltEmail> {
        Self::build_entry_email_with_options(
            cfg,
            dest,
            feed,
            entry,
            route_id,
            existing_message_id,
            AppLocale::En,
            chrono_tz::UTC,
        )
    }

    /// Render and assemble an RFC 5322 `multipart/alternative` email localized to `locale` (UTC default).
    pub fn build_entry_email_with_locale(
        cfg: &SmtpChannelConfig,
        dest: &SmtpDestinationConfig,
        feed: &Feed,
        entry: &Entry,
        route_id: i64,
        existing_message_id: Option<&str>,
        locale: AppLocale,
    ) -> AppResult<BuiltEmail> {
        Self::build_entry_email_with_options(
            cfg,
            dest,
            feed,
            entry,
            route_id,
            existing_message_id,
            locale,
            chrono_tz::UTC,
        )
    }

    /// Render and assemble an RFC 5322 `multipart/alternative` email localized to `locale` and `tz`.
    #[allow(clippy::too_many_arguments)]
    pub fn build_entry_email_with_options(
        cfg: &SmtpChannelConfig,
        dest: &SmtpDestinationConfig,
        feed: &Feed,
        entry: &Entry,
        route_id: i64,
        existing_message_id: Option<&str>,
        locale: AppLocale,
        tz: Tz,
    ) -> AppResult<BuiltEmail> {
        let overrides = RenderOverrides::default();
        let compliance = SmtpComplianceFooter {
            sender_identity: cfg.sender_identity.as_deref(),
            ..SmtpComplianceFooter::default()
        };
        Self::build_entry_email_full(
            cfg,
            &dest.to_addresses,
            dest.reply_to.as_deref(),
            feed,
            entry,
            route_id,
            existing_message_id,
            locale,
            tz,
            &overrides,
            &compliance,
        )
    }

    /// Full email builder supporting `RenderOverrides`, per-recipient single-`To` envelopes,
    /// RFC 8058 `List-Unsubscribe` / `List-Unsubscribe-Post` headers, and CAN-SPAM footers.
    #[allow(clippy::too_many_arguments)]
    pub fn build_entry_email_full(
        cfg: &SmtpChannelConfig,
        to_addresses: &[String],
        reply_to: Option<&str>,
        feed: &Feed,
        entry: &Entry,
        route_id: i64,
        existing_message_id: Option<&str>,
        locale: AppLocale,
        tz: Tz,
        overrides: &RenderOverrides,
        compliance: &SmtpComplianceFooter<'_>,
    ) -> AppResult<BuiltEmail> {
        let from_addr: Address = cfg.from_address.trim().parse().map_err(|err| {
            AppError::Validation(format!(
                "invalid SMTP from_address '{}': {err}",
                cfg.from_address
            ))
        })?;
        let from_domain = sanitize_domain_ascii(from_addr.domain());

        let sanitized_feed = sanitize_feed_title(&feed.title);
        let display_name = format!("{sanitized_feed} via FeedCourier");
        let from_mailbox = Mailbox::new(Some(display_name), from_addr);

        let to_mailboxes = parse_recipient_mailboxes(to_addresses)?;
        let reply_to_mailbox = parse_optional_mailbox(reply_to, "reply_to")?;

        let entry_title = entry
            .title
            .as_deref()
            .map(sanitize_header_text)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if locale.is_zh() {
                    format!("来自 {sanitized_feed} 的未命名更新")
                } else {
                    format!("Untitled update from {sanitized_feed}")
                }
            });

        let message_id = existing_message_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| {
                deterministic_message_id(&entry.fingerprint_sha256, route_id, &from_domain)
            });

        let list_id = match compliance.destination_id {
            Some(dest_id) => format!("<dest-{dest_id}.{from_domain}>"),
            None => list_id_value(feed.id, &from_domain),
        };
        let (html_body, html_truncated) =
            render_entry_html(feed, entry, &entry_title, locale, tz, overrides, compliance)?;
        let plain_body =
            render_plain_fallback(&html_body, feed, entry, &entry_title, locale, overrides);

        let mut builder = Message::builder()
            .from(from_mailbox)
            .subject(&entry_title)
            .message_id(Some(message_id.clone()))
            .raw_header(HeaderValue::new(
                HeaderName::new_from_ascii_str("Auto-Submitted"),
                "auto-generated".to_owned(),
            ))
            .raw_header(HeaderValue::new(
                HeaderName::new_from_ascii_str("List-Id"),
                list_id,
            ));

        if compliance.include_compliance_headers || compliance.unsubscribe_url.is_some() {
            let mailto = format!("<mailto:{}?subject=unsubscribe>", cfg.from_address.trim());
            if let Some(unsub_url) = compliance
                .unsubscribe_url
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                builder = builder
                    .raw_header(HeaderValue::new(
                        HeaderName::new_from_ascii_str("List-Unsubscribe"),
                        format!("<{unsub_url}>, {mailto}"),
                    ))
                    .raw_header(HeaderValue::new(
                        HeaderName::new_from_ascii_str("List-Unsubscribe-Post"),
                        "List-Unsubscribe=One-Click".to_owned(),
                    ));
            } else {
                builder = builder.raw_header(HeaderValue::new(
                    HeaderName::new_from_ascii_str("List-Unsubscribe"),
                    mailto,
                ));
            }
        }

        for mbox in to_mailboxes {
            builder = builder.to(mbox);
        }
        if let Some(reply_to) = reply_to_mailbox {
            builder = builder.reply_to(reply_to);
        }

        let message = builder
            .multipart(MultiPart::alternative_plain_html(
                plain_body.clone(),
                html_body.clone(),
            ))
            .map_err(|err| AppError::Validation(format!("failed to build MIME email: {err}")))?;

        Ok(BuiltEmail {
            message,
            message_id,
            html_body,
            plain_body,
            html_truncated,
        })
    }

    /// Immediately send a double-opt-in verification email containing a 6-digit code and an
    /// optional one-click confirmation page link (`/v/{token}` when `PUBLIC_URL` is configured).
    /// Bypasses the outbox queue and channel quotas.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_verification_email(
        &self,
        cfg: &SmtpChannelConfig,
        recipient_email: &str,
        destination_name: &str,
        code: &str,
        verify_url: Option<&str>,
        locale: AppLocale,
    ) -> AppResult<String> {
        let from_addr: Address = cfg.from_address.trim().parse().map_err(|err| {
            AppError::Validation(format!(
                "invalid SMTP from_address '{}': {err}",
                cfg.from_address
            ))
        })?;
        let from_domain = sanitize_domain_ascii(from_addr.domain());
        let from_mailbox = Mailbox::new(Some("FeedCourier".to_owned()), from_addr);
        let to_mbox: Mailbox = recipient_email.trim().parse().map_err(|err| {
            AppError::Validation(format!("invalid SMTP recipient '{recipient_email}': {err}"))
        })?;

        let probe_nonce = sha256_hex(
            format!(
                "verify:{recipient_email}:{code}:{}",
                crate::db::models::now_unix()
            )
            .as_bytes(),
        );
        let message_id = format!("<fc-verify-{}@{from_domain}>", &probe_nonce[..16]);

        let safe_dest = html_escape_text(destination_name);
        let safe_code = html_escape_text(code);
        let (subject, body_html, delivered_by_text) = if locale.is_zh() {
            let link_block = match verify_url {
                Some(u) => format!(
                    "<p style=\"margin:20px 0;\"><a href=\"{}\" style=\"display:inline-block;padding:10px 20px;background-color:#2563eb;color:#ffffff;font-weight:600;text-decoration:none;border-radius:6px;\">确认订阅</a></p><p style=\"font-size:13px;color:#6b7280;\">或将下方 6 位验证码告知管理员在后台完成验证（30 分钟内有效）：</p>",
                    html_escape_attr_simple(u)
                ),
                None => "<p style=\"font-size:14px;color:#374151;\">请将下方 6 位验证码告知管理员在 FeedCourier 后台完成验证（30 分钟内有效）：</p>".to_string(),
            };
            (
                format!("请确认接收 FeedCourier 订阅邮件 ({destination_name})"),
                format!(
                    "<p>管理员正在将你的邮箱添加为 <strong>{safe_dest}</strong> 的 RSS 邮件投递收件人。</p>{link_block}<div style=\"margin:16px 0;padding:16px;background-color:#f3f4f6;border-radius:8px;text-align:center;font-size:28px;font-weight:700;letter-spacing:0.25em;color:#111827;font-family:monospace;\">{safe_code}</div><p style=\"font-size:12px;color:#6b7280;\">如果你并未请求接收此类邮件，直接忽略本邮件即可，系统不会向你投递任何订阅内容。</p>"
                ),
                "由 FeedCourier 发送 · 收件人验证",
            )
        } else {
            let link_block = match verify_url {
                Some(u) => format!(
                    "<p style=\"margin:20px 0;\"><a href=\"{}\" style=\"display:inline-block;padding:10px 20px;background-color:#2563eb;color:#ffffff;font-weight:600;text-decoration:none;border-radius:6px;\">Confirm Subscription</a></p><p style=\"font-size:13px;color:#6b7280;\">Or share this 6-digit verification code with your administrator (valid for 30 minutes):</p>",
                    html_escape_attr_simple(u)
                ),
                None => "<p style=\"font-size:14px;color:#374151;\">Please share this 6-digit verification code with your FeedCourier administrator to confirm delivery (valid for 30 minutes):</p>".to_string(),
            };
            (
                format!("Confirm your FeedCourier email subscription ({destination_name})"),
                format!(
                    "<p>An administrator added this address as a recipient for <strong>{safe_dest}</strong>.</p>{link_block}<div style=\"margin:16px 0;padding:16px;background-color:#f3f4f6;border-radius:8px;text-align:center;font-size:28px;font-weight:700;letter-spacing:0.25em;color:#111827;font-family:monospace;\">{safe_code}</div><p style=\"font-size:12px;color:#6b7280;\">If you did not expect this email, you can safely ignore it — no feed deliveries will be sent until verified.</p>"
                ),
                "Sent by FeedCourier · Recipient Verification",
            )
        };

        let env = template_env();
        let html_body = env
            .get_template("email")
            .and_then(|tmpl| {
                tmpl.render(context! {
                    html_lang => locale.as_str(),
                    subject => &subject,
                    feed_title => "FeedCourier",
                    entry_title => &subject,
                    meta_line => Option::<String>::None,
                    body_html => &body_html,
                    truncated => false,
                    truncation_notice => "",
                    continue_label => "",
                    read_original_label => "",
                    delivered_by_text => delivered_by_text,
                    continue_url => Option::<String>::None,
                    original_url => Option::<String>::None,
                    subscription_reason => Option::<String>::None,
                    unsubscribe_url => Option::<String>::None,
                    unsubscribe_label => "",
                    unsubscribe_hint => Option::<String>::None,
                    sender_identity => cfg.sender_identity.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                })
            })
            .map_err(|err| AppError::Internal(anyhow::anyhow!("template render error: {err}")))?;

        let plain_body = if locale.is_zh() {
            let mut text = format!(
                "{subject}\n\n管理员正在将你的邮箱添加为 {destination_name} 的 RSS 邮件投递收件人。\n\n验证码（30 分钟内有效）：{code}\n"
            );
            if let Some(u) = verify_url {
                text.push_str(&format!("\n确认订阅链接：{u}\n"));
            }
            text.push_str("\n如果你并未请求接收此类邮件，请忽略本邮件。\n");
            text
        } else {
            let mut text = format!(
                "{subject}\n\nAn administrator added this address as a recipient for {destination_name}.\n\nVerification code (valid for 30 minutes): {code}\n"
            );
            if let Some(u) = verify_url {
                text.push_str(&format!("\nConfirm subscription link: {u}\n"));
            }
            text.push_str("\nIf you did not expect this email, you can safely ignore it.\n");
            text
        };

        let email = Message::builder()
            .from(from_mailbox)
            .to(to_mbox)
            .subject(&subject)
            .message_id(Some(message_id))
            .raw_header(HeaderValue::new(
                HeaderName::new_from_ascii_str("Auto-Submitted"),
                "auto-generated".to_owned(),
            ))
            .multipart(MultiPart::alternative_plain_html(plain_body, html_body))
            .map_err(|err| {
                AppError::Validation(format!("failed to build verification email: {err}"))
            })?;

        let transport = self
            .transport_for(cfg)
            .map_err(|err| AppError::Upstream(format!("SMTP transport setup failed: {err}")))?;
        let resp = transport.send(email).await.map_err(|err| {
            let status = err.status().map(u16::from);
            if status == Some(535) {
                AppError::Unauthorized(format!("SMTP authentication failed (535): {err}"))
            } else {
                AppError::Upstream(format!("SMTP verification email failed: {err}"))
            }
        })?;

        Ok(format!(
            "Verification email accepted ({}) for {}",
            u16::from(resp.code()),
            recipient_email.trim()
        ))
    }
}

#[async_trait]
impl ChannelSender for SmtpSender {
    fn kind(&self) -> ChannelKind {
        ChannelKind::Smtp
    }

    async fn send(&self, ctx: &DeliveryContext<'_>) -> AppResult<DeliveryOutcome> {
        let ChannelConfig::Smtp(cfg) = ctx.channel_config else {
            return Err(AppError::Validation(format!(
                "SmtpSender received incompatible channel config kind '{}'",
                ctx.channel_config.kind()
            )));
        };
        let DestinationConfig::Smtp(dest) = ctx.destination_config else {
            return Err(AppError::Validation(format!(
                "SmtpSender received incompatible destination config kind '{}'",
                ctx.destination_config.kind()
            )));
        };

        let locale = ctx.route_options.locale.unwrap_or_else(|| self.locale());

        // Compliant per-recipient delivery path (invoked by the outbox scheduler).
        if let Some(plan) = ctx.smtp_delivery.as_ref() {
            let (base_msg_id, mut sent_recipients) = match &ctx.checkpoint {
                Some(DeliveryCheckpoint::Smtp {
                    message_id,
                    sent_recipients,
                }) => (message_id.clone(), sent_recipients.clone()),
                _ => {
                    let from_domain = cfg
                        .from_address
                        .trim()
                        .split('@')
                        .nth(1)
                        .map(sanitize_domain_ascii)
                        .unwrap_or_else(|| "feedcourier.local".to_owned());
                    (
                        deterministic_message_id(
                            &ctx.entry.fingerprint_sha256,
                            ctx.route.id,
                            &from_domain,
                        ),
                        Vec::new(),
                    )
                }
            };

            let transport = match self.transport_for(cfg) {
                Ok(t) => t,
                Err(err) => {
                    return Ok(classify_smtp_error(
                        err,
                        DeliveryCheckpoint::Smtp {
                            message_id: base_msg_id,
                            sent_recipients,
                        },
                    ));
                }
            };

            let from_domain = cfg
                .from_address
                .trim()
                .split('@')
                .nth(1)
                .map(sanitize_domain_ascii)
                .unwrap_or_else(|| "feedcourier.local".to_owned());

            let mut delivered_this_run = 0usize;
            let mut last_code: Option<i32> = None;
            let mut bounced_notes: Vec<String> = Vec::new();

            for recipient in &plan.recipients {
                if sent_recipients.contains(&recipient.address_hmac) {
                    continue;
                }

                if delivered_this_run > 0 && !plan.per_recipient_delay.is_zero() {
                    tokio::time::sleep(plan.per_recipient_delay).await;
                    if let Some(sink) = ctx.checkpoint_sink {
                        let _ = sink.renew_lease().await;
                    }
                }

                let recip_msg_id = deterministic_recipient_message_id(
                    &ctx.entry.fingerprint_sha256,
                    ctx.route.id,
                    &recipient.address_hmac,
                    &from_domain,
                );
                let compliance = SmtpComplianceFooter {
                    destination_id: Some(ctx.destination.id),
                    route_name: Some(ctx.route.name.as_str()),
                    unsubscribe_url: recipient.unsubscribe_url.as_deref(),
                    sender_identity: cfg.sender_identity.as_deref(),
                    include_compliance_headers: true,
                };
                let single_to = [recipient.email.clone()];
                let built = match Self::build_entry_email_full(
                    cfg,
                    &single_to,
                    dest.reply_to.as_deref(),
                    ctx.feed,
                    ctx.entry,
                    ctx.route.id,
                    Some(&recip_msg_id),
                    locale,
                    ctx.timezone,
                    &ctx.overrides,
                    &compliance,
                ) {
                    Ok(b) => b,
                    Err(err) => {
                        let diag = err.to_string();
                        if let Some(sink) = ctx.checkpoint_sink {
                            let _ = sink
                                .record_smtp_hard_bounce(recipient.recipient_id, &diag)
                                .await;
                        }
                        bounced_notes.push(format!("{}: {diag}", recipient.email));
                        continue;
                    }
                };

                match transport.send(built.message).await {
                    Ok(resp) => {
                        let code = i32::from(u16::from(resp.code()));
                        last_code = Some(code);
                        delivered_this_run += 1;
                        sent_recipients.push(recipient.address_hmac.clone());
                        let cp = DeliveryCheckpoint::Smtp {
                            message_id: base_msg_id.clone(),
                            sent_recipients: sent_recipients.clone(),
                        };
                        if let Some(sink) = ctx.checkpoint_sink {
                            sink.save_checkpoint(&cp).await?;
                        }
                    }
                    Err(err) => {
                        let status_code = err.status().map(|c| i32::from(u16::from(c)));
                        let is_perm = err.is_permanent() || matches!(status_code, Some(500..=599));
                        if is_perm {
                            let diag = err.to_string();
                            if let Some(sink) = ctx.checkpoint_sink {
                                let _ = sink
                                    .record_smtp_hard_bounce(recipient.recipient_id, &diag)
                                    .await;
                            }
                            bounced_notes.push(format!(
                                "{} (HTTP/SMTP {}): {diag}",
                                recipient.email,
                                status_code.unwrap_or(550)
                            ));
                        } else {
                            let cp = DeliveryCheckpoint::Smtp {
                                message_id: base_msg_id,
                                sent_recipients,
                            };
                            return Ok(classify_smtp_error(err, cp));
                        }
                    }
                }
            }

            let final_cp = DeliveryCheckpoint::Smtp {
                message_id: base_msg_id.clone(),
                sent_recipients: sent_recipients.clone(),
            };

            if sent_recipients.is_empty() && !bounced_notes.is_empty() {
                return Ok(DeliveryOutcome::PermanentFailure {
                    status_code: Some(550),
                    checkpoint: Some(final_cp),
                    diagnostic: format!(
                        "All SMTP recipients hard-bounced: {}",
                        bounced_notes.join("; ")
                    ),
                });
            }

            let mut diag_parts = vec![format!(
                "SMTP {} OK ({}/{} recipient(s) delivered)",
                last_code.unwrap_or(250),
                sent_recipients.len(),
                plan.recipients.len()
            )];
            if !bounced_notes.is_empty() {
                diag_parts.push(format!("hard-bounced: {}", bounced_notes.join("; ")));
            }
            for w in &ctx.overrides.warnings {
                diag_parts.push(w.clone());
            }

            return Ok(DeliveryOutcome::Delivered {
                status_code: last_code.or(Some(250)),
                checkpoint: Some(final_cp),
                diagnostic: Some(diag_parts.join(" | ")),
                updated_capabilities: None,
            });
        }

        // Legacy / standalone unit-test / Debug Push single-message path.
        let prior_msg_id = match &ctx.checkpoint {
            Some(DeliveryCheckpoint::Smtp { message_id, .. }) => Some(message_id.as_str()),
            _ => None,
        };
        let compliance = SmtpComplianceFooter {
            destination_id: (ctx.destination.id > 0).then_some(ctx.destination.id),
            route_name: (!ctx.route.name.is_empty()).then_some(ctx.route.name.as_str()),
            unsubscribe_url: None,
            sender_identity: cfg.sender_identity.as_deref(),
            include_compliance_headers: false,
        };

        let built = match Self::build_entry_email_full(
            cfg,
            &dest.to_addresses,
            dest.reply_to.as_deref(),
            ctx.feed,
            ctx.entry,
            ctx.route.id,
            prior_msg_id,
            locale,
            ctx.timezone,
            &ctx.overrides,
            &compliance,
        ) {
            Ok(built) => built,
            Err(err) => {
                return Ok(DeliveryOutcome::PermanentFailure {
                    status_code: None,
                    checkpoint: prior_msg_id.map(|id| DeliveryCheckpoint::Smtp {
                        message_id: id.to_owned(),
                        sent_recipients: Vec::new(),
                    }),
                    diagnostic: err.to_string(),
                });
            }
        };

        let checkpoint = DeliveryCheckpoint::Smtp {
            message_id: built.message_id.clone(),
            sent_recipients: Vec::new(),
        };

        let transport = match self.transport_for(cfg) {
            Ok(t) => t,
            Err(err) => {
                return Ok(classify_smtp_error(err, checkpoint));
            }
        };

        match transport.send(built.message).await {
            Ok(resp) => {
                let code = i32::from(u16::from(resp.code()));
                let msg = resp.message().collect::<Vec<_>>().join(" ");
                let mut diagnostic = if msg.is_empty() {
                    format!("SMTP {code} OK (Message-ID {})", built.message_id)
                } else {
                    format!("SMTP {code}: {msg}")
                };
                if !ctx.overrides.warnings.is_empty() {
                    diagnostic.push_str(" | ");
                    diagnostic.push_str(&ctx.overrides.warnings.join(" | "));
                }
                Ok(DeliveryOutcome::Delivered {
                    status_code: Some(code),
                    checkpoint: Some(checkpoint),
                    diagnostic: Some(diagnostic),
                    updated_capabilities: None,
                })
            }
            Err(err) => Ok(classify_smtp_error(err, checkpoint)),
        }
    }

    async fn send_test(
        &self,
        channel_config: &ChannelConfig,
        destination_config: &DestinationConfig,
        _use_proxy: bool,
    ) -> AppResult<String> {
        let ChannelConfig::Smtp(cfg) = channel_config else {
            return Err(AppError::Validation(format!(
                "SmtpSender::send_test expected smtp account config, got '{}'",
                channel_config.kind()
            )));
        };
        let DestinationConfig::Smtp(dest) = destination_config else {
            return Err(AppError::Validation(format!(
                "SmtpSender::send_test expected smtp destination config, got '{}'",
                destination_config.kind()
            )));
        };
        channel_config.validate()?;
        destination_config.validate()?;

        let from_addr: Address = cfg.from_address.trim().parse().map_err(|err| {
            AppError::Validation(format!(
                "invalid SMTP from_address '{}': {err}",
                cfg.from_address
            ))
        })?;
        let from_domain = sanitize_domain_ascii(from_addr.domain());
        let from_mailbox = Mailbox::new(Some("FeedCourier via FeedCourier".to_owned()), from_addr);
        let to_mailboxes = parse_recipient_mailboxes(&dest.to_addresses)?;
        let reply_to = parse_optional_mailbox(dest.reply_to.as_deref(), "reply_to")?;

        let probe_nonce = sha256_hex(
            format!(
                "test:{}:{}:{}",
                cfg.from_address,
                cfg.host,
                crate::db::models::now_unix()
            )
            .as_bytes(),
        );
        let message_id = format!("<fc-test-{}@{from_domain}>", &probe_nonce[..16]);
        let locale = self.locale();
        let (subject, body_html, read_original_label, delivered_by_text) = if locale.is_zh() {
            (
                "FeedCourier SMTP 通道连通性验证",
                "<p>你的 FeedCourier SMTP 投递通道已配置就绪。</p><p>该通道上的所有订阅推送均严格采用 <strong>Force-From</strong> 发件人对齐策略，确保符合 SPF、DKIM 与 DMARC 校验要求。</p>",
                "访问源码仓库 →",
                "由 FeedCourier 投递 · FeedCourier",
            )
        } else {
            (
                "FeedCourier SMTP Channel Verification",
                "<p>Your FeedCourier SMTP delivery channel is configured and ready.</p><p>All feed deliveries on this channel use strict <strong>Force-From</strong> alignment for SPF, DKIM, and DMARC compliance.</p>",
                "Read original →",
                "Delivered by FeedCourier · FeedCourier",
            )
        };

        let env = template_env();
        let html_body = env
            .get_template("email")
            .and_then(|tmpl| {
                tmpl.render(context! {
                    html_lang => locale.as_str(),
                    subject => subject,
                    feed_title => "FeedCourier",
                    entry_title => subject,
                    meta_line => format!("Force-From: {} · Security: {:?}", cfg.from_address.trim(), cfg.security),
                    body_html => body_html,
                    truncated => false,
                    truncation_notice => "",
                    continue_label => "",
                    read_original_label => read_original_label,
                    delivered_by_text => delivered_by_text,
                    continue_url => Option::<String>::None,
                    original_url => Some(crate::config::REPO_URL),
                    subscription_reason => Option::<String>::None,
                    unsubscribe_url => Option::<String>::None,
                    unsubscribe_label => "",
                    unsubscribe_hint => Option::<String>::None,
                    sender_identity => cfg.sender_identity.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                })
            })
            .map_err(|err| AppError::Internal(anyhow::anyhow!("template render error: {err}")))?;

        let plain_body = html2text::from_read(html_body.as_bytes(), PLAIN_TEXT_WRAP_WIDTH)
            .unwrap_or_else(|_| {
                format!(
                    "{subject}\n\nForce-From: {}\nSecurity: {:?}\n\nYour FeedCourier SMTP delivery channel is configured and ready.\n",
                    cfg.from_address.trim(),
                    cfg.security
                )
            });

        let mut builder = Message::builder()
            .from(from_mailbox)
            .subject(subject)
            .message_id(Some(message_id))
            .raw_header(HeaderValue::new(
                HeaderName::new_from_ascii_str("Auto-Submitted"),
                "auto-generated".to_owned(),
            ))
            .raw_header(HeaderValue::new(
                HeaderName::new_from_ascii_str("List-Id"),
                format!("<feed-test.{from_domain}>"),
            ));

        for mbox in to_mailboxes {
            builder = builder.to(mbox);
        }
        if let Some(rt) = reply_to {
            builder = builder.reply_to(rt);
        }

        let email = builder
            .multipart(MultiPart::alternative_plain_html(plain_body, html_body))
            .map_err(|err| {
                AppError::Validation(format!("failed to construct test email: {err}"))
            })?;

        let transport = self
            .transport_for(cfg)
            .map_err(|err| AppError::Upstream(format!("SMTP transport setup failed: {err}")))?;

        let resp = transport.send(email).await.map_err(|err| {
            let status = err.status().map(u16::from);
            if status == Some(535) {
                AppError::Unauthorized(format!("SMTP authentication failed (535): {err}"))
            } else {
                AppError::Upstream(format!("SMTP send failed: {err}"))
            }
        })?;

        let code = u16::from(resp.code());
        Ok(format!(
            "SMTP verification email accepted ({code}) from {} to {}",
            cfg.from_address.trim(),
            dest.to_addresses.join(", ")
        ))
    }
}

/// Generate a deterministic per-recipient RFC 5322 `Message-ID`
/// (`<fc-{fp[..24]}-r{route_id}-{hmac[..8]}@{from_domain}>`).
pub fn deterministic_recipient_message_id(
    fingerprint_sha256: &str,
    route_id: i64,
    address_hmac: &str,
    from_domain: &str,
) -> String {
    let fp_prefix: String = fingerprint_sha256
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(24)
        .collect();
    let fp = if fp_prefix.is_empty() {
        "000000000000000000000000"
    } else {
        fp_prefix.as_str()
    };
    let recip_tag: String = address_hmac
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect();
    let domain = sanitize_domain_ascii(from_domain);
    format!("<fc-{fp}-r{route_id}-{recip_tag}@{domain}>")
}

/// Generate a deterministic RFC 5322 `Message-ID` (`<fc-{fp[..24]}-r{route_id}@{from_domain}>`).
pub fn deterministic_message_id(
    fingerprint_sha256: &str,
    route_id: i64,
    from_domain: &str,
) -> String {
    let fp_prefix: String = fingerprint_sha256
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(24)
        .collect();
    let fp = if fp_prefix.is_empty() {
        "000000000000000000000000"
    } else {
        fp_prefix.as_str()
    };
    let domain = sanitize_domain_ascii(from_domain);
    format!("<fc-{fp}-r{route_id}@{domain}>")
}

/// Generate an ASCII `List-Id` header value (`<feed-{feed_id}.{from_domain}>`).
pub fn list_id_value(feed_id: i64, from_domain: &str) -> String {
    let domain = sanitize_domain_ascii(from_domain);
    format!("<feed-{feed_id}.{domain}>")
}

/// Classify a `lettre` SMTP error into `DeliveryOutcome::Retry` vs `DeliveryOutcome::PermanentFailure`.
pub fn classify_smtp_error(
    err: lettre::transport::smtp::Error,
    checkpoint: DeliveryCheckpoint,
) -> DeliveryOutcome {
    let status_code = err.status().map(|c| i32::from(u16::from(c)));
    let diagnostic = err.to_string();

    if err.is_permanent() || matches!(status_code, Some(500..=599)) {
        return DeliveryOutcome::PermanentFailure {
            status_code,
            checkpoint: Some(checkpoint),
            diagnostic,
        };
    }

    let lower = diagnostic.to_ascii_lowercase();
    let is_421_or_rate_limit = matches!(status_code, Some(421 | 450 | 451 | 452))
        || lower.contains("rate")
        || lower.contains("throttl")
        || lower.contains("too many")
        || lower.contains("quota");

    DeliveryOutcome::Retry {
        retry_after_secs: parse_retry_after_hint(&lower),
        status_code,
        checkpoint: Some(checkpoint),
        diagnostic,
        cooldown_credential_group: is_421_or_rate_limit,
    }
}

fn parse_retry_after_hint(lower: &str) -> Option<u64> {
    for marker in ["retry after ", "try again in ", "wait "] {
        if let Some(rest) = lower.split(marker).nth(1) {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(secs) = digits.parse::<u64>()
                && secs > 0
            {
                return Some(secs);
            }
        }
    }
    None
}

fn sanitize_header_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_space = false;
    for ch in raw.chars() {
        if ch.is_control() {
            if !prev_space && !out.is_empty() {
                out.push(' ');
                prev_space = true;
            }
            continue;
        }
        if ch.is_whitespace() {
            if !prev_space && !out.is_empty() {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_owned()
}

fn sanitize_feed_title(title: &str) -> String {
    let cleaned = sanitize_header_text(title);
    if cleaned.is_empty() {
        return "Feed".to_owned();
    }
    let mut truncated: String = cleaned.chars().take(96).collect();
    if cleaned.chars().count() > 96 {
        truncated.push('…');
    }
    truncated
}

fn sanitize_domain_ascii(domain: &str) -> String {
    let cleaned: String = domain
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let trimmed = cleaned.trim_matches(['.', '-']);
    if trimmed.is_empty() {
        "feedcourier.local".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn parse_recipient_mailboxes(addresses: &[String]) -> AppResult<Vec<Mailbox>> {
    let mut mailboxes = Vec::new();
    for raw in addresses {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mbox: Mailbox = trimmed.parse().map_err(|err| {
            AppError::Validation(format!("invalid SMTP recipient '{trimmed}': {err}"))
        })?;
        mailboxes.push(mbox);
    }
    if mailboxes.is_empty() {
        return Err(AppError::Validation(
            "SMTP channel requires at least one valid recipient in to_addresses".to_owned(),
        ));
    }
    Ok(mailboxes)
}

fn parse_optional_mailbox(raw: Option<&str>, field_name: &str) -> AppResult<Option<Mailbox>> {
    let Some(trimmed) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let mbox: Mailbox = trimmed.parse().map_err(|err| {
        AppError::Validation(format!("invalid SMTP {field_name} '{trimmed}': {err}"))
    })?;
    Ok(Some(mbox))
}

fn template_env() -> Environment<'static> {
    let mut env = Environment::new();
    env.add_template("email", EMAIL_HTML_TEMPLATE)
        .expect("embedded SMTP minijinja template must compile");
    env
}

fn render_entry_html(
    feed: &Feed,
    entry: &Entry,
    entry_title: &str,
    locale: AppLocale,
    tz: Tz,
    overrides: &RenderOverrides,
    compliance: &SmtpComplianceFooter<'_>,
) -> AppResult<(String, bool)> {
    let env = template_env();
    let tmpl = env
        .get_template("email")
        .map_err(|err| AppError::Internal(anyhow::anyhow!("minijinja error: {err}")))?;

    let feed_title = sanitize_feed_title(&feed.title);
    let original_url = overrides.effective_primary_link(entry);
    let continue_url = original_url.or_else(|| {
        feed.site_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    });

    let mut meta_parts = Vec::new();
    if let Some(author) = entry
        .author
        .as_deref()
        .map(sanitize_header_text)
        .filter(|s| !s.is_empty())
    {
        meta_parts.push(author);
    }
    if let Some(formatted_ts) = entry
        .published_at
        .and_then(|ts| format_local_timestamp(ts, tz))
    {
        meta_parts.push(formatted_ts);
    }
    let meta_line = if meta_parts.is_empty() {
        None
    } else {
        Some(meta_parts.join(" · "))
    };

    let (
        truncation_notice,
        continue_label,
        read_original_label,
        delivered_by_text,
        unsubscribe_label,
    ) = if locale.is_zh() {
        (
            "为避免邮件客户端折叠截断，正文预览已精简。",
            "继续阅读原文 →",
            "阅读原文 →",
            format!("由 FeedCourier 投递 · {feed_title}"),
            "退订此类邮件",
        )
    } else {
        (
            "Article preview was shortened to prevent email client clipping.",
            "Continue reading original article →",
            "Read original →",
            format!("Delivered by FeedCourier · {feed_title}"),
            "Unsubscribe",
        )
    };

    let sub_label = compliance
        .route_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&feed_title);
    let subscription_reason = if compliance.include_compliance_headers {
        Some(if locale.is_zh() {
            format!("你收到本邮件是因为订阅了 {sub_label}。")
        } else {
            format!("You are receiving this email because you subscribed to {sub_label}.")
        })
    } else {
        None
    };
    let unsubscribe_url = compliance
        .unsubscribe_url
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let unsubscribe_hint = if compliance.include_compliance_headers && unsubscribe_url.is_none() {
        Some(if locale.is_zh() {
            "如需退订，请直接回复本邮件。"
        } else {
            "Reply to this email to unsubscribe."
        })
    } else {
        None
    };
    let sender_identity = compliance
        .sender_identity
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let raw_body_html = rewrite_body_html_for_email(&build_body_html(entry, overrides, locale));

    let full_html = tmpl
        .render(context! {
            html_lang => locale.as_str(),
            subject => entry_title,
            feed_title => &feed_title,
            entry_title => entry_title,
            meta_line => &meta_line,
            body_html => &raw_body_html,
            truncated => false,
            truncation_notice => truncation_notice,
            continue_label => continue_label,
            read_original_label => read_original_label,
            delivered_by_text => &delivered_by_text,
            continue_url => continue_url,
            original_url => original_url,
            subscription_reason => &subscription_reason,
            unsubscribe_url => unsubscribe_url,
            unsubscribe_label => unsubscribe_label,
            unsubscribe_hint => unsubscribe_hint,
            sender_identity => sender_identity,
        })
        .map_err(|err| AppError::Internal(anyhow::anyhow!("minijinja render error: {err}")))?;

    if full_html.len() <= MAX_EMAIL_HTML_BYTES {
        return Ok((full_html, false));
    }

    // Measure template shell size with truncation notice enabled so we stay strictly <= 95 KiB.
    let shell_html = tmpl
        .render(context! {
            html_lang => locale.as_str(),
            subject => entry_title,
            feed_title => &feed_title,
            entry_title => entry_title,
            meta_line => &meta_line,
            body_html => "",
            truncated => true,
            truncation_notice => truncation_notice,
            continue_label => continue_label,
            read_original_label => read_original_label,
            delivered_by_text => &delivered_by_text,
            continue_url => continue_url,
            original_url => original_url,
            subscription_reason => &subscription_reason,
            unsubscribe_url => unsubscribe_url,
            unsubscribe_label => unsubscribe_label,
            unsubscribe_hint => unsubscribe_hint,
            sender_identity => sender_identity,
        })
        .map_err(|err| AppError::Internal(anyhow::anyhow!("minijinja render error: {err}")))?;

    let mut body_budget = MAX_EMAIL_HTML_BYTES.saturating_sub(shell_html.len() + 256);
    for _ in 0..8 {
        let shortened_body = truncate_html_cleanly(&raw_body_html, body_budget);
        let candidate = tmpl
            .render(context! {
                html_lang => locale.as_str(),
                subject => entry_title,
                feed_title => &feed_title,
                entry_title => entry_title,
                meta_line => &meta_line,
                body_html => &shortened_body,
                truncated => true,
                truncation_notice => truncation_notice,
                continue_label => continue_label,
                read_original_label => read_original_label,
                delivered_by_text => &delivered_by_text,
                continue_url => continue_url,
                original_url => original_url,
                subscription_reason => &subscription_reason,
                unsubscribe_url => unsubscribe_url,
                unsubscribe_label => unsubscribe_label,
                unsubscribe_hint => unsubscribe_hint,
                sender_identity => sender_identity,
            })
            .map_err(|err| AppError::Internal(anyhow::anyhow!("minijinja render error: {err}")))?;

        if candidate.len() <= MAX_EMAIL_HTML_BYTES {
            return Ok((candidate, true));
        }
        let overshoot = candidate.len() - MAX_EMAIL_HTML_BYTES;
        body_budget = body_budget.saturating_sub(overshoot + 256);
        if body_budget == 0 {
            break;
        }
    }

    Ok((shell_html, true))
}

fn build_body_html(entry: &Entry, overrides: &RenderOverrides, locale: AppLocale) -> String {
    if let Some(html) = overrides
        .effective_content_html(entry)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return html.to_owned();
    }

    if let Some(summary) = entry
        .summary_text
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let mut out = String::new();
        for para in summary
            .split("\n\n")
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            out.push_str("<p>");
            escape_html_into(&mut out, para);
            out.push_str("</p>");
        }
        if !out.is_empty() {
            return out;
        }
    }

    if let Some(first) = entry
        .first_sentence
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let mut out = String::from("<p>");
        escape_html_into(&mut out, first);
        out.push_str("</p>");
        return out;
    }

    if locale.is_zh() {
        "<p><em>订阅源未提供正文内容。</em></p>".to_owned()
    } else {
        "<p><em>No article body available in feed.</em></p>".to_owned()
    }
}

/// Rewrite sanitized article body HTML so wide media and containers (`<img>`, `<figure>`,
/// `<figcaption>`, `<table>`, `<pre>`) never overflow the 640px email card across Gmail,
/// Apple Mail, and Outlook desktop/web clients.
pub fn rewrite_body_html_for_email(html: &str) -> String {
    let mut out = String::with_capacity(html.len() + 256);
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
            if let Some(end_comment) = html[lt + 4..].find("-->") {
                let end = lt + 4 + end_comment + 3;
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

        let inner = &full_tag[1..full_tag.len() - 1];
        let trimmed = inner.trim();
        if trimmed.is_empty() || trimmed.starts_with('!') || trimmed.starts_with('?') {
            out.push_str(full_tag);
            continue;
        }

        if let Some(closing) = trimmed.strip_prefix('/') {
            let close_name = closing
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .map(|c| c.to_ascii_lowercase())
                .collect::<String>();
            out.push_str(full_tag);
            if close_name == "table" {
                out.push_str("</div>");
            }
            continue;
        }

        let self_closing = trimmed.ends_with('/');
        let body = if self_closing {
            trimmed.strip_suffix('/').unwrap_or(trimmed).trim_end()
        } else {
            trimmed
        };
        let name_end = body
            .find(|c: char| c.is_ascii_whitespace() || c == '/')
            .unwrap_or(body.len());
        let tag_lower = body[..name_end].to_ascii_lowercase();

        if !matches!(
            tag_lower.as_str(),
            "img" | "figure" | "figcaption" | "table" | "pre"
        ) {
            out.push_str(full_tag);
            continue;
        }

        let mut attrs = parse_email_tag_attrs(&body[name_end..]);
        match tag_lower.as_str() {
            "img" => {
                // Strip `height` so aspect ratio scales proportionally on narrow screens.
                attrs.retain(|a| !a.name.eq_ignore_ascii_case("height"));

                // Preserve relative percentage widths (1%–100%) and explicit small pixel widths
                // (< 592px) so icons and fractional-width figures aren't stretched; clamp missing
                // or oversized widths to card inner width (592px) for Outlook compatibility.
                let raw_width = attrs
                    .iter()
                    .find(|a| a.name.eq_ignore_ascii_case("width"))
                    .and_then(|a| a.value.as_deref());

                let (width_attr, img_style) = if let Some(pct) =
                    raw_width.and_then(parse_percent_dimension)
                {
                    (
                        format!("{pct}%"),
                        format!(
                            "display:block;max-width:100%;width:{pct}%;height:auto;border:0;outline:none;text-decoration:none;"
                        ),
                    )
                } else if let Some(w) = raw_width
                    .and_then(parse_px_dimension)
                    .filter(|&w| w > 0 && w < EMAIL_CARD_INNER_WIDTH_PX)
                {
                    (
                        w.to_string(),
                        "display:block;max-width:100%;height:auto;border:0;outline:none;text-decoration:none;".to_string(),
                    )
                } else {
                    (
                        EMAIL_CARD_INNER_WIDTH_PX.to_string(),
                        "display:block;max-width:100%;width:100%;height:auto;border:0;outline:none;text-decoration:none;".to_string(),
                    )
                };

                set_or_replace_attr(&mut attrs, "width", width_attr);
                set_or_replace_attr(&mut attrs, "style", img_style);
                emit_html_tag(&mut out, "img", &attrs, self_closing);
            }
            "figure" => {
                set_or_replace_attr(
                    &mut attrs,
                    "style",
                    "margin:16px 0;padding:0;max-width:100%;".to_string(),
                );
                emit_html_tag(&mut out, "figure", &attrs, self_closing);
            }
            "figcaption" => {
                set_or_replace_attr(
                    &mut attrs,
                    "style",
                    "margin:8px 0 0 0;font-size:13px;line-height:1.5;color:#6b7280;".to_string(),
                );
                emit_html_tag(&mut out, "figcaption", &attrs, self_closing);
            }
            "table" => {
                attrs.retain(|a| !a.name.eq_ignore_ascii_case("width"));
                set_or_replace_attr(
                    &mut attrs,
                    "style",
                    "max-width:100%;width:100%;border-collapse:collapse;table-layout:auto;overflow-wrap:break-word;".to_string(),
                );
                if !self_closing {
                    out.push_str(
                        "<div style=\"max-width:100%;overflow-x:auto;-webkit-overflow-scrolling:touch;margin:14px 0;\">",
                    );
                }
                emit_html_tag(&mut out, "table", &attrs, self_closing);
            }
            "pre" => {
                set_or_replace_attr(
                    &mut attrs,
                    "style",
                    "max-width:100%;overflow-x:auto;white-space:pre-wrap;word-wrap:break-word;background-color:#f3f4f6;padding:12px 14px;border-radius:6px;font-size:13px;line-height:1.5;".to_string(),
                );
                emit_html_tag(&mut out, "pre", &attrs, self_closing);
            }
            _ => out.push_str(full_tag),
        }
    }

    out
}

#[derive(Debug, Clone)]
struct EmailTagAttr {
    name: String,
    value: Option<String>,
}

fn find_html_tag_end(slice_from_lt: &str) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (i, b) in slice_from_lt.bytes().enumerate().skip(1) {
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
        } else if b == b'"' || b == b'\'' {
            quote = Some(b);
        } else if b == b'>' {
            return Some(i);
        }
    }
    None
}

fn parse_email_tag_attrs(s: &str) -> Vec<EmailTagAttr> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let name_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'/'
        {
            i += 1;
        }
        if i == name_start {
            i += 1;
            continue;
        }
        let name = s[name_start..i].to_string();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= bytes.len() {
                out.push(EmailTagAttr {
                    name,
                    value: Some(String::new()),
                });
                break;
            }
            let val = if bytes[i] == b'"' || bytes[i] == b'\'' {
                let q = bytes[i];
                i += 1;
                let val_start = i;
                while i < bytes.len() && bytes[i] != q {
                    i += 1;
                }
                let extracted = s[val_start..i].to_string();
                if i < bytes.len() && bytes[i] == q {
                    i += 1;
                }
                extracted
            } else {
                let val_start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                s[val_start..i].to_string()
            };
            out.push(EmailTagAttr {
                name,
                value: Some(val),
            });
        } else {
            out.push(EmailTagAttr { name, value: None });
        }
    }
    out
}

fn parse_px_dimension(raw: &str) -> Option<u32> {
    let trimmed = raw.trim().to_ascii_lowercase();
    let numeric = trimmed.strip_suffix("px").unwrap_or(&trimmed).trim();
    numeric.parse::<u32>().ok()
}

fn parse_percent_dimension(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let numeric = trimmed.strip_suffix('%')?.trim();
    if numeric.is_empty()
        || numeric.starts_with('+')
        || numeric.starts_with('-')
        || !numeric.bytes().all(|b| b.is_ascii_digit() || b == b'.')
    {
        return None;
    }
    let pct: f64 = numeric.parse().ok()?;
    if !pct.is_finite() || pct <= 0.0 || pct > 100.0 {
        return None;
    }
    // Preserve clean integer or fractional percentage strings (e.g. "50" or "33.3").
    let formatted = if (pct.fract()).abs() < 1e-6 {
        format!("{}", pct.round() as u32)
    } else {
        let s = format!("{pct:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    };
    Some(formatted)
}

fn set_or_replace_attr(attrs: &mut Vec<EmailTagAttr>, name: &str, value: String) {
    if let Some(existing) = attrs.iter_mut().find(|a| a.name.eq_ignore_ascii_case(name)) {
        existing.value = Some(value);
    } else {
        attrs.push(EmailTagAttr {
            name: name.to_string(),
            value: Some(value),
        });
    }
}

fn emit_html_tag(out: &mut String, tag: &str, attrs: &[EmailTagAttr], self_closing: bool) {
    out.push('<');
    out.push_str(tag);
    for attr in attrs {
        out.push(' ');
        out.push_str(&attr.name);
        if let Some(val) = &attr.value {
            out.push_str("=\"");
            for ch in val.chars() {
                match ch {
                    '"' => out.push_str("&quot;"),
                    '<' => out.push_str("&lt;"),
                    '>' => out.push_str("&gt;"),
                    other => out.push(other),
                }
            }
            out.push('"');
        }
    }
    if self_closing {
        out.push_str(" />");
    } else {
        out.push('>');
    }
}

fn render_plain_fallback(
    rendered_html: &str,
    feed: &Feed,
    entry: &Entry,
    entry_title: &str,
    locale: AppLocale,
    overrides: &RenderOverrides,
) -> String {
    if let Ok(text) = html2text::from_read(rendered_html.as_bytes(), PLAIN_TEXT_WRAP_WIDTH)
        && !text.trim().is_empty()
    {
        return text;
    }

    let mut fallback = format!("{}\n{}\n\n", feed.title.trim(), entry_title);
    if let Some(summary) = entry
        .summary_text
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        fallback.push_str(summary.trim());
        fallback.push_str("\n\n");
    }
    if let Some(url) = overrides.effective_primary_link(entry) {
        if locale.is_zh() {
            fallback.push_str("阅读原文: ");
        } else {
            fallback.push_str("Read original: ");
        }
        fallback.push_str(url.trim());
        fallback.push('\n');
    }
    fallback
}

fn html_escape_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
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

fn html_escape_attr_simple(input: &str) -> String {
    html_escape_text(input)
}

fn escape_html_into(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\n' => out.push_str("<br>"),
            _ => out.push(ch),
        }
    }
}

/// Truncate an HTML fragment at a valid UTF-8 boundary without splitting HTML tags or
/// character entities, closing any open non-void elements in LIFO order.
fn truncate_html_cleanly(html: &str, max_bytes: usize) -> String {
    if html.len() <= max_bytes {
        return html.to_owned();
    }
    if max_bytes == 0 {
        return String::new();
    }

    // Reserve headroom for ellipsis + closing tags.
    let target = max_bytes.saturating_sub(192);
    if target == 0 {
        return String::new();
    }

    let mut cut = floor_utf8_boundary(html, target);
    let prefix = &html[..cut];

    // Never cut inside an unclosed `<tag ...` or `<!-- comment`.
    if let Some(last_lt) = prefix.rfind('<') {
        let after_lt = &prefix[last_lt..];
        if !after_lt.contains('>') {
            cut = last_lt;
        }
    }

    // Never cut inside an unclosed `&entity;`.
    let prefix = &html[..cut];
    let entity_scan_start = prefix.len().saturating_sub(16);
    let tail_window = &prefix[floor_utf8_boundary(prefix, entity_scan_start)..];
    if let Some(rel_amp) = tail_window.rfind('&') {
        let after_amp = &tail_window[rel_amp..];
        if !after_amp.contains(';') && !after_amp.contains(char::is_whitespace) {
            cut -= after_amp.len();
        }
    }

    let kept = &html[..cut];
    let open_tags = collect_unclosed_tags(kept);

    let mut out = String::with_capacity(kept.len() + 64);
    out.push_str(kept);
    if !out.is_empty() {
        out.push('…');
    }
    for tag in open_tags.into_iter().rev() {
        out.push_str("</");
        out.push_str(&tag);
        out.push('>');
    }
    out
}

fn floor_utf8_boundary(s: &str, mut index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    while index > 0 && !s.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn is_void_html_element(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn collect_unclosed_tags(html: &str) -> Vec<String> {
    let mut stack: Vec<String> = Vec::new();
    let bytes = html.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &html[i..];
        if rest.starts_with("<!--") {
            if let Some(end) = rest.find("-->") {
                i += end + 3;
                continue;
            }
            break;
        }
        let Some(gt_offset) = rest.find('>') else {
            break;
        };
        let inside = rest[1..gt_offset].trim();
        i += gt_offset + 1;

        if inside.is_empty() || inside.starts_with('!') || inside.starts_with('?') {
            continue;
        }

        if let Some(closing_rest) = inside.strip_prefix('/') {
            let name: String = closing_rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .map(|c| c.to_ascii_lowercase())
                .collect();
            if let Some(pos) = stack.iter().rposition(|open| open == &name) {
                stack.truncate(pos);
            }
            continue;
        }

        let self_closing = inside.ends_with('/');
        let name: String = inside
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .map(|c| c.to_ascii_lowercase())
            .collect();
        if !name.is_empty() && !self_closing && !is_void_html_element(&name) {
            stack.push(name);
        }
    }

    stack
}
