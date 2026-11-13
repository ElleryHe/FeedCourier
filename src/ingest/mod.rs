//! Feed polling, `feed-rs` parsing, 4-tier anti-drift SHA-256 fingerprinting, relative URL
//! resolution, `ammonia` HTML sanitization, on-demand `dom_smoothie` Readability extraction,
//! content filtering, and OPML 2.0 import/export (`feedcourier.md` Sections 四.1, 四.2).

pub mod filter;
pub mod fingerprint;
pub mod html;
pub mod opml;
pub mod readability;

use std::cmp::Reverse;
use std::collections::HashSet;
use std::io::Cursor;

use reqwest::StatusCode;
use reqwest::header::{ACCEPT, HeaderValue};
use serde::{Deserialize, Serialize};
use tracing::warn;
use url::Url;

use crate::db::Database;
use crate::db::models::{
    EntryInsertOutcome, Feed, FeedHealthStatus, MediaAttachment, MediaAttachmentKind, NewEntry,
    Route, now_unix, unix_to_rfc3339,
};
use crate::error::{AppError, AppResult};
use crate::net::{DEFAULT_MAX_FEED_BYTES, OutboundRequest, ProtectedHttpClient};

pub use self::filter::{
    CompiledContentFilter, ContentFilterRules, route_matches_entry, route_matches_feed,
};
pub use self::fingerprint::{
    FingerprintInput, FingerprintTier, compute_entry_fingerprint, normalize_canonical_url,
};
pub use self::html::{
    SanitizedHtmlOutput, classify_media_kind, deduplicate_media, extract_first_sentence,
    html_to_plain_text, sanitize_and_extract_html,
};
pub use self::opml::{
    OpmlFeedOutline, OpmlImportResult, export_opml, import_opml_into_db, parse_opml,
};
pub use self::readability::{
    DEFAULT_READABILITY_CONCURRENCY, DEFAULT_READABILITY_TIMEOUT, ExtractedArticle,
    ReadabilityExtractor,
};

/// Default consecutive failure threshold before tripping `feeds.health_status = 'circuit_open'`.
pub const DEFAULT_CIRCUIT_BREAKER_THRESHOLD: i64 = 15;

/// Maximum exponential backoff delay (`6 hours`) before adding jitter.
pub const MAX_BACKOFF_DELAY_SECS: i64 = 6 * 3600;

/// Maximum latest items returned by [`IngestEngine::preview_feed`].
pub const PREVIEW_MAX_ITEMS: usize = 10;

/// Maximum top items on which live Readability runs during [`IngestEngine::preview_feed`].
pub const PREVIEW_READABILITY_MAX_ITEMS: usize = 3;

/// Maximum sanitized HTML characters included per item in [`FeedPreviewItem::content_html_preview`].
pub const PREVIEW_HTML_MAX_CHARS: usize = 4096;

const FEED_ACCEPT_HEADER: &str = "application/feed+json, application/atom+xml, application/rss+xml, application/xml, text/xml;q=0.9, */*;q=0.8";

/// Outcome returned by [`IngestEngine::poll_feed`] (matches `PollFeedResponse` in `web/src/api/types.ts`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollFeedOutcome {
    pub feed: Feed,
    pub new_entries: usize,
    pub enqueued_tasks: usize,
    pub not_modified: bool,
}

/// Single previewed entry returned by [`IngestEngine::preview_feed`] (matches `FeedPreviewItem` in `web/src/api/types.ts`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedPreviewItem {
    pub fingerprint_sha256: String,
    pub title: String,
    pub canonical_url: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<String>,
    pub first_sentence: Option<String>,
    pub summary_text: Option<String>,
    pub content_html_preview: Option<String>,
    pub media_urls: Vec<String>,
    pub full_content_extracted: bool,
    pub matched_filter: bool,
    pub filter_reason: Option<String>,
}

/// Live feed preview payload returned by [`IngestEngine::preview_feed`] (matches `FeedPreviewResponse` in `web/src/api/types.ts`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FeedPreviewResponse {
    pub feed_title: Option<String>,
    pub site_url: Option<String>,
    pub favicon_url: Option<String>,
    pub items: Vec<FeedPreviewItem>,
}

/// Alias matching [`FeedPreviewResponse`] for callers referring to `PreviewResult`.
pub type PreviewResult = FeedPreviewResponse;

