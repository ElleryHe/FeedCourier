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
use lettre::address::Address;
use lettre::message::header::{HeaderName, HeaderValue};
use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::PoolConfig;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use minijinja::{Environment, context};

use crate::channels::{
    ChannelConfig, ChannelKind, ChannelSender, DeliveryCheckpoint, DeliveryContext,
    DeliveryOutcome, SmtpChannelConfig, SmtpSecurity,
};
use crate::crypto::sha256_hex;
use crate::db::models::{Entry, Feed, unix_to_datetime};
use crate::error::{AppError, AppResult};

/// Maximum UTF-8 byte budget for the rendered HTML email part (`95 KiB`), keeping the
/// final MIME-encoded message safely below Gmail's `102 KB` clipping threshold.
pub const MAX_EMAIL_HTML_BYTES: usize = 95 * 1024;

/// Line-wrap width for the `text/plain` `multipart/alternative` part.
pub const PLAIN_TEXT_WRAP_WIDTH: usize = 78;

/// Default per-operation SMTP socket/command timeout.
pub const DEFAULT_SMTP_TIMEOUT: Duration = Duration::from_secs(30);

const EMAIL_HTML_TEMPLATE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1.0">
<title>{{ subject }}</title>
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
      Article preview was shortened to prevent email client clipping.
      {% if continue_url %}
      <a href="{{ continue_url }}" style="color:#2563eb;font-weight:600;text-decoration:none;">Continue reading original article →</a>
      {% else %}
      <span>Continue reading original article →</span>
      {% endif %}
    </div>
    {% endif %}
    <div style="margin-top:28px;padding-top:16px;border-top:1px solid #e5e7eb;font-size:14px;color:#6b7280;">
      {% if original_url %}
      <p style="margin:0 0 8px 0;">
        <a href="{{ original_url }}" style="color:#2563eb;font-weight:600;text-decoration:none;">Read original →</a>
      </p>
      {% endif %}
      <p style="margin:0;font-size:12px;color:#9ca3af;">Delivered by FeedCourier · {{ feed_title }}</p>
    </div>
  </div>
</body>
</html>"#;

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
}

