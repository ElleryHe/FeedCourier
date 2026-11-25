//! Unified log redaction rules shared by stdout formatting and persistent log storage.
//!
//! Guarantees that sensitive values (tokens, passwords, signatures, cookies, email addresses,
//! Telegram `/bot<token>/` URL segments, URL userinfo & query strings, `chat_id`s, and long titles)
//! are uniformly sanitized once before reaching either `docker logs` or `logs.db` / ClickHouse.

use std::sync::LazyLock;

use base64::Engine as _;
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

static BASIC_AUTH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bBasic\s+([A-Za-z0-9+/=]{4,})").expect("BASIC_AUTH_RE must compile")
});

static DIGEST_AUTH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\bDigest\s+(?:[A-Za-z0-9_-]+\s*=\s*(?:"(?:\\.|[^"\\])*"|[^\s,;\]]+)(?:\s*,\s*)?)+"#,
    )
    .expect("DIGEST_AUTH_RE must compile")
});

static URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b(?:https?|smtps?|socks5h?|socks4a?)://[^\s"'<>)\]}]+"#)
        .expect("URL_RE must compile")
});

static KV_SECRET_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)((?:\\{0,2}["'])?)([A-Za-z0-9_-]*(?:token|password|passwd|passphrase|secret|signature|api[_-]?key|apikey|device[_-]?key|push[_-]?key|access[_-]?key|webhook[_-]?key|authorization|cookie|credential|bearer|private[_-]?key)[A-Za-z0-9_-]*)((?:\\{0,2}["'])?)(\s*)([=:])(\s*)(?:(\\\\"(?:\\\\\\"|\\\\[^"]|[^"\\])+\\\\")|(\\"(?:\\\\\\"|\\\\|\\[^"]|[^"\\])+\\")|("(?:\\.|[^"\\])+")|('(?:\\.|[^'\\])+')|(Digest\s+(?:[A-Za-z0-9_-]+\s*=\s*(?:"(?:\\.|[^"\\])*"|[^\s,;\]]+)(?:\s*,\s*)?)+|(?:(?:Bearer|Basic|Token)\s+)?[^\s,;&"'}\]\\]+))"#,
    )
    .expect("KV_SECRET_RE must compile")
});

fn is_likely_basic_auth_token(tok: &str) -> bool {
    if tok.len() >= 4 {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(tok)
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(tok));
        if let Ok(bytes) = decoded
            && let Ok(text) = std::str::from_utf8(&bytes)
            && text.contains(':')
        {
            return true;
        }
    }
    if tok.len() >= 8 {
        if tok.ends_with('=')
            || tok
                .bytes()
                .any(|b| b.is_ascii_digit() || matches!(b, b'+' | b'/'))
        {
            return true;
        }
        if tok.bytes().skip(1).any(|b| b.is_ascii_uppercase()) {
            return true;
        }
    }
    false
}

/// Returns `true` when a structured tracing field name indicates secret material.
pub fn is_sensitive_field_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "token_id" | "token_name" | "token_scope" | "token_prefix" | "token_count" | "tokens_count"
    ) || lower.starts_with("has_")
        || lower.ends_with("_hint")
    {
        return false;
    }
    [
        "token",
        "password",
        "passwd",
        "passphrase",
        "secret",
        "signature",
        "api_key",
        "api-key",
        "apikey",
        "device_key",
        "device-key",
        "push_key",
        "push-key",
        "access_key",
        "access-key",
        "webhook_key",
        "webhook-key",
        "authorization",
        "cookie",
        "credential",
        "bearer",
        "private_key",
        "private-key",
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

    // 4. Mask inline `key=value`, `key: value`, quoted `key="value"`, JSON `"key": "value"`, and
    //    debug-escaped JSON `\"key\":\"value\"` secrets.
    let kv_cleaned = KV_SECRET_RE.replace_all(&tg_cleaned, |caps: &regex::Captures<'_>| {
        let key = &caps[2];
        if !is_sensitive_field_name(key) {
            return caps[0].to_string();
        }
        let open_kq = &caps[1];
        let close_kq = &caps[3];
        let ws_before = &caps[4];
        let sep = &caps[5];
        let ws_after = &caps[6];
        if caps.get(7).is_some() {
            format!(r#"{open_kq}{key}{close_kq}{ws_before}{sep}{ws_after}\\"[redacted]\\""#)
        } else if caps.get(8).is_some() {
            format!(r#"{open_kq}{key}{close_kq}{ws_before}{sep}{ws_after}\"[redacted]\""#)
        } else if caps.get(9).is_some() {
            format!("{open_kq}{key}{close_kq}{ws_before}{sep}{ws_after}\"[redacted]\"")
        } else if caps.get(10).is_some() {
            format!("{open_kq}{key}{close_kq}{ws_before}{sep}{ws_after}'[redacted]'")
        } else if caps.get(11).is_some_and(|m| m.as_str() == "null") && !open_kq.is_empty() {
            caps[0].to_string()
        } else {
            format!("{open_kq}{key}{close_kq}{sep}[redacted]")
        }
    });

    // 5. Mask any remaining standalone `Bearer <token>`, `Basic <base64>`, or `Digest ...` credentials.
    let bearer_cleaned = BEARER_RE.replace_all(&kv_cleaned, "Bearer [redacted]");
    let basic_cleaned = BASIC_AUTH_RE.replace_all(&bearer_cleaned, |caps: &regex::Captures<'_>| {
        let tok = &caps[1];
        if is_likely_basic_auth_token(tok) {
            "Basic [redacted]".to_string()
        } else {
            caps[0].to_string()
        }
    });
    let digest_cleaned = DIGEST_AUTH_RE.replace_all(&basic_cleaned, "Digest [redacted]");

    // 6. Mask any remaining email addresses (`alice@example.com` -> `a***e@example.com`).
    let email_cleaned = mask_emails_in_text(&digest_cleaned);

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

    #[test]
    fn redacts_json_quoted_secrets_and_smtp_urls_without_redacting_mcp_audit_fields() {
        // P1-01: JSON double-quoted, single/double-quoted values, and smtp(s):// userinfo.
        let json_payload = r#"upstream error: {"access_token":"tk_secret_42","password": "hunter2","bridge_secret":"s3cr3t","token_id":7,"token_name":"cli-admin","token_scope":"admin","has_access_token":true}"#;
        let out = redact_text(json_payload);
        assert!(
            !out.contains("tk_secret_42"),
            "json access_token leaked: {out}"
        );
        assert!(!out.contains("hunter2"), "json password leaked: {out}");
        assert!(!out.contains("s3cr3t"), "json bridge_secret leaked: {out}");
        assert!(
            out.contains(r#""access_token":"[redacted]""#),
            "unexpected format: {out}"
        );
        assert!(
            out.contains(r#""password": "[redacted]""#),
            "unexpected format: {out}"
        );
        assert!(
            out.contains(r#""bridge_secret":"[redacted]""#),
            "unexpected format: {out}"
        );
        assert!(
            out.contains(r#""token_id":7"#),
            "token_id wrongly redacted: {out}"
        );
        assert!(
            out.contains(r#""token_name":"cli-admin""#),
            "token_name wrongly redacted: {out}"
        );
        assert!(
            out.contains(r#""token_scope":"admin""#),
            "token_scope wrongly redacted: {out}"
        );
        assert!(
            out.contains(r#""has_access_token":true"#),
            "has_access_token wrongly redacted: {out}"
        );

        let quoted = r#"auth failed password="my \"quoted\" pw" secret='single_sec' smtp://alice:pass1@mail.example.com:587 smtps://bob:pass2@smtp.example.org:465"#;
        let q_out = redact_text(quoted);
        assert!(
            !q_out.contains("quoted"),
            "double-quoted password leaked: {q_out}"
        );
        assert!(
            !q_out.contains("single_sec"),
            "single-quoted secret leaked: {q_out}"
        );
        assert!(
            !q_out.contains("pass1"),
            "smtp url password leaked: {q_out}"
        );
        assert!(
            !q_out.contains("pass2"),
            "smtps url password leaked: {q_out}"
        );
        assert!(
            q_out.contains(r#"password="[redacted]""#),
            "unexpected quoted output: {q_out}"
        );
        assert!(
            q_out.contains("secret='[redacted]'"),
            "unexpected single-quoted output: {q_out}"
        );
        assert!(
            q_out.contains("smtp://mail.example.com:587"),
            "smtp host missing: {q_out}"
        );
        assert!(
            q_out.contains("smtps://smtp.example.org:465"),
            "smtps host missing: {q_out}"
        );

        // P1-02: MCP audit metadata & presence flags must not be redacted by field name.
        assert!(!is_sensitive_field_name("token_id"));
        assert!(!is_sensitive_field_name("token_name"));
        assert!(!is_sensitive_field_name("token_scope"));
        assert!(!is_sensitive_field_name("token_prefix"));
        assert!(!is_sensitive_field_name("has_password"));
        assert!(!is_sensitive_field_name("has_access_token"));
        assert_eq!(redact_field("token_name", "ops-cli"), "ops-cli");
        assert_eq!(redact_field("token_scope", "admin"), "admin");

        // Actual secret fields remain sensitive.
        assert!(is_sensitive_field_name("access_token"));
        assert!(is_sensitive_field_name("bridge_secret"));
        assert!(is_sensitive_field_name("cookie"));
        assert!(is_sensitive_field_name("private_key"));
        assert!(is_sensitive_field_name("credential"));
        assert!(is_sensitive_field_name("device_key"));
        assert!(is_sensitive_field_name("passphrase"));
        assert!(is_sensitive_field_name("push_key"));
        assert!(is_sensitive_field_name("access_key"));
        assert!(is_sensitive_field_name("webhook_key"));
        // Non-secret audit keys stay unmasked.
        assert!(!is_sensitive_field_name("idempotency_key"));
    }

    #[test]
    fn redacts_basic_auth_escaped_json_and_device_key_without_false_positives() {
        // Basic auth in Authorization header and standalone Basic base64 token.
        let basic_msg = "header Authorization: Basic dXNlcjpwYXNz and standalone Basic YWRtaW46c2VjcmV0 plus Basic settings and Basic Authentication prose";
        let out = redact_text(basic_msg);
        assert!(!out.contains("dXNlcjpwYXNz"), "header Basic leaked: {out}");
        assert!(
            !out.contains("YWRtaW46c2VjcmV0"),
            "standalone Basic leaked: {out}"
        );
        assert!(
            out.contains("Authorization:[redacted]"),
            "unexpected: {out}"
        );
        assert!(out.contains("Basic [redacted]"), "unexpected: {out}");
        assert!(
            out.contains("Basic settings"),
            "prose wrongly redacted: {out}"
        );
        assert!(
            out.contains("Basic Authentication"),
            "title-case prose wrongly redacted: {out}"
        );

        // Debug-escaped JSON strings produced by `tracing::debug!(?val)` (`record_debug`).
        let escaped = r#"body="{\"password\":\"hunter2\",\"device_key\":\"dk_live_98765\",\"passphrase\":\"p@ss\",\"idempotency_key\":\"idem_abc\"}""#;
        let esc_out = redact_text(escaped);
        assert!(
            !esc_out.contains("hunter2"),
            "escaped password leaked: {esc_out}"
        );
        assert!(
            !esc_out.contains("dk_live_98765"),
            "escaped device_key leaked: {esc_out}"
        );
        assert!(
            !esc_out.contains("p@ss"),
            "escaped passphrase leaked: {esc_out}"
        );
        assert!(
            esc_out.contains(r#"\"password\":\"[redacted]\""#),
            "unexpected escaped format: {esc_out}"
        );
        assert!(
            esc_out.contains(r#"\"device_key\":\"[redacted]\""#),
            "unexpected escaped device_key format: {esc_out}"
        );
        assert!(
            esc_out.contains(r#"\"idempotency_key\":\"idem_abc\""#),
            "idempotency_key wrongly redacted: {esc_out}"
        );

        // R2-P2-2 & R2-P3-1: `*_hint` metadata stays non-secret; bracketed unquoted values
        // preserve `]`; double-escaped JSON quotes and multi-segment Digest headers are redacted.
        assert!(!is_sensitive_field_name("device_key_hint"));
        assert!(!is_sensitive_field_name("access_key_hint"));
        assert!(!is_sensitive_field_name("signature_hint"));

        let bracketed = "meta [authorization=tok_secret_123] ok";
        let bracketed_out = redact_text(bracketed);
        assert!(
            !bracketed_out.contains("tok_secret_123"),
            "bracketed secret leaked: {bracketed_out}"
        );
        assert!(
            bracketed_out.contains("[authorization=[redacted]] ok"),
            "closing bracket was eaten: {bracketed_out}"
        );

        let double_escaped =
            r#"outer=\\"{\"password\":\\"nested_pw_42\\",\"device_key\":\\"dk_nested_77\\"}\\""#;
        let double_out = redact_text(double_escaped);
        assert!(
            !double_out.contains("nested_pw_42"),
            "double-escaped password leaked: {double_out}"
        );
        assert!(
            !double_out.contains("dk_nested_77"),
            "double-escaped device_key leaked: {double_out}"
        );

        let digest_msg = r#"Authorization: Digest username="Mufasa", realm="test@host", nonce="dcd98b71", response="6629fae49393a05397450978507c4ef1" and standalone Digest username="bob", response="deadbeef1234" plus Digest summary ready"#;
        let digest_out = redact_text(digest_msg);
        assert!(
            !digest_out.contains("6629fae49393a05397450978507c4ef1"),
            "Digest header response leaked: {digest_out}"
        );
        assert!(
            !digest_out.contains("dcd98b71"),
            "Digest header nonce leaked: {digest_out}"
        );
        assert!(
            !digest_out.contains("deadbeef1234"),
            "standalone Digest response leaked: {digest_out}"
        );
        assert!(
            digest_out.contains("Digest summary ready"),
            "prose Digest wrongly redacted: {digest_out}"
        );
    }
}
