//! Unified log redaction rules shared by stdout formatting and persistent log storage.
//!
//! Guarantees that sensitive values (tokens, passwords, signatures, cookies, email addresses,
//! Telegram `/bot<token>/` URL segments, URL userinfo & query strings, `chat_id`s, and long titles)
//! are uniformly sanitized once before reaching either `docker logs` or `logs.db` / ClickHouse.

use std::sync::LazyLock;

use regex::Regex;

use crate::crypto::mask_emails_in_text;

/// Maximum Unicode characters retained for title-like fields in logs.
pub const MAX_LOG_TITLE_CHARS: usize = 80;

/// Maximum Unicode characters retained for a single log message or field value.
pub const MAX_LOG_VALUE_CHARS: usize = 1_024;

static TG_BOT_PATH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)/bot[0-9A-Za-z:_-]{6,}/").expect("TG_BOT_PATH_RE must compile")
});

static BEARER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{4,}").expect("BEARER_RE must compile")
});

static URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b(?:https?|socks5h?|socks4a?)://[^\s"'<>)\]}]+"#)
        .expect("URL_RE must compile")
});

static KV_SECRET_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b(token|password|passwd|secret|signature|api[_-]?key|authorization|cookie|access_token|bot_token|private_key)\s*([=:])\s*(?:Bearer\s+)?([^\s,;&"'}\]]+)"#,
    )
    .expect("KV_SECRET_RE must compile")
});

/// Returns `true` when a structured tracing field name indicates secret material.
pub fn is_sensitive_field_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "token",
        "password",
        "passwd",
        "secret",
        "signature",
        "api_key",
        "apikey",
        "authorization",
        "cookie",
        "credential",
        "bearer",
        "private_key",
    ]
    .iter()
    .any(|kw| lower.contains(kw))
}

/// Mask a Telegram `chat_id` keeping at most the final 4 characters (e.g. `-1001234567890` → `***7890`).
pub fn mask_chat_id(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 4 {
        return "****".to_string();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("***{tail}")
}

/// Strip userinfo (`user:pass@`), query string (`?…`), and fragment (`#…`) from a URL string.
pub fn strip_url_secrets(raw: &str) -> String {
    let Some(scheme_end) = raw.find("://") else {
        return raw.to_string();
    };
    let scheme = &raw[..scheme_end];
    let after_scheme = &raw[scheme_end + 3..];

    // Cut query and fragment first.
    let no_qf = after_scheme
        .split(['?', '#'])
        .next()
        .unwrap_or(after_scheme);

    // Split authority from path so userinfo is only stripped inside authority.
    let (authority, path) = match no_qf.find('/') {
        Some(idx) => (&no_qf[..idx], &no_qf[idx..]),
        None => (no_qf, ""),
    };
    let clean_authority = match authority.rsplit_once('@') {
        Some((_, host_port)) => host_port,
        None => authority,
    };

    format!("{scheme}://{clean_authority}{path}")
}

/// Sanitize arbitrary log text: strip control chars, mask Telegram `/bot<token>/`, Bearer tokens,
/// URL userinfo & query strings, secret key=value pairs, and email addresses.
pub fn redact_text(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }

    // 1. Strip ASCII control chars except '\n' and '\t' (prevents ANSI / log-forging injection).
    let cleaned: String = raw
        .chars()
        .filter(|&c| !c.is_ascii_control() || c == '\n' || c == '\t')
        .collect();

    // 2. Strip URL userinfo, query params, and fragments first so credentials in URLs vanish early.
    let urls_cleaned = URL_RE.replace_all(&cleaned, |caps: &regex::Captures<'_>| {
        strip_url_secrets(&caps[0])
    });

    // 3. Mask Telegram Bot API `/bot<token>/` path segments.
    let tg_cleaned = TG_BOT_PATH_RE.replace_all(&urls_cleaned, "/bot[redacted]/");

    // 4. Mask inline `key=value` / `key: value` secret patterns (including `Authorization: Bearer …`).
    let kv_cleaned = KV_SECRET_RE.replace_all(&tg_cleaned, "${1}${2}[redacted]");

    // 5. Mask any remaining standalone `Bearer <token>` values.
    let bearer_cleaned = BEARER_RE.replace_all(&kv_cleaned, "Bearer [redacted]");

    // 6. Mask any remaining email addresses (`alice@example.com` -> `a***e@example.com`).
    let email_cleaned = mask_emails_in_text(&bearer_cleaned);

    truncate_chars(&email_cleaned, MAX_LOG_VALUE_CHARS)
}

/// Redact a structured tracing field value according to its field name and content.
pub fn redact_field(name: &str, value: &str) -> String {
    if is_sensitive_field_name(name) {
        return "[redacted]".to_string();
    }
    let lower = name.to_ascii_lowercase();
    if lower == "chat_id" || lower.ends_with("_chat_id") {
        return mask_chat_id(value);
    }
    let redacted = redact_text(value);
    if lower == "title" || lower.ends_with("_title") {
        return truncate_chars(&redacted, MAX_LOG_TITLE_CHARS);
    }
    redacted
}

/// Truncate a string to at most `max_chars` Unicode scalar values, appending `…` when clipped.
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    if max_chars == 1 {
        return "…".to_string();
    }
    let mut out: String = text.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_sensitive_fields_emails_urls_bot_tokens_and_titles() {
        assert_eq!(redact_field("bot_token", "123456:ABCdef"), "[redacted]");
        assert_eq!(redact_field("smtp_password", "hunter2"), "[redacted]");
        assert_eq!(redact_field("signature", "deadbeef"), "[redacted]");
        assert_eq!(redact_field("api_key", "secret_123"), "[redacted]");
        assert_eq!(redact_field("chat_id", "-1001234567890"), "***7890");
        assert_eq!(redact_field("chat_id", "1234"), "****");

        let long_title = "测".repeat(100);
        let truncated = redact_field("entry_title", &long_title);
        assert_eq!(truncated.chars().count(), MAX_LOG_TITLE_CHARS);
        assert!(truncated.ends_with('…'));

        let msg = "failed calling https://admin:secret@sho.rt/yourls-api.php?signature=abc123&url=https://x.com and https://api.telegram.org/bot123456:AAHxyz999/sendMessage for alice@example.com with Authorization: Bearer fc_live_secret_xyz and standalone Bearer tok_9999";
        let out = redact_text(msg);
        assert!(!out.contains("admin:secret"), "userinfo leaked: {out}");
        assert!(!out.contains("abc123"), "query signature leaked: {out}");
        assert!(!out.contains("AAHxyz999"), "tg bot token leaked: {out}");
        assert!(!out.contains("alice@example.com"), "email leaked: {out}");
        assert!(!out.contains("fc_live_secret_xyz"), "bearer leaked: {out}");
        assert!(!out.contains("tok_9999"), "standalone bearer leaked: {out}");
        assert!(out.contains("https://sho.rt/yourls-api.php"));
        assert!(out.contains("https://api.telegram.org/bot[redacted]/sendMessage"));
        assert!(out.contains("a***e@example.com"));
        assert!(out.contains("Authorization:[redacted]"));
        assert!(out.contains("Bearer [redacted]"));
    }
}