/// Unified feed ingestion engine coordinating SSRF-guarded feed polling, 4-tier deduplication,
/// optional Readability extraction, route filtering, outbox task enqueueing, and OPML import/export.
#[derive(Debug, Clone)]
pub struct IngestEngine {
    db: Database,
    http: ProtectedHttpClient,
    readability: ReadabilityExtractor,
    circuit_breaker_threshold: i64,
}

impl IngestEngine {
    /// Construct an [`IngestEngine`] with default Readability concurrency (`2`) and circuit-breaker
    /// threshold (`15`).
    pub fn new(db: Database, http: ProtectedHttpClient) -> Self {
        Self {
            db,
            http,
            readability: ReadabilityExtractor::new(DEFAULT_READABILITY_CONCURRENCY),
            circuit_breaker_threshold: DEFAULT_CIRCUIT_BREAKER_THRESHOLD,
        }
    }

    /// Construct an [`IngestEngine`] with a custom Readability concurrency limit.
    pub fn with_readability_concurrency(
        db: Database,
        http: ProtectedHttpClient,
        concurrency: usize,
    ) -> Self {
        Self {
            db,
            http,
            readability: ReadabilityExtractor::new(concurrency),
            circuit_breaker_threshold: DEFAULT_CIRCUIT_BREAKER_THRESHOLD,
        }
    }

