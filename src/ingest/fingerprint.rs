//! 4-tier anti-drift SHA-256 entry fingerprinting and canonical URL normalization
//! (`feedcourier.md` Section 四.2).

use std::sync::OnceLock;

use regex::Regex;
use url::Url;

use crate::crypto::sha256_hex;

/// Maximum plain-text body characters hashed in Tier 4 fallback fingerprinting.
pub const MAX_FINGERPRINT_BODY_CHARS: usize = 2048;

/// Which tier produced an entry's deterministic 64-char SHA-256 hex fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FingerprintTier {
    /// Tier 1: Explicit Atom `<id>`, RSS `<guid>`, or JSON Feed `"id"`.
    EntryId,
    /// Tier 2: Normalized canonical URL with marketing/tracking query parameters stripped.
    CanonicalUrl,
    /// Tier 3: Trimmed title + explicit feed-supplied publish/update Unix timestamp.
    TitleTimestamp,
    /// Tier 4: Title + normalized plain-text body prefix (relative-time noise stripped).
    NormalizedBody,
}

/// Inputs extracted from a raw feed entry for 4-tier fingerprinting.
#[derive(Debug, Clone, Default)]
pub struct FingerprintInput<'a> {
    /// Explicit entry `<id>` / `<guid>` from the feed (MUST NOT be a parser-synthesized random UUID).
    pub raw_id: Option<&'a str>,
    /// Canonical article URL already normalized via [`normalize_canonical_url`].
    pub normalized_canonical_url: Option<&'a str>,
    /// Explicit `published` or `updated` Unix timestamp from the raw feed (**NEVER** `Utc::now()`).
    pub explicit_timestamp_unix: Option<i64>,
    /// Entry title, if present.
    pub title: Option<&'a str>,
    /// Plain-text summary or stripped HTML body for Tier 4 fallback.
    pub plain_body: Option<&'a str>,
}

/// Normalize a candidate canonical URL by resolving against `base_url` (if relative), restricting
/// to `http`/`https`, stripping `#fragment`, stripping marketing/tracking query parameters
/// (`utm_*`, `spm`, `fbclid`, `gclid`, etc.), and sorting remaining query parameters deterministically.
pub fn normalize_canonical_url(raw_url: &str, base_url: Option<&Url>) -> Option<String> {
    let trimmed = raw_url.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut url = match Url::parse(trimmed) {
        Ok(u) => u,
        Err(_) => base_url?.join(trimmed).ok()?,
    };

    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }

    url.set_fragment(None);

    if url.query().is_some() {
        let mut kept: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(k, _)| !is_tracking_query_param(k))
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();

        if kept.is_empty() {
            url.set_query(None);
        } else {
            kept.sort();
            url.query_pairs_mut().clear().extend_pairs(&kept);
        }
    }

    Some(url.to_string())
}

fn is_tracking_query_param(key: &str) -> bool {
    let lower = key.trim().to_ascii_lowercase();
    if lower.starts_with("utm_") {
        return true;
    }
    matches!(
        lower.as_str(),
        "spm"
            | "fbclid"
            | "gclid"
            | "dclid"
            | "msclkid"
            | "twclid"
            | "igshid"
            | "mc_cid"
            | "mc_eid"
            | "yclid"
            | "_hsenc"
            | "_hsmi"
            | "mkt_tok"
            | "vero_id"
            | "oly_enc_id"
            | "oly_anon_id"
            | "rb_clickid"
            | "s_kwcid"
            | "ref_src"
            | "ref_url"
    )
}

/// Compute the 64-char SHA-256 hex fingerprint and the [`FingerprintTier`] used.
pub fn compute_entry_fingerprint(input: &FingerprintInput<'_>) -> (String, FingerprintTier) {
    // Tier 1: Non-empty entry `<id>` / `<guid>`.
    if let Some(trimmed_id) = input.raw_id.map(str::trim).filter(|s| !s.is_empty()) {
        let payload = format!("id:{trimmed_id}");
        return (sha256_hex(payload.as_bytes()), FingerprintTier::EntryId);
    }

    // Tier 2: Normalized canonical URL.
    if let Some(norm_url) = input
        .normalized_canonical_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let payload = format!("url:{norm_url}");
        return (
            sha256_hex(payload.as_bytes()),
            FingerprintTier::CanonicalUrl,
        );
    }

    // Tier 3: ONLY when raw feed explicitly provides a published/updated timestamp + non-empty title.
    if let (Some(trimmed_title), Some(unix_ts)) = (
        input.title.map(str::trim).filter(|s| !s.is_empty()),
        input.explicit_timestamp_unix,
    ) {
        let payload = format!("title_ts:{trimmed_title}\n{unix_ts}");
        return (
            sha256_hex(payload.as_bytes()),
            FingerprintTier::TitleTimestamp,
        );
    }

    // Tier 4: Normalized plain-text body summary (strip relative-time noise, collapse whitespace,
    // prefix up to 2048 chars + title).
    let normalized = normalize_body_for_fingerprint(input.title, input.plain_body);
    let payload = format!("body:{normalized}");
    (
        sha256_hex(payload.as_bytes()),
        FingerprintTier::NormalizedBody,
    )
}

fn relative_time_noise_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:\b\d+\s*(?:seconds?|secs?|minutes?|mins?|hours?|hrs?|days?|weeks?|months?|years?)\s+ago\b|\bjust\s+now\b|\d+\s*(?:秒|分钟|小时|天|周|个月|年)前|刚刚)",
        )
        .expect("valid relative-time noise regex")
    })
}

/// Normalize title + plain-text body for Tier 4 fallback hashing.
pub fn normalize_body_for_fingerprint(title: Option<&str>, plain_body: Option<&str>) -> String {
    let norm_title = title.map(collapse_whitespace).unwrap_or_default();

    let cleaned_body = plain_body
        .map(|raw| {
            let stripped = relative_time_noise_regex().replace_all(raw, " ");
            let collapsed = collapse_whitespace(&stripped);
            collapsed
                .chars()
                .take(MAX_FINGERPRINT_BODY_CHARS)
                .collect::<String>()
        })
        .unwrap_or_default();

    match (norm_title.is_empty(), cleaned_body.is_empty()) {
        (true, true) => String::new(),
        (false, true) => norm_title,
        (true, false) => cleaned_body,
        (false, false) => format!("{norm_title}\n{cleaned_body}"),
    }
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
