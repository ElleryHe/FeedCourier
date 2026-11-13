//! On-demand full-article extraction via `dom_smoothie` (pure-Rust Readability), guarded by an
//! independent concurrency semaphore (`feedcourier.md` Section 四.1).

use std::sync::Arc;
use std::time::Duration;

use dom_smoothie::{Config as ReadabilityConfig, Readability};
use reqwest::header::{ACCEPT, HeaderValue};
use tokio::sync::Semaphore;
use tracing::{debug, warn};
use url::Url;

use crate::ingest::html::{SanitizedHtmlOutput, sanitize_and_extract_html};
use crate::net::{DEFAULT_MAX_ARTICLE_BYTES, OutboundRequest, ProtectedHttpClient};

/// Default maximum concurrent Readability full-page fetches & DOM extractions (`2`).
pub const DEFAULT_READABILITY_CONCURRENCY: usize = 2;

/// Independent request timeout for full-article fetches so slow origin sites cannot stall feed polling.
pub const DEFAULT_READABILITY_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum DOM elements `dom_smoothie` will traverse before bailing out cleanly.
const MAX_DOM_ELEMENTS_TO_PARSE: usize = 30_000;

/// Minimum character threshold for Readability candidate selection (gentler than 500 so concise
/// release notes and technical posts still extract cleanly).
const READABILITY_CHAR_THRESHOLD: usize = 100;

/// Extracted full-article result after Readability + relative URL resolution + `ammonia` sanitization.
#[derive(Debug, Clone)]
pub struct ExtractedArticle {
    pub final_url: Url,
    pub title: Option<String>,
    pub byline: Option<String>,
    pub favicon_url: Option<String>,
    pub sanitized: SanitizedHtmlOutput,
}

/// Semaphore-guarded full-content extractor shared across feed polling and live previews.
#[derive(Debug, Clone)]
pub struct ReadabilityExtractor {
    semaphore: Arc<Semaphore>,
    timeout: Duration,
}

impl Default for ReadabilityExtractor {
    fn default() -> Self {
        Self::new(DEFAULT_READABILITY_CONCURRENCY)
    }
}

impl ReadabilityExtractor {
    /// Construct a [`ReadabilityExtractor`] with the given concurrency cap (minimum `1`).
    pub fn new(concurrency: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(concurrency.max(1))),
            timeout: DEFAULT_READABILITY_TIMEOUT,
        }
    }

    /// Override the per-article HTTP fetch timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Fetch `article_url` through `http` (`max_bytes = 5 MiB`), extract the main article via
    /// `dom_smoothie::Readability`, resolve relative URLs against the final post-redirect URL,
    /// and sanitize with `ammonia`.
    ///
    /// Returns `None` on any network, SSRF, HTTP status, size-cap, or DOM extraction failure so
    /// callers gracefully fall back to the feed's native summary/content without failing the entry.
    pub async fn fetch_and_extract(
        &self,
        http: &ProtectedHttpClient,
        article_url: &str,
        use_proxy: bool,
    ) -> Option<ExtractedArticle> {
        let trimmed = article_url.trim();
        if trimmed.is_empty() {
            return None;
        }

        let _permit = match self.semaphore.acquire().await {
            Ok(permit) => permit,
            Err(_) => return None,
        };

        let req = OutboundRequest::new(trimmed)
            .max_bytes(DEFAULT_MAX_ARTICLE_BYTES)
            .use_proxy(use_proxy)
            .timeout(self.timeout)
            .header(
                ACCEPT,
                HeaderValue::from_static(
                    "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
                ),
            );

        let resp = match http.get_bytes(req).await {
            Ok(r) => r,
            Err(err) => {
                warn!(url = %trimmed, error = %err, "Readability fetch failed; falling back to feed content");
                return None;
            }
        };

        if !resp.status.is_success() {
            warn!(
                url = %trimmed,
                status = %resp.status,
                "Readability target returned non-2xx HTTP status; falling back to feed content"
            );
            return None;
        }

        if let Some(ct) = resp.content_type.as_deref() {
            let lower = ct.to_ascii_lowercase();
            let essence = lower.split(';').next().unwrap_or(&lower).trim();
            if !essence.is_empty()
                && !essence.contains("html")
                && !essence.contains("xml")
                && !essence.starts_with("text/")
            {
                debug!(
                    url = %trimmed,
                    content_type = %ct,
                    "Readability target is not HTML/text; falling back to feed content"
                );
                return None;
            }
        }

        if resp.bytes.is_empty() {
            return None;
        }

        let raw_html = String::from_utf8_lossy(&resp.bytes).into_owned();
        let final_url = resp.final_url;

        match tokio::task::spawn_blocking(move || extract_from_html_sync(&raw_html, &final_url))
            .await
        {
            Ok(Some(extracted)) => Some(extracted),
            Ok(None) => {
                debug!(url = %trimmed, "Readability produced empty/unusable output; falling back to feed content");
                None
            }
            Err(join_err) => {
                warn!(url = %trimmed, error = %join_err, "Readability worker panicked; falling back to feed content");
                None
            }
        }
    }
}

/// Pure synchronous helper running `dom_smoothie::Readability` + `sanitize_and_extract_html`.
pub fn extract_from_html_sync(raw_html: &str, document_url: &Url) -> Option<ExtractedArticle> {
    if raw_html.trim().is_empty() {
        return None;
    }

    let cfg = ReadabilityConfig {
        // Keep `class` attributes during DOM extraction so syntax-highlighting hints like
        // `<code class="language-rust">` survive into `ammonia` (which strips non-code classes).
        keep_classes: true,
        max_elements_to_parse: MAX_DOM_ELEMENTS_TO_PARSE,
        char_threshold: READABILITY_CHAR_THRESHOLD,
        ..ReadabilityConfig::default()
    };

    let mut reader = Readability::new(raw_html, Some(document_url.as_str()), Some(cfg)).ok()?;
    let article = reader.parse().ok()?;

    let content_str: &str = article.content.as_ref();
    let text_str: &str = article.text_content.as_ref();
    if content_str.trim().is_empty() || text_str.trim().is_empty() {
        return None;
    }

    let sanitized = sanitize_and_extract_html(content_str, Some(document_url));
    if sanitized.content_html.trim().is_empty() || sanitized.plain_text.trim().is_empty() {
        return None;
    }

    let title = non_empty_trimmed(&article.title);
    let byline = article.byline.as_deref().and_then(non_empty_trimmed);
    let favicon_url = article
        .favicon
        .as_deref()
        .and_then(|fav| resolve_http_url(fav, document_url));

    Some(ExtractedArticle {
        final_url: document_url.clone(),
        title,
        byline,
        favicon_url,
        sanitized,
    })
}

fn non_empty_trimmed(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn resolve_http_url(raw: &str, base: &Url) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parsed = Url::parse(trimmed)
        .ok()
        .or_else(|| base.join(trimmed).ok())?;
    if matches!(parsed.scheme(), "http" | "https") {
        Some(parsed.to_string())
    } else {
        None
    }
}