    /// Override the consecutive failure threshold that trips `FeedHealthStatus::CircuitOpen`.
    pub fn with_circuit_breaker_threshold(mut self, threshold: i64) -> Self {
        self.circuit_breaker_threshold = threshold.max(1);
        self
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    pub fn http(&self) -> &ProtectedHttpClient {
        &self.http
    }

    pub fn readability(&self) -> &ReadabilityExtractor {
        &self.readability
    }

    /// Poll a persisted feed by `feed_id`:
    /// - Optionally resets circuit-breaker / backoff state (`reset_circuit == true`).
    /// - Sends conditional `If-None-Match` / `If-Modified-Since` when `!reset_circuit`.
    /// - Records exponential backoff (`min(base * 2^failures, 6h) + jitter`, honoring `Retry-After`
    ///   on `429`/`503`) and trips `circuit_open` at `DEFAULT_CIRCUIT_BREAKER_THRESHOLD` (`15`).
    /// - Deduplicates entries via 4-tier SHA-256 fingerprinting, runs optional Readability on new
    ///   deliverable entries, honors `silent_init` (`0` tasks vs `1` newest entry when
    ///   `push_latest_one_if_silent == true`), and enqueues `delivery_tasks` across matching
    ///   enabled routes.
    pub async fn poll_feed(
        &self,
        feed_id: i64,
        reset_circuit: bool,
        push_latest_one_if_silent: bool,
    ) -> AppResult<PollFeedOutcome> {
        let mut feed = self
            .db
            .get_feed(feed_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("feed {feed_id} not found")))?;

        if reset_circuit {
            feed = self
                .db
                .reset_feed_for_immediate_poll(feed_id, now_unix())
                .await?;
        }

        let mut req = OutboundRequest::new(&feed.url)
            .max_bytes(DEFAULT_MAX_FEED_BYTES)
            .use_proxy(feed.use_proxy)
            .header(ACCEPT, HeaderValue::from_static(FEED_ACCEPT_HEADER));

        if !reset_circuit {
            req = req
                .if_none_match(feed.etag.clone())
                .if_modified_since(feed.last_modified.clone());
        }

        let resp = match self.http.get_bytes(req).await {
            Ok(r) => r,
            Err(err) => {
                self.record_poll_failure(&feed, None, &err.to_string())
                    .await?;
                return Err(err);
            }
        };

        if resp.status == StatusCode::NOT_MODIFIED {
            let next_poll_at = now_unix().saturating_add(feed.poll_interval_secs.max(1));
            let updated_feed = self
                .db
                .record_feed_poll_success(
                    feed.id,
                    resp.etag.as_deref(),
                    resp.last_modified.as_deref(),
                    next_poll_at,
                    None,
                    None,
                    None,
                    false,
                )
                .await?;

            return Ok(PollFeedOutcome {
                feed: updated_feed,
                new_entries: 0,
                enqueued_tasks: 0,
                not_modified: true,
            });
        }

        if !resp.status.is_success() {
            let retry_after = if matches!(
                resp.status,
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
            ) {
                resp.retry_after_secs
            } else {
                None
            };
            let err =
                AppError::Upstream(format!("feed '{}' returned HTTP {}", feed.url, resp.status));
            self.record_poll_failure(&feed, retry_after, &err.to_string())
                .await?;
            return Err(err);
        }

        let parsed_feed = match parse_feed_bytes(&resp.bytes, &resp.final_url) {
            Ok(parsed) => parsed,
            Err(parse_err) => {
                self.record_poll_failure(&feed, None, &parse_err.to_string())
                    .await?;
                return Err(parse_err);
            }
        };

        let meta = extract_feed_metadata(&parsed_feed, &resp.final_url, feed.site_url.as_deref());
        let mut discovered_favicon = meta.favicon_url.clone();

        // Prepare candidate entries (4-tier fingerprint + native HTML sanitization + enclosures).
        let mut candidates = prepare_candidates(&parsed_feed, &resp.final_url, &meta);

        // Check which fingerprints already exist on this feed so we never waste Readability HTTP
        // fetches on already-ingested historical items.
        let existing_fps = self.load_existing_fingerprints(feed.id).await?;
        for cand in &mut candidates {
            cand.already_exists = existing_fps.contains(&cand.fingerprint_sha256);
        }

        // When `silent_init == true && push_latest_one_if_silent == true`, identify the single
        // newest brand-new entry (preferring highest explicit `published_at`, breaking ties by
        // earliest position in the feed).
        let silent_push_idx: Option<usize> = if feed.silent_init && push_latest_one_if_silent {
            candidates
                .iter()
                .enumerate()
                .filter(|(_, c)| !c.already_exists)
                .max_by_key(|(idx, c)| (c.published_at.unwrap_or(i64::MIN), Reverse(*idx)))
                .map(|(idx, _)| idx)
        } else {
            None
        };

        let matching_routes = self.load_compiled_routes_for_feed(&feed).await?;
        let now = now_unix();
        let mut new_entries = 0usize;
        let mut enqueued_tasks = 0usize;

        for (idx, mut cand) in candidates.into_iter().enumerate() {
            if cand.already_exists {
                continue;
            }

            let should_deliver = if !feed.silent_init {
                true
            } else if push_latest_one_if_silent {
                Some(idx) == silent_push_idx
            } else {
                false
            };

            if feed.fetch_full_content
                && should_deliver
                && let Some(canon_url) = cand.canonical_url.clone()
                && let Some(extracted) = self
                    .readability
                    .fetch_and_extract(&self.http, &canon_url, feed.use_proxy)
                    .await
            {
                apply_extracted_article(&mut cand, extracted, &mut discovered_favicon);
            }

            let new_entry = NewEntry {
                feed_id: feed.id,
                fingerprint_sha256: cand.fingerprint_sha256.clone(),
                published_at: cand.published_at,
                discovered_at: now,
                title: cand.title.clone(),
                canonical_url: cand.canonical_url.clone(),
                author: cand.author.clone(),
                content_html: non_empty_string(&cand.sanitized.content_html),
                first_sentence: cand.sanitized.first_sentence.clone(),
                summary_text: cand.sanitized.summary_text.clone(),
                media: cand.media.clone(),
            };

            match self.db.insert_entry_if_new(&new_entry).await? {
                EntryInsertOutcome::Inserted(inserted) => {
                    new_entries += 1;

                    if should_deliver {
                        let filter_body = filter_body_text(&cand);
                        for compiled_route in &matching_routes {
                            let (matched, _) = compiled_route
                                .filter
                                .evaluate(cand.title.as_deref(), Some(&filter_body));
                            if matched
                                && self
                                    .db
                                    .enqueue_delivery_task(
                                        compiled_route.route.id,
                                        compiled_route.route.channel_id,
                                        inserted.id,
                                        now,
                                    )
                                    .await?
                                    .is_some()
                            {
                                enqueued_tasks += 1;
                            }
                        }
                    } else {
                        // Silent initialization item not slated for delivery: keep the permanent
                        // SHA-256 fingerprint row while immediately purging transient body columns.
                        self.purge_entry_payload_now(inserted.id, now).await?;
                    }
                }
                EntryInsertOutcome::Duplicate { .. } => {}
            }
        }

        let next_poll_at = now_unix().saturating_add(feed.poll_interval_secs.max(1));
        let updated_feed = self
            .db
            .record_feed_poll_success(
                feed.id,
                resp.etag.as_deref(),
                resp.last_modified.as_deref(),
                next_poll_at,
                meta.title.as_deref(),
                meta.site_url.as_deref(),
                discovered_favicon.as_deref(),
                feed.silent_init,
            )
            .await?;

        Ok(PollFeedOutcome {
            feed: updated_feed,
            new_entries,
            enqueued_tasks,
            not_modified: false,
        })
    }