impl std::fmt::Debug for SmtpSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmtpSender")
            .field("timeout", &self.timeout)
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

    /// Render and assemble an RFC 5322 `multipart/alternative` email for a feed entry.
    pub fn build_entry_email(
        cfg: &SmtpChannelConfig,
        feed: &Feed,
        entry: &Entry,
        route_id: i64,
        existing_message_id: Option<&str>,
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

        let to_mailboxes = parse_recipient_mailboxes(&cfg.to_addresses)?;
        let reply_to_mailbox = parse_optional_mailbox(cfg.reply_to.as_deref(), "reply_to")?;

        let entry_title = entry
            .title
            .as_deref()
            .map(sanitize_header_text)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("Untitled update from {sanitized_feed}"));

        let message_id = existing_message_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| {
                deterministic_message_id(&entry.fingerprint_sha256, route_id, &from_domain)
            });

        let list_id = list_id_value(feed.id, &from_domain);
        let (html_body, html_truncated) = render_entry_html(feed, entry, &entry_title)?;
        let plain_body = render_plain_fallback(&html_body, feed, entry, &entry_title);

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

        let prior_msg_id = match &ctx.checkpoint {
            Some(DeliveryCheckpoint::Smtp { message_id }) => Some(message_id.as_str()),
            _ => None,
        };

        let built =
            match Self::build_entry_email(cfg, ctx.feed, ctx.entry, ctx.route.id, prior_msg_id) {
                Ok(built) => built,
                Err(err) => {
                    return Ok(DeliveryOutcome::PermanentFailure {
                        status_code: None,
                        checkpoint: prior_msg_id.map(|id| DeliveryCheckpoint::Smtp {
                            message_id: id.to_owned(),
                        }),
                        diagnostic: err.to_string(),
                    });
                }
            };

        let checkpoint = DeliveryCheckpoint::Smtp {
            message_id: built.message_id.clone(),
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
                let diagnostic = if msg.is_empty() {
                    Some(format!("SMTP {code} OK (Message-ID {})", built.message_id))
                } else {
                    Some(format!("SMTP {code}: {msg}"))
                };
                Ok(DeliveryOutcome::Delivered {
                    status_code: Some(code),
                    checkpoint: Some(checkpoint),
                    diagnostic,
                    updated_capabilities: None,
                })
            }
            Err(err) => Ok(classify_smtp_error(err, checkpoint)),
        }
    }

    async fn send_test(&self, config: &ChannelConfig, _use_proxy: bool) -> AppResult<String> {
        let ChannelConfig::Smtp(cfg) = config else {
            return Err(AppError::Validation(format!(
                "SmtpSender::send_test expected smtp config, got '{}'",
                config.kind()
            )));
        };
        config.validate()?;

        let from_addr: Address = cfg.from_address.trim().parse().map_err(|err| {
            AppError::Validation(format!(
                "invalid SMTP from_address '{}': {err}",
                cfg.from_address
            ))
        })?;
        let from_domain = sanitize_domain_ascii(from_addr.domain());
        let from_mailbox = Mailbox::new(Some("FeedCourier via FeedCourier".to_owned()), from_addr);
        let to_mailboxes = parse_recipient_mailboxes(&cfg.to_addresses)?;
        let reply_to = parse_optional_mailbox(cfg.reply_to.as_deref(), "reply_to")?;

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
        let subject = "FeedCourier SMTP Channel Verification";

        let env = template_env();
        let html_body = env
            .get_template("email")
            .and_then(|tmpl| {
                tmpl.render(context! {
                    subject => subject,
                    feed_title => "FeedCourier",
                    entry_title => subject,
                    meta_line => format!("Force-From: {} · Security: {:?}", cfg.from_address.trim(), cfg.security),
                    body_html => "<p>Your FeedCourier SMTP delivery channel is configured and ready.</p><p>All feed deliveries on this channel use strict <strong>Force-From</strong> alignment for SPF, DKIM, and DMARC compliance.</p>",
                    truncated => false,
                    continue_url => Option::<String>::None,
                    original_url => Some(crate::config::REPO_URL),
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
            cfg.to_addresses.join(", ")
        ))
    }
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

fn render_entry_html(feed: &Feed, entry: &Entry, entry_title: &str) -> AppResult<(String, bool)> {
    let env = template_env();
    let tmpl = env
        .get_template("email")
        .map_err(|err| AppError::Internal(anyhow::anyhow!("minijinja error: {err}")))?;

    let feed_title = sanitize_feed_title(&feed.title);
    let original_url = entry
        .canonical_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
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
    if let Some(ts) = entry.published_at.and_then(unix_to_datetime) {
        meta_parts.push(ts.format("%Y-%m-%d %H:%M UTC").to_string());
    }
    let meta_line = if meta_parts.is_empty() {
        None
    } else {
        Some(meta_parts.join(" · "))
    };

    let raw_body_html = build_body_html(entry);

    let full_html = tmpl
        .render(context! {
            subject => entry_title,
            feed_title => &feed_title,
            entry_title => entry_title,
            meta_line => &meta_line,
            body_html => &raw_body_html,
            truncated => false,
            continue_url => continue_url,
            original_url => original_url,
        })
        .map_err(|err| AppError::Internal(anyhow::anyhow!("minijinja render error: {err}")))?;

    if full_html.len() <= MAX_EMAIL_HTML_BYTES {
        return Ok((full_html, false));
    }

    // Measure template shell size with truncation notice enabled so we stay strictly <= 95 KiB.
    let shell_html = tmpl
        .render(context! {
            subject => entry_title,
            feed_title => &feed_title,
            entry_title => entry_title,
            meta_line => &meta_line,
            body_html => "",
            truncated => true,
            continue_url => continue_url,
            original_url => original_url,
        })
        .map_err(|err| AppError::Internal(anyhow::anyhow!("minijinja render error: {err}")))?;

    let mut body_budget = MAX_EMAIL_HTML_BYTES.saturating_sub(shell_html.len() + 256);
    for _ in 0..8 {
        let shortened_body = truncate_html_cleanly(&raw_body_html, body_budget);
        let candidate = tmpl
            .render(context! {
                subject => entry_title,
                feed_title => &feed_title,
                entry_title => entry_title,
                meta_line => &meta_line,
                body_html => &shortened_body,
                truncated => true,
                continue_url => continue_url,
                original_url => original_url,
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

fn build_body_html(entry: &Entry) -> String {
    if let Some(html) = entry
        .content_html
        .as_deref()
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

    "<p><em>No article body available in feed.</em></p>".to_owned()
}

fn render_plain_fallback(
    rendered_html: &str,
    feed: &Feed,
    entry: &Entry,
    entry_title: &str,
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
    if let Some(url) = entry
        .canonical_url
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        fallback.push_str("Read original: ");
        fallback.push_str(url.trim());
        fallback.push('\n');
    }
    fallback
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