    /// Live SSRF-protected feed preview (`POST /api/v1/feeds/preview` & MCP `preview_feed_latest`):
    /// - Fetches and parses up to [`PREVIEW_MAX_ITEMS`] (`10`) latest items without touching SQLite.
    /// - Runs Readability on up to [`PREVIEW_READABILITY_MAX_ITEMS`] (`3`) items when
    ///   `fetch_full_content == true` so interactive previews remain fast.
    /// - Evaluates optional `filter` dry-run rules against every previewed item.
    pub async fn preview_feed(
        &self,
        url: &str,
        fetch_full_content: bool,
        use_proxy: bool,
        filter: Option<&ContentFilterRules>,
    ) -> AppResult<FeedPreviewResponse> {
        let compiled_filter = match filter {
            Some(rules) => Some(rules.compile()?),
            None => None,
        };

        let req = OutboundRequest::new(url.trim())
            .max_bytes(DEFAULT_MAX_FEED_BYTES)
            .use_proxy(use_proxy)
            .header(ACCEPT, HeaderValue::from_static(FEED_ACCEPT_HEADER));

        let resp = self.http.get_bytes(req).await?;
        if !resp.status.is_success() {
            return Err(AppError::Upstream(format!(
                "feed preview target '{}' returned HTTP {}",
                resp.final_url, resp.status
            )));
        }

        let parsed_feed = parse_feed_bytes(&resp.bytes, &resp.final_url)?;
        let meta = extract_feed_metadata(&parsed_feed, &resp.final_url, None);
        let mut discovered_favicon = meta.favicon_url.clone();

        let candidates = prepare_candidates(&parsed_feed, &resp.final_url, &meta);
        let mut items = Vec::with_capacity(candidates.len().min(PREVIEW_MAX_ITEMS));

        for (idx, mut cand) in candidates.into_iter().take(PREVIEW_MAX_ITEMS).enumerate() {
            let mut full_content_extracted = false;
            if fetch_full_content
                && idx < PREVIEW_READABILITY_MAX_ITEMS
                && let Some(canon_url) = cand.canonical_url.clone()
                && let Some(extracted) = self
                    .readability
                    .fetch_and_extract(&self.http, &canon_url, use_proxy)
                    .await
            {
                apply_extracted_article(&mut cand, extracted, &mut discovered_favicon);
                full_content_extracted = true;
            }

            let filter_body = filter_body_text(&cand);
            let (matched_filter, filter_reason) = match &compiled_filter {
                Some(cf) => cf.evaluate(cand.title.as_deref(), Some(&filter_body)),
                None => (true, None),
            };

            let display_title = cand
                .title
                .clone()
                .or_else(|| cand.sanitized.first_sentence.clone())
                .unwrap_or_else(|| "(Untitled)".to_string());

            let content_html_preview = truncate_html_preview(&cand.sanitized.content_html);
            let media_urls = cand.media.iter().map(|m| m.url.clone()).collect();

            items.push(FeedPreviewItem {
                fingerprint_sha256: cand.fingerprint_sha256,
                title: display_title,
                canonical_url: cand.canonical_url,
                author: cand.author,
                published_at: cand.published_at.map(unix_to_rfc3339),
                first_sentence: cand.sanitized.first_sentence,
                summary_text: cand.sanitized.summary_text,
                content_html_preview,
                media_urls,
                full_content_extracted,
                matched_filter,
                filter_reason,
            });
        }

        Ok(FeedPreviewResponse {
            feed_title: meta.title,
            site_url: meta.site_url,
            favicon_url: discovered_favicon,
            items,
        })
    }

    /// Import an OPML 2.0 XML document into SQLite, skipping already-subscribed URLs.
    pub async fn import_opml(
        &self,
        opml_xml: &str,
        silent_init: bool,
    ) -> AppResult<OpmlImportResult> {
        import_opml_into_db(&self.db, opml_xml, silent_init).await
    }

    /// Export all persisted feeds from SQLite as an OPML 2.0 XML document.
    pub async fn export_opml(&self) -> AppResult<String> {
        let feeds = self.db.list_feeds().await?;
        export_opml(&feeds)
    }

    async fn record_poll_failure(
        &self,
        feed: &Feed,
        retry_after_secs: Option<u64>,
        error_msg: &str,
    ) -> AppResult<Feed> {
        let now = now_unix();
        let new_failures = feed.consecutive_failures.saturating_add(1);

        let (health_status, next_poll_at) = if new_failures >= self.circuit_breaker_threshold {
            (FeedHealthStatus::CircuitOpen, None)
        } else {
            let delay =
                compute_backoff_delay_secs(feed.poll_interval_secs, new_failures, retry_after_secs);
            (FeedHealthStatus::Backoff, Some(now.saturating_add(delay)))
        };

        self.db
            .record_feed_poll_failure(
                feed.id,
                new_failures,
                health_status,
                next_poll_at,
                error_msg,
            )
            .await
    }

    async fn load_existing_fingerprints(&self, feed_id: i64) -> AppResult<HashSet<String>> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT fingerprint_sha256 FROM entries WHERE feed_id = ?1")
                .bind(feed_id)
                .fetch_all(self.db.read_pool())
                .await?;
        Ok(rows.into_iter().map(|(fp,)| fp).collect())
    }

    async fn purge_entry_payload_now(&self, entry_id: i64, now: i64) -> AppResult<()> {
        sqlx::query(
            r#"
            UPDATE entries
            SET title = NULL,
                canonical_url = NULL,
                author = NULL,
                content_html = NULL,
                first_sentence = NULL,
                summary_text = NULL,
                media_json = NULL,
                payload_purged_at = ?1
            WHERE id = ?2
            "#,
        )
        .bind(now)
        .bind(entry_id)
        .execute(self.db.write_pool())
        .await?;
        Ok(())
    }

    async fn load_compiled_routes_for_feed(&self, feed: &Feed) -> AppResult<Vec<CompiledRoute>> {
        let enabled_routes = self.db.list_enabled_routes().await?;
        let mut out = Vec::new();

        for route in enabled_routes {
            if !route_matches_feed(&route, feed) {
                continue;
            }
            let rules = ContentFilterRules::from_route(&route);
            match rules.compile() {
                Ok(filter) => out.push(CompiledRoute { route, filter }),
                Err(err) => {
                    warn!(
                        route_id = route.id,
                        route_name = %route.name,
                        error = %err,
                        "skipping route with invalid content filter regex"
                    );
                }
            }
        }

        Ok(out)
    }
}

struct CompiledRoute {
    route: Route,
    filter: CompiledContentFilter,
}

#[derive(Debug, Clone, Default)]
struct DiscoveredFeedMeta {
    title: Option<String>,
    site_url: Option<String>,
    favicon_url: Option<String>,
    feed_author: Option<String>,
}

#[derive(Debug, Clone)]
struct CandidateEntry {
    fingerprint_sha256: String,
    published_at: Option<i64>,
    title: Option<String>,
    canonical_url: Option<String>,
    author: Option<String>,
    sanitized: SanitizedHtmlOutput,
    media: Vec<MediaAttachment>,
    already_exists: bool,
}

/// Compute exponential backoff with jitter: `min(base * 2^failures, 6h) + jitter`, honoring
/// upstream `Retry-After` when present (`429` / `503`).
pub fn compute_backoff_delay_secs(
    base_interval_secs: i64,
    consecutive_failures: i64,
    retry_after_secs: Option<u64>,
) -> i64 {
    let base = base_interval_secs.max(60);
    let shift = u32::try_from(consecutive_failures.max(1))
        .unwrap_or(20)
        .min(20);
    let exp_core = base
        .saturating_mul(1_i64 << shift)
        .min(MAX_BACKOFF_DELAY_SECS);

    let core_delay = if let Some(ra) = retry_after_secs {
        i64::try_from(ra)
            .unwrap_or(MAX_BACKOFF_DELAY_SECS)
            .clamp(1, MAX_BACKOFF_DELAY_SECS)
    } else {
        exp_core
    };

    let jitter_max = (core_delay / 10).clamp(1, 300);
    let jitter = rand::random_range(0..=jitter_max);
    core_delay.saturating_add(jitter)
}

/// Parse RSS 0.9x/1.0/2.0, Atom, or JSON Feed bytes with `feed-rs` configured NOT to synthesize
/// random UUIDs or pre-strip relative URLs before our custom URL resolver + `ammonia` pipeline.
pub fn parse_feed_bytes(bytes: &[u8], base_url: &Url) -> AppResult<feed_rs::model::Feed> {
    feed_rs::parser::Builder::new()
        .base_uri(Some(base_url.as_str()))
        .id_generator(|_, _, _| String::new())
        .sanitize_content(false)
        .build()
        .parse(Cursor::new(bytes))
        .map_err(|err| AppError::Upstream(format!("failed to parse feed from '{base_url}': {err}")))
}

fn extract_feed_metadata(
    parsed: &feed_rs::model::Feed,
    final_feed_url: &Url,
    existing_site_url: Option<&str>,
) -> DiscoveredFeedMeta {
    let title = parsed
        .title
        .as_ref()
        .map(|t| html_to_plain_text(&t.content))
        .and_then(|s| non_empty_string(&s));

    let site_url = parsed
        .links
        .iter()
        .find(|link| {
            link.target.is_none()
                && matches!(
                    link.rel.as_deref().map(str::to_ascii_lowercase).as_deref(),
                    None | Some("alternate")
                )
                && resolve_http_url(&link.href, final_feed_url).is_some()
        })
        .and_then(|link| resolve_http_url(&link.href, final_feed_url))
        .or_else(|| {
            parsed
                .links
                .iter()
                .find(|link| {
                    !matches!(
                        link.rel.as_deref().map(str::to_ascii_lowercase).as_deref(),
                        Some("self" | "hub" | "enclosure")
                    )
                })
                .and_then(|link| resolve_http_url(&link.href, final_feed_url))
        })
        .or_else(|| existing_site_url.and_then(|s| resolve_http_url(s, final_feed_url)));

    let favicon_base = site_url
        .as_deref()
        .and_then(|s| Url::parse(s).ok())
        .unwrap_or_else(|| final_feed_url.clone());

    let favicon_url = parsed
        .icon
        .as_ref()
        .and_then(|img| resolve_http_url(&img.uri, &favicon_base))
        .or_else(|| {
            parsed
                .logo
                .as_ref()
                .and_then(|img| resolve_http_url(&img.uri, &favicon_base))
        });

    let feed_author = extract_first_person(&parsed.authors);

    DiscoveredFeedMeta {
        title,
        site_url,
        favicon_url,
        feed_author,
    }
}

fn prepare_candidates(
    parsed_feed: &feed_rs::model::Feed,
    final_feed_url: &Url,
    meta: &DiscoveredFeedMeta,
) -> Vec<CandidateEntry> {
    let site_base_url = meta.site_url.as_deref().and_then(|s| Url::parse(s).ok());
    let mut seen_batch_fps = HashSet::new();
    let mut out = Vec::with_capacity(parsed_feed.entries.len());

    for entry in &parsed_feed.entries {
        let entry_xml_base = entry
            .base
            .as_deref()
            .and_then(|b| resolve_http_url(b, final_feed_url))
            .and_then(|s| Url::parse(&s).ok());

        let link_resolution_base = entry_xml_base
            .as_ref()
            .or(site_base_url.as_ref())
            .unwrap_or(final_feed_url);

        let raw_link = select_entry_primary_link(entry);
        let canonical_url = raw_link
            .as_deref()
            .and_then(|href| normalize_canonical_url(href, Some(link_resolution_base)));

        // Base URL fallback chain per Section 四.1: canonical_url -> xml:base -> site_url -> feed url.
        let effective_base = canonical_url
            .as_deref()
            .and_then(|s| Url::parse(s).ok())
            .or(entry_xml_base)
            .or_else(|| site_base_url.clone())
            .unwrap_or_else(|| final_feed_url.clone());

        let title = entry
            .title
            .as_ref()
            .map(|t| html_to_plain_text(&t.content))
            .and_then(|s| non_empty_string(&s));

        let author = extract_first_person(&entry.authors).or_else(|| meta.feed_author.clone());

        // Strictly use ONLY explicit feed-supplied timestamps; NEVER substitute `Utc::now()`.
        let published_at = entry.published.or(entry.updated).map(|dt| dt.timestamp());

        let raw_body_html = extract_entry_raw_html(entry);
        let sanitized = sanitize_and_extract_html(&raw_body_html, Some(&effective_base));

        let raw_id = non_empty_string(&entry.id);
        let plain_body = non_empty_string(&sanitized.plain_text);
        let fp_input = FingerprintInput {
            raw_id: raw_id.as_deref(),
            normalized_canonical_url: canonical_url.as_deref(),
            explicit_timestamp_unix: published_at,
            title: title.as_deref(),
            plain_body: plain_body.as_deref(),
        };
        let (fingerprint_sha256, _tier) = compute_entry_fingerprint(&fp_input);

        // Deduplicate identical fingerprints within a single malformed feed response.
        if !seen_batch_fps.insert(fingerprint_sha256.clone()) {
            continue;
        }

        let mut raw_media = extract_feed_entry_media(entry, &effective_base);
        raw_media.extend(sanitized.inline_media.clone());
        let media = deduplicate_media(raw_media);

        out.push(CandidateEntry {
            fingerprint_sha256,
            published_at,
            title,
            canonical_url,
            author,
            sanitized,
            media,
            already_exists: false,
        });
    }

    out
}

fn apply_extracted_article(
    cand: &mut CandidateEntry,
    extracted: ExtractedArticle,
    discovered_favicon: &mut Option<String>,
) {
    if cand.title.is_none() {
        cand.title = extracted.title;
    }
    if cand.author.is_none() {
        cand.author = extracted.byline;
    }
    if discovered_favicon.is_none() && extracted.favicon_url.is_some() {
        *discovered_favicon = extracted.favicon_url;
    }

    // Preserve explicit `<enclosure>` / MediaRSS attachments alongside Readability's inline media.
    let mut combined_media = extracted.sanitized.inline_media.clone();
    combined_media.append(&mut cand.media);
    cand.media = deduplicate_media(combined_media);
    cand.sanitized = extracted.sanitized;
}

fn select_entry_primary_link(entry: &feed_rs::model::Entry) -> Option<String> {
    if let Some(link) = entry.links.iter().find(|l| {
        l.target.is_none()
            && matches!(
                l.rel.as_deref().map(str::to_ascii_lowercase).as_deref(),
                None | Some("alternate")
            )
            && !l.href.trim().is_empty()
    }) {
        return Some(link.href.clone());
    }

    if let Some(link) = entry.links.iter().find(|l| {
        l.target.is_none()
            && !matches!(
                l.rel.as_deref().map(str::to_ascii_lowercase).as_deref(),
                Some("enclosure" | "self" | "replies" | "via" | "related")
            )
            && !l.href.trim().is_empty()
    }) {
        return Some(link.href.clone());
    }

    if let Some(src_link) = entry
        .content
        .as_ref()
        .and_then(|c| c.src.as_ref())
        .filter(|l| !l.href.trim().is_empty())
    {
        return Some(src_link.href.clone());
    }

    // In RSS 2.0, `<guid isPermaLink="true">https://...</guid>` is often the sole article URL when
    // `<link>` is omitted.
    let trimmed_id = entry.id.trim();
    if (trimmed_id.starts_with("http://") || trimmed_id.starts_with("https://"))
        && Url::parse(trimmed_id).is_ok()
    {
        return Some(trimmed_id.to_string());
    }

    None
}

fn extract_entry_raw_html(entry: &feed_rs::model::Entry) -> String {
    if let Some(content) = &entry.content
        && let Some(body) = content.body.as_deref()
        && !body.trim().is_empty()
    {
        return coerce_to_html_fragment(body, content.content_type.as_str());
    }

    if let Some(summary) = &entry.summary
        && !summary.content.trim().is_empty()
    {
        return coerce_to_html_fragment(&summary.content, summary.content_type.as_str());
    }

    String::new()
}

/// If a feed explicitly marks text as `text/plain` and it contains no HTML markup tags, wrap
/// paragraphs cleanly in `<p>` with HTML entity escaping so literal `<` or `&` characters are not
/// stripped by `ammonia`.
fn coerce_to_html_fragment(raw: &str, mime_str: &str) -> String {
    let is_plain_mime = mime_str.trim().eq_ignore_ascii_case("text/plain");
    if is_plain_mime && !looks_like_html(raw) {
        let mut out = String::with_capacity(raw.len() + 16);
        for para in raw.split("\n\n") {
            let trimmed = para.trim();
            if trimmed.is_empty() {
                continue;
            }
            out.push_str("<p>");
            for (line_idx, line) in trimmed.lines().enumerate() {
                if line_idx > 0 {
                    out.push_str("<br>");
                }
                push_escaped_html_text(&mut out, line);
            }
            out.push_str("</p>");
        }
        return out;
    }
    raw.to_string()
}

fn looks_like_html(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    for marker in [
        "<p",
        "<br",
        "<div",
        "<a ",
        "<img",
        "<ul",
        "<ol",
        "<li",
        "<h1",
        "<h2",
        "<h3",
        "<h4",
        "<pre",
        "<code",
        "<blockquote",
        "<table",
        "<figure",
        "<video",
        "<audio",
        "<span",
        "<em",
        "<strong",
        "<b>",
        "<i>",
    ] {
        if lower.contains(marker) {
            return true;
        }
    }
    false
}

fn push_escaped_html_text(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
}

fn extract_feed_entry_media(entry: &feed_rs::model::Entry, base_url: &Url) -> Vec<MediaAttachment> {
    let mut media = Vec::new();

    // 1. `<enclosure>` (mapped into `entry.media` by `feed-rs`), MediaRSS (`media:content`,
    // `media:thumbnail`), iTunes, and Podcast media objects.
    for obj in &entry.media {
        let obj_caption = obj
            .title
            .as_ref()
            .map(|t| html_to_plain_text(&t.content))
            .and_then(|s| non_empty_string(&s))
            .or_else(|| {
                obj.description
                    .as_ref()
                    .map(|d| html_to_plain_text(&d.content))
                    .and_then(|s| non_empty_string(&s))
            });

        let mut pushed_content = false;
        for mc in &obj.content {
            if let Some(url) = &mc.url
                && matches!(url.scheme(), "http" | "https")
            {
                let url_str = url.to_string();
                let mime = mc.content_type.as_ref().map(ToString::to_string);
                let kind = classify_media_kind(mime.as_deref(), &url_str);
                media.push(MediaAttachment {
                    url: url_str,
                    mime_type: mime,
                    kind,
                    length_bytes: mc.size,
                    caption: obj_caption.clone(),
                });
                pushed_content = true;
            }
        }

        if !pushed_content {
            for thumb in &obj.thumbnails {
                if let Some(resolved) = resolve_http_url(&thumb.image.uri, base_url) {
                    media.push(MediaAttachment {
                        mime_type: None,
                        kind: MediaAttachmentKind::Image,
                        url: resolved,
                        length_bytes: None,
                        caption: thumb
                            .image
                            .title
                            .as_deref()
                            .and_then(non_empty_string)
                            .or_else(|| obj_caption.clone()),
                    });
                }
            }
        }
    }

    // 2. Atom / JSON Feed `<link rel="enclosure" ...>` attachments.
    for link in &entry.links {
        if link
            .rel
            .as_deref()
            .is_some_and(|r| r.eq_ignore_ascii_case("enclosure"))
            && let Some(resolved) = resolve_http_url(&link.href, base_url)
        {
            let mime = link.media_type.as_deref().and_then(non_empty_string);
            let kind = classify_media_kind(mime.as_deref(), &resolved);
            media.push(MediaAttachment {
                url: resolved,
                mime_type: mime,
                kind,
                length_bytes: link.length,
                caption: link.title.as_deref().and_then(non_empty_string),
            });
        }
    }

    media
}

fn extract_first_person(people: &[feed_rs::model::Person]) -> Option<String> {
    for person in people {
        if let Some(name) = person.name.as_deref().and_then(non_empty_string) {
            return Some(name);
        }
        if let Some(email) = person.email.as_deref().and_then(non_empty_string) {
            return Some(email);
        }
    }
    None
}

fn filter_body_text(cand: &CandidateEntry) -> String {
    if !cand.sanitized.plain_text.trim().is_empty() {
        cand.sanitized.plain_text.clone()
    } else {
        cand.sanitized.summary_text.clone().unwrap_or_default()
    }
}

fn truncate_html_preview(html: &str) -> Option<String> {
    let trimmed = html.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().count() <= PREVIEW_HTML_MAX_CHARS {
        Some(trimmed.to_string())
    } else {
        let truncated: String = trimmed.chars().take(PREVIEW_HTML_MAX_CHARS).collect();
        Some(truncated)
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

fn non_empty_string(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
