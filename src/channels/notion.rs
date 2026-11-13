//! Notion Data Source channel adapter (`Notion-Version: 2026-03-11`, Markdown v2 normalizer,
//! dual database/schema onboarding helpers, and crash-safe effective-once outbox delivery).
//!
//! Aligns strictly with `feedcourier.md` Section 五.3 & 三.3:
//! - Every API call targets `Notion-Version: 2026-03-11` (`data_source_id` parent model,
//!   `initial_data_source.properties` database provisioning, `in_trash` instead of removed `archived`).
//! - Article bodies are converted via `htmd` + `normalize_markdown_v2` (dynamically expanding code
//!   fences containing `>= 3` consecutive backticks and truncating cleanly at block boundaries
//!   below Notion's 5,000-block limit).
//! - Effective-once semantics are enforced through `DeliveryCheckpoint::Notion` (`create_initiated`,
//!   `async_task_id` polling, and `FC-ID` fingerprint reconciliation prior to retrying creation).

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Deref;
use std::sync::RwLock;
use std::time::Duration;

use async_trait::async_trait;
use htmd::HtmlToMarkdown;
use htmd::options::{
    BrStyle, BulletListMarker, CodeBlockFence, CodeBlockStyle, HeadingStyle, HrStyle, LinkStyle,
    Options as HtmdOptions, TranslationMode,
};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, RETRY_AFTER};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use url::Url;

use crate::channels::{
    ChannelConfig, ChannelKind, ChannelSender, DeliveryCheckpoint, DeliveryContext,
    DeliveryOutcome, NotionChannelConfig, NotionColumnMapping,
};
use crate::config::{APP_NAME, VERSION};
use crate::db::models::{Entry, Feed, unix_to_rfc3339};
use crate::error::{AppError, AppResult};

/// Pinned Notion REST API version header (`2026-03-11`).
pub const NOTION_API_VERSION: &str = "2026-03-11";

/// Default Notion REST API base URL.
pub const DEFAULT_NOTION_API_BASE_URL: &str = "https://api.notion.com";

/// Default database title when provisioning a new 7-column Notion database.
pub const DEFAULT_NOTION_DB_TITLE: &str = "FeedCourier Inbox";

/// Safety cap below Notion's hard 5,000-block single-write validation limit.
pub const MAX_NOTION_BLOCKS: usize = 4_800;

/// Maximum Markdown body payload size in UTF-8 bytes before block-boundary truncation.
pub const MAX_MARKDOWN_BYTES: usize = 450_000;

/// Block count threshold above which `POST /v1/pages` enables `"allow_async": true`.
pub const ASYNC_BLOCK_THRESHOLD: usize = 250;

/// Markdown byte threshold (`24 KiB`) above which `POST /v1/pages` enables `"allow_async": true`.
pub const ASYNC_BYTE_THRESHOLD: usize = 24 * 1024;

/// Maximum characters Notion accepts in a single `title` / `rich_text` text fragment.
const MAX_NOTION_TEXT_FRAGMENT_CHARS: usize = 2_000;

/// Maximum characters Notion accepts in a `select` / `multi_select` option name.
const MAX_NOTION_SELECT_CHARS: usize = 100;

/// Default per-request HTTP timeout for Notion API calls.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Default delay between bounded inline polls of `GET /v1/async_tasks/{id}`.
const DEFAULT_ASYNC_POLL_DELAY: Duration = Duration::from_millis(600);

/// Default number of immediate inline poll attempts right after receiving `202 Accepted`.
const DEFAULT_INITIAL_ASYNC_POLLS: usize = 2;

/// Default number of inline poll attempts when resuming a task with an existing `async_task_id`.
const DEFAULT_RESUME_ASYNC_POLLS: usize = 3;

// ---------------------------------------------------------------------------
// Onboarding & Schema Inspection Contracts (1-to-1 with `web/src/api/types.ts`)
// ---------------------------------------------------------------------------

/// `POST /api/v1/channels/notion/init-db` request payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotionInitDbRequest {
    pub secret: String,
    pub parent_page_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub use_proxy: bool,
}

/// `POST /api/v1/channels/notion/init-db` response payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotionInitDbResponse {
    pub database_id: String,
    pub data_source_id: String,
    pub database_url: Option<String>,
    pub column_mapping: NotionColumnMapping,
}

/// Canonical standard column identifiers for FeedCourier's 7-column Notion schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotionStandardFieldKey {
    Title,
    FcId,
    Link,
    Source,
    Published,
    State,
    Tag,
}

impl NotionStandardFieldKey {
    /// Canonical standard Notion column name.
    pub const fn standard_name(self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::FcId => "FC-ID",
            Self::Link => "Link",
            Self::Source => "Source",
            Self::Published => "Published",
            Self::State => "State",
            Self::Tag => "Tag",
        }
    }

    /// Expected Notion data source property type.
    pub const fn expected_type(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::FcId => "rich_text",
            Self::Link => "url",
            Self::Source => "select",
            Self::Published => "date",
            Self::State => "select",
            Self::Tag => "multi_select",
        }
    }

    /// Returns `true` strictly for mandatory fields (`Title` and `FC-ID`).
    pub const fn is_required(self) -> bool {
        matches!(self, Self::Title | Self::FcId)
    }

    /// Ordered list of all 7 standard columns.
    pub const ALL: [Self; 7] = [
        Self::Title,
        Self::FcId,
        Self::Link,
        Self::Source,
        Self::Published,
        Self::State,
        Self::Tag,
    ];
}

/// Schema alignment status for an individual standard column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotionColumnCheckStatus {
    Matched,
    Mismatched,
    Missing,
}

/// Summary of an existing property present on a Notion data source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotionPropertySummary {
    pub name: String,
    pub property_type: String,
}

/// Inspection status of one standard FeedCourier column against a Notion data source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotionColumnCheckItem {
    pub field: NotionStandardFieldKey,
    pub standard_name: String,
    pub expected_type: String,
    pub actual_type: Option<String>,
    pub required: bool,
    pub status: NotionColumnCheckStatus,
    pub mapped_property: Option<String>,
}

/// `POST /api/v1/channels/notion/inspect-schema` request payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotionInspectSchemaRequest {
    pub secret: String,
    pub data_source_or_db_id: String,
    #[serde(default)]
    pub patch_missing: Option<bool>,
    #[serde(default)]
    pub use_proxy: bool,
    #[serde(default)]
    pub channel_id: Option<i64>,
}

/// `POST /api/v1/channels/notion/inspect-schema` response payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotionInspectSchemaResponse {
    pub data_source_id: String,
    pub database_id: Option<String>,
    pub title: Option<String>,
    pub columns: Vec<NotionColumnCheckItem>,
    pub available_properties: Vec<NotionPropertySummary>,
    pub suggested_mapping: NotionColumnMapping,
    pub patched_columns: Vec<String>,
    pub ready: bool,
}

// ---------------------------------------------------------------------------
// Notion ID & URL Normalization
// ---------------------------------------------------------------------------

/// Extract and normalize a Notion UUID (`8-4-4-4-12` lowercase) from a raw Notion URL,
/// 32-char hex string, or hyphenated UUID. Clean non-URL synthetic IDs (e.g. in mock tests)
/// are preserved when valid.
pub fn normalize_notion_id(raw: &str) -> AppResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::Validation(
            "Notion identifier or URL must not be empty".to_string(),
        ));
    }

    // 1. Direct 32-hex or hyphenated UUID.
    if let Some(uuid) = parse_exact_32_hex(trimmed) {
        return Ok(uuid);
    }

    // 2. Full URL (`https://www.notion.so/...`, `notion.site`, `collection://...`).
    let looks_like_url = trimmed.contains("://")
        || trimmed.starts_with("www.notion.")
        || trimmed.contains("notion.so/")
        || trimmed.contains("notion.site/");

    if looks_like_url {
        let candidate_url = if trimmed.contains("://") {
            trimmed.to_string()
        } else {
            format!("https://{trimmed}")
        };
        let parsed = Url::parse(&candidate_url).map_err(|err| {
            AppError::Validation(format!("invalid Notion URL '{trimmed}': {err}"))
        })?;

        // Check URL host first for `collection://<uuid>` links.
        if let Some(host) = parsed.host_str()
            && let Some(uuid) = extract_trailing_uuid_from_segment(host)
        {
            return Ok(uuid);
        }

        // Inspect path segments from right to left so workspace slugs never shadow the leaf page/DB ID.
        if let Some(segments) = parsed.path_segments() {
            for seg in segments.rev() {
                if let Some(uuid) = extract_trailing_uuid_from_segment(seg) {
                    return Ok(uuid);
                }
            }
        }

        // Fallback to `?p=<page_id>` peek links when path carries no UUID.
        for (k, v) in parsed.query_pairs() {
            if k == "p"
                && let Some(uuid) = extract_trailing_uuid_from_segment(&v)
            {
                return Ok(uuid);
            }
        }

        return Err(AppError::Validation(format!(
            "could not extract a 32-character Notion UUID from URL '{trimmed}'"
        )));
    }

    // 3. Slug-prefixed string pasted without scheme, e.g. `FeedCourier-Inbox-a1b2c3d4...`.
    if let Some(uuid) = extract_trailing_uuid_from_segment(trimmed) {
        return Ok(uuid);
    }

    // 4. Allow clean synthetic identifiers for local mock testing while rejecting malformed inputs.
    if trimmed.len() <= 128
        && trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Ok(trimmed.to_string());
    }

    Err(AppError::Validation(format!(
        "invalid Notion identifier '{trimmed}' (expected a Notion URL or 32-hex UUID)"
    )))
}

fn parse_exact_32_hex(input: &str) -> Option<String> {
    if !input.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return None;
    }
    let hex_only: String = input
        .chars()
        .filter(|c| *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if hex_only.len() == 32 {
        Some(format_hyphenated_uuid(&hex_only))
    } else {
        None
    }
}

fn extract_trailing_uuid_from_segment(segment: &str) -> Option<String> {
    let seg = segment.trim().trim_matches('/');
    if seg.is_empty() {
        return None;
    }
    if let Some(uuid) = parse_exact_32_hex(seg) {
        return Some(uuid);
    }

    // Check trailing 36-char hyphenated UUID (`...-8-4-4-4-12`).
    if seg.len() >= 36 {
        let tail36 = &seg[seg.len() - 36..];
        let bytes = tail36.as_bytes();
        if bytes[8] == b'-'
            && bytes[13] == b'-'
            && bytes[18] == b'-'
            && bytes[23] == b'-'
            && let Some(uuid) = parse_exact_32_hex(tail36)
        {
            return Some(uuid);
        }
    }

    // Check trailing 32-char compact hex UUID (`Slug-Name-a1b2c3d4e5f67890abcdef1234567890`).
    if seg.len() >= 32 {
        let tail32 = &seg[seg.len() - 32..];
        if tail32.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(format_hyphenated_uuid(&tail32.to_ascii_lowercase()));
        }
    }

    None
}

fn format_hyphenated_uuid(hex32: &str) -> String {
    debug_assert_eq!(hex32.len(), 32);
    format!(
        "{}-{}-{}-{}-{}",
        &hex32[0..8],
        &hex32[8..12],
        &hex32[12..16],
        &hex32[16..20],
        &hex32[20..32]
    )
}

// ---------------------------------------------------------------------------
// Markdown v2 Normalizer & 5,000-Block Truncation
// ---------------------------------------------------------------------------

/// Result of converting & normalizing article content for Notion `markdown_version: "v2"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedMarkdown {
    /// Notion-v2-safe Markdown body.
    pub markdown: String,
    /// Estimated logical Notion block count after any truncation.
    pub block_count: usize,
    /// Whether the body was truncated to fit `MAX_NOTION_BLOCKS` or `MAX_MARKDOWN_BYTES`.
    pub truncated: bool,
}

impl NormalizedMarkdown {
    /// Returns `true` when `POST /v1/pages` should set `"allow_async": true`.
    pub const fn should_allow_async(&self) -> bool {
        self.block_count > ASYNC_BLOCK_THRESHOLD || self.markdown.len() > ASYNC_BYTE_THRESHOLD
    }
}

impl Deref for NormalizedMarkdown {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.markdown
    }
}

impl AsRef<str> for NormalizedMarkdown {
    fn as_ref(&self) -> &str {
        &self.markdown
    }
}

impl fmt::Display for NormalizedMarkdown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.markdown)
    }
}

/// Convert sanitized HTML into clean CommonMark/GFM using `htmd`.
pub fn html_to_markdown(html: &str) -> String {
    if html.trim().is_empty() {
        return String::new();
    }

    let options = HtmdOptions {
        heading_style: HeadingStyle::Atx,
        hr_style: HrStyle::Dashes,
        br_style: BrStyle::TwoSpaces,
        link_style: LinkStyle::Inlined,
        code_block_style: CodeBlockStyle::Fenced,
        code_block_fence: CodeBlockFence::Backticks,
        bullet_list_marker: BulletListMarker::Dash,
        ul_bullet_spacing: 1,
        ol_number_spacing: 1,
        preformatted_code: false,
        translation_mode: TranslationMode::Pure,
        ..HtmdOptions::default()
    };

    let converter = HtmlToMarkdown::builder()
        .options(options)
        .skip_tags(vec!["script", "style", "svg", "template"])
        .build();

    converter.convert(html).unwrap_or_default()
}

/// Convert sanitized HTML to Notion `markdown_version: "v2"` with fence expansion and block cap.
pub fn html_to_notion_markdown_v2(html: &str, canonical_url: Option<&str>) -> NormalizedMarkdown {
    let raw_md = html_to_markdown(html);
    normalize_markdown_v2(&raw_md, canonical_url)
}

/// Extract and normalize the best available body for a feed `Entry` (`content_html`, falling back
/// to `summary_text` or `first_sentence`).
pub fn entry_to_notion_markdown_v2(entry: &Entry) -> NormalizedMarkdown {
    let canonical = entry
        .canonical_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty());

    if let Some(html) = entry
        .content_html
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        let converted = html_to_markdown(html);
        if !converted.trim().is_empty() {
            return normalize_markdown_v2(&converted, canonical);
        }
    }

    if let Some(summary) = entry
        .summary_text
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return normalize_markdown_v2(summary, canonical);
    }

    if let Some(first) = entry
        .first_sentence
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return normalize_markdown_v2(first, canonical);
    }

    NormalizedMarkdown {
        markdown: String::new(),
        block_count: 0,
        truncated: false,
    }
}

/// Normalize Markdown for Notion `markdown_version: "v2"` using default safety caps
/// (`MAX_NOTION_BLOCKS = 4_800`, `MAX_MARKDOWN_BYTES = 450_000`).
pub fn normalize_markdown_v2(markdown: &str, canonical_url: Option<&str>) -> NormalizedMarkdown {
    normalize_markdown_v2_with_limits(
        markdown,
        canonical_url,
        MAX_NOTION_BLOCKS,
        MAX_MARKDOWN_BYTES,
    )
}

/// Normalize Markdown for Notion `markdown_version: "v2"` with explicit block and byte caps:
/// 1. Expands fenced code blocks whose body contains `k >= 3` consecutive backticks so outer
///    opening/closing fences use `max(3, max_inner_backtick_run + 1)` backticks.
/// 2. Counts logical Markdown blocks and truncates cleanly on a block boundary before exceeding
///    `max_blocks` or `max_bytes`, appending a canonical-link truncation footer.
pub fn normalize_markdown_v2_with_limits(
    markdown: &str,
    canonical_url: Option<&str>,
    max_blocks: usize,
    max_bytes: usize,
) -> NormalizedMarkdown {
    let normalized_newlines = markdown.replace("\r\n", "\n").replace('\r', "\n");
    if normalized_newlines.trim().is_empty() {
        return NormalizedMarkdown {
            markdown: String::new(),
            block_count: 0,
            truncated: false,
        };
    }

    let segments = parse_markdown_segments(&normalized_newlines);
    if segments.is_empty() {
        return NormalizedMarkdown {
            markdown: String::new(),
            block_count: 0,
            truncated: false,
        };
    }

    let total_blocks: usize = segments.iter().map(|s| s.notion_blocks).sum();
    let full_rendered = render_segments(&segments);

    if total_blocks <= max_blocks && full_rendered.len() <= max_bytes {
        return NormalizedMarkdown {
            markdown: full_rendered,
            block_count: total_blocks,
            truncated: false,
        };
    }

    let footer = build_truncation_footer(canonical_url);
    let footer_blocks = 2usize; // `---` divider + `> ...` quote block
    let block_budget = max_blocks.saturating_sub(footer_blocks);
    let byte_budget = max_bytes.saturating_sub(footer.len());

    let mut out = String::new();
    let mut kept_blocks = 0usize;

    for (idx, seg) in segments.iter().enumerate() {
        let sep = if idx == 0 {
            ""
        } else if seg.join_tight {
            "\n"
        } else {
            "\n\n"
        };

        if kept_blocks + seg.notion_blocks > block_budget
            || out.len() + sep.len() + seg.rendered.len() > byte_budget
        {
            // Edge case: if the very first block exceeds `byte_budget` while `block_budget >= 1`,
            // slice safely on a UTF-8 character boundary so we still include partial leading text.
            if idx == 0 && block_budget >= 1 && byte_budget > 16 && !seg.is_code_fence {
                let sliced = truncate_utf8_prefix(&seg.rendered, byte_budget);
                if !sliced.trim().is_empty() {
                    out.push_str(sliced.trim_end());
                    kept_blocks += 1;
                }
            }
            break;
        }

        out.push_str(sep);
        out.push_str(&seg.rendered);
        kept_blocks += seg.notion_blocks;
    }

    let final_md = if out.is_empty() {
        footer.trim_start_matches('\n').to_string()
    } else {
        format!("{out}{footer}")
    };

    NormalizedMarkdown {
        markdown: final_md,
        block_count: kept_blocks + footer_blocks,
        truncated: true,
    }
}

fn build_truncation_footer(canonical_url: Option<&str>) -> String {
    match canonical_url.map(str::trim).filter(|u| !u.is_empty()) {
        Some(url) => format!(
            "\n\n---\n\n> Content truncated due to Notion block limit. [Read full original article]({url})"
        ),
        None => "\n\n---\n\n> Content truncated due to Notion block limit.".to_string(),
    }
}

fn truncate_utf8_prefix(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[derive(Debug, Clone)]
struct MarkdownSegment {
    rendered: String,
    notion_blocks: usize,
    join_tight: bool,
    is_code_fence: bool,
}

fn render_segments(segments: &[MarkdownSegment]) -> String {
    let mut out = String::new();
    for (idx, seg) in segments.iter().enumerate() {
        if idx > 0 {
            if seg.join_tight {
                out.push('\n');
            } else {
                out.push_str("\n\n");
            }
        }
        out.push_str(&seg.rendered);
    }
    out
}

#[derive(Debug, Clone, Copy)]
struct FenceOpen<'a> {
    indent: &'a str,
    ch: char,
    len: usize,
    info: &'a str,
}

fn parse_fence_open(line: &str, max_indent: usize) -> Option<FenceOpen<'_>> {
    let leading_spaces = line.bytes().take_while(|b| *b == b' ').count();
    if leading_spaces > max_indent {
        return None;
    }
    let indent = &line[..leading_spaces];
    let rest = &line[leading_spaces..];
    let first = rest.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let run_len = rest.bytes().take_while(|b| *b == first as u8).count();
    if run_len < 3 {
        return None;
    }
    let info = rest[run_len..].trim();
    // CommonMark §4.5: backtick code fence info strings cannot contain backticks.
    if first == '`' && info.contains('`') {
        return None;
    }
    Some(FenceOpen {
        indent,
        ch: first,
        len: run_len,
        info,
    })
}

fn is_fence_close(line: &str, ch: char, min_len: usize, max_indent: usize) -> bool {
    let leading_spaces = line.bytes().take_while(|b| *b == b' ').count();
    if leading_spaces > max_indent {
        return false;
    }
    let rest = &line[leading_spaces..];
    let run_len = rest.bytes().take_while(|b| *b == ch as u8).count();
    run_len >= min_len && rest[run_len..].trim().is_empty()
}

/// Determine whether treating an inner opening fence line (e.g. ```` ```rust ```` inside
/// ```` ```markdown ````) as a nested pair leaves all fences across the rest of `rem_lines`
/// cleanly balanced at EOF.
fn nested_fence_balances(
    rem_lines: &[&str],
    ch: char,
    open_len: usize,
    start_depth: usize,
    max_indent: usize,
) -> bool {
    let mut depth = start_depth;
    let mut top_fence: Option<(char, usize)> = None;

    for &line in rem_lines {
        if depth > 0 {
            if is_fence_close(line, ch, open_len, max_indent) {
                depth -= 1;
            } else if let Some(inner) = parse_fence_open(line, max_indent)
                && inner.ch == ch
                && inner.len >= open_len
                && !inner.info.is_empty()
            {
                depth += 1;
            }
        } else if let Some((top_ch, top_len)) = top_fence {
            if is_fence_close(line, top_ch, top_len, 3) {
                top_fence = None;
            }
        } else if let Some(next_open) = parse_fence_open(line, 3) {
            top_fence = Some((next_open.ch, next_open.len));
        }
    }

    depth == 0 && top_fence.is_none()
}

fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0usize;
    let mut current = 0usize;
    for b in text.bytes() {
        if b == b'`' {
            current += 1;
            if current > longest {
                longest = current;
            }
        } else {
            current = 0;
        }
    }
    longest
}

fn format_expanded_code_fence(
    indent: &str,
    orig_ch: char,
    orig_len: usize,
    info: &str,
    body_lines: &[&str],
) -> String {
    let max_inner = body_lines
        .iter()
        .map(|line| longest_backtick_run(line))
        .max()
        .unwrap_or(0);

    let base_len = if orig_ch == '`' { orig_len.max(3) } else { 3 };
    let target_len = base_len.max(max_inner + 1);
    let fence = "`".repeat(target_len);

    let mut out = String::new();
    out.push_str(indent);
    out.push_str(&fence);
    if !info.is_empty() {
        out.push_str(info);
    }
    out.push('\n');
    if !body_lines.is_empty() {
        out.push_str(&body_lines.join("\n"));
        out.push('\n');
    }
    out.push_str(indent);
    out.push_str(&fence);
    out
}

fn consume_fenced_block<'a>(
    lines: &[&'a str],
    start: usize,
    open: FenceOpen<'a>,
    max_indent: usize,
) -> (String, usize) {
    let mut nested_depth = 0usize;
    let mut close_idx = None;
    let mut j = start + 1;

    while j < lines.len() {
        let line = lines[j];
        if is_fence_close(line, open.ch, open.len, max_indent) {
            if nested_depth == 0 {
                close_idx = Some(j);
                break;
            }
            nested_depth -= 1;
        } else if let Some(inner) = parse_fence_open(line, max_indent)
            && inner.ch == open.ch
            && inner.len >= open.len
            && !inner.info.is_empty()
            && nested_fence_balances(
                &lines[j + 1..],
                open.ch,
                open.len,
                nested_depth + 2,
                max_indent,
            )
        {
            nested_depth += 1;
        }
        j += 1;
    }

    let (body_end, next_idx) = match close_idx {
        Some(c) => (c, c + 1),
        None => (lines.len(), lines.len()),
    };

    let rendered = format_expanded_code_fence(
        open.indent,
        open.ch,
        open.len,
        open.info,
        &lines[start + 1..body_end],
    );
    (rendered, next_idx)
}

fn is_thematic_break(line: &str) -> bool {
    let leading_spaces = line.bytes().take_while(|b| *b == b' ').count();
    if leading_spaces > 3 {
        return false;
    }
    let trimmed = line[leading_spaces..].trim_end();
    let Some(marker) = trimmed.chars().next() else {
        return false;
    };
    if !matches!(marker, '-' | '*' | '_') {
        return false;
    }
    let mut count = 0usize;
    for ch in trimmed.chars() {
        if ch == marker {
            count += 1;
        } else if ch != ' ' && ch != '\t' {
            return false;
        }
    }
    count >= 3
}

fn is_atx_heading(line: &str) -> bool {
    let leading_spaces = line.bytes().take_while(|b| *b == b' ').count();
    if leading_spaces > 3 {
        return false;
    }
    let rest = &line[leading_spaces..];
    let hashes = rest.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&hashes) {
        return false;
    }
    let after = &rest[hashes..];
    after.is_empty() || after.starts_with(' ') || after.starts_with('\t')
}

fn is_setext_underline(line: &str) -> bool {
    let leading_spaces = line.bytes().take_while(|b| *b == b' ').count();
    if leading_spaces > 3 {
        return false;
    }
    let trimmed = line[leading_spaces..].trim_end();
    if trimmed.is_empty() {
        return false;
    }
    let first = trimmed.as_bytes()[0];
    (first == b'=' || first == b'-') && trimmed.bytes().all(|b| b == first)
}

fn is_blockquote_line(line: &str) -> bool {
    let leading_spaces = line.bytes().take_while(|b| *b == b' ').count();
    leading_spaces <= 3 && line[leading_spaces..].starts_with('>')
}

fn list_marker_content_indent(line: &str) -> Option<usize> {
    if is_thematic_break(line) {
        return None;
    }
    let indent = line.bytes().take_while(|b| *b == b' ').count();
    let rest = &line[indent..];
    let bytes = rest.as_bytes();
    let first = *bytes.first()?;

    if matches!(first, b'-' | b'*' | b'+') {
        if bytes.len() == 1 {
            return Some(indent + 2);
        }
        if bytes[1] == b' ' || bytes[1] == b'\t' {
            let spaces = rest[1..].bytes().take_while(|b| *b == b' ').count().max(1);
            return Some(indent + 1 + spaces);
        }
        return None;
    }

    if first.is_ascii_digit() {
        let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
        if (1..=9).contains(&digits) && bytes.len() > digits && matches!(bytes[digits], b'.' | b')')
        {
            if bytes.len() == digits + 1 {
                return Some(indent + digits + 2);
            }
            if bytes[digits + 1] == b' ' || bytes[digits + 1] == b'\t' {
                let spaces = rest[digits + 1..]
                    .bytes()
                    .take_while(|b| *b == b' ')
                    .count()
                    .max(1);
                return Some(indent + digits + 1 + spaces);
            }
        }
    }

    None
}

fn is_table_delimiter_row(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.contains('|') || !trimmed.contains('-') {
        return false;
    }
    let inner = trimmed.trim_matches('|');
    let cells: Vec<&str> = inner.split('|').collect();
    if cells.is_empty() {
        return false;
    }
    cells.iter().all(|cell| {
        let c = cell.trim();
        if c.is_empty() {
            return false;
        }
        let core = c.trim_start_matches(':').trim_end_matches(':');
        core.len() >= 3 && core.bytes().all(|b| b == b'-')
    })
}

fn is_table_start(lines: &[&str], idx: usize) -> bool {
    idx + 1 < lines.len()
        && lines[idx].contains('|')
        && !lines[idx].trim().is_empty()
        && is_table_delimiter_row(lines[idx + 1])
}

fn is_new_block_starter(lines: &[&str], idx: usize) -> bool {
    let line = lines[idx];
    line.trim().is_empty()
        || parse_fence_open(line, 3).is_some()
        || is_atx_heading(line)
        || is_thematic_break(line)
        || is_blockquote_line(line)
        || list_marker_content_indent(line).is_some()
        || is_table_start(lines, idx)
}

fn parse_markdown_segments(markdown: &str) -> Vec<MarkdownSegment> {
    let lines: Vec<&str> = markdown.split('\n').collect();
    let mut segments: Vec<MarkdownSegment> = Vec::new();
    let mut i = 0usize;
    let mut had_blank = false;
    let mut prev_was_list_item = false;

    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() {
            had_blank = true;
            i += 1;
            continue;
        }

        // 1. Fenced code block (` ``` ` or `~~~`).
        if let Some(open) = parse_fence_open(line, 3) {
            let (rendered, next_i) = consume_fenced_block(&lines, i, open, 3);
            segments.push(MarkdownSegment {
                rendered,
                notion_blocks: 1,
                join_tight: false,
                is_code_fence: true,
            });
            i = next_i;
            had_blank = false;
            prev_was_list_item = false;
            continue;
        }

        // 2. ATX Heading (`# ...`).
        if is_atx_heading(line) {
            segments.push(MarkdownSegment {
                rendered: line.trim_end().to_string(),
                notion_blocks: 1,
                join_tight: false,
                is_code_fence: false,
            });
            i += 1;
            had_blank = false;
            prev_was_list_item = false;
            continue;
        }

        // 3. Thematic break (`---`, `***`, `___`).
        if is_thematic_break(line) {
            segments.push(MarkdownSegment {
                rendered: line.trim().to_string(),
                notion_blocks: 1,
                join_tight: false,
                is_code_fence: false,
            });
            i += 1;
            had_blank = false;
            prev_was_list_item = false;
            continue;
        }

        // 4. GFM Table (`| H1 | H2 |\n| --- | --- |\n| ... |`).
        if is_table_start(&lines, i) {
            let header_and_delim = format!("{}\n{}", lines[i].trim_end(), lines[i + 1].trim_end());
            segments.push(MarkdownSegment {
                rendered: header_and_delim,
                notion_blocks: 2, // 1 `table` + 1 header `table_row`
                join_tight: false,
                is_code_fence: false,
            });
            i += 2;
            while i < lines.len() {
                let row = lines[i];
                if row.trim().is_empty()
                    || !row.contains('|')
                    || parse_fence_open(row, 3).is_some()
                    || is_atx_heading(row)
                    || is_thematic_break(row)
                {
                    break;
                }
                segments.push(MarkdownSegment {
                    rendered: row.trim_end().to_string(),
                    notion_blocks: 1,
                    join_tight: true,
                    is_code_fence: false,
                });
                i += 1;
            }
            had_blank = false;
            prev_was_list_item = false;
            continue;
        }

        // 5. Blockquote (`> ...`).
        if is_blockquote_line(line) {
            let mut quote_lines = vec![line.trim_end()];
            i += 1;
            while i < lines.len() && is_blockquote_line(lines[i]) {
                quote_lines.push(lines[i].trim_end());
                i += 1;
            }
            segments.push(MarkdownSegment {
                rendered: quote_lines.join("\n"),
                notion_blocks: 1,
                join_tight: false,
                is_code_fence: false,
            });
            had_blank = false;
            prev_was_list_item = false;
            continue;
        }

        // 6. List item (`- ...`, `* ...`, `1. ...`), including indented continuations & nested fences.
        if let Some(content_indent) = list_marker_content_indent(line) {
            let mut item_lines: Vec<String> = vec![line.trim_end().to_string()];
            let mut extra_blocks = 0usize;
            i += 1;

            while i < lines.len() {
                let next_line = lines[i];
                if next_line.trim().is_empty() {
                    break;
                }
                let next_indent = next_line.bytes().take_while(|b| *b == b' ').count();
                if next_indent >= content_indent
                    && let Some(nested_open) = parse_fence_open(next_line, content_indent + 4)
                {
                    let (fenced, next_i) =
                        consume_fenced_block(&lines, i, nested_open, content_indent + 4);
                    item_lines.push(fenced);
                    extra_blocks += 1;
                    i = next_i;
                    continue;
                }
                if list_marker_content_indent(next_line).is_some() {
                    break;
                }
                if next_indent >= content_indent {
                    item_lines.push(next_line.trim_end().to_string());
                    i += 1;
                } else {
                    break;
                }
            }

            let join_tight = prev_was_list_item && !had_blank;
            segments.push(MarkdownSegment {
                rendered: item_lines.join("\n"),
                notion_blocks: 1 + extra_blocks,
                join_tight,
                is_code_fence: false,
            });
            had_blank = false;
            prev_was_list_item = true;
            continue;
        }

        // 7. Paragraph (or Setext heading).
        let mut para_lines = vec![line.trim_end()];
        i += 1;
        while i < lines.len() {
            if is_setext_underline(lines[i]) {
                para_lines.push(lines[i].trim_end());
                i += 1;
                break;
            }
            if is_new_block_starter(&lines, i) {
                break;
            }
            para_lines.push(lines[i].trim_end());
            i += 1;
        }

        segments.push(MarkdownSegment {
            rendered: para_lines.join("\n"),
            notion_blocks: 1,
            join_tight: false,
            is_code_fence: false,
        });
        had_blank = false;
        prev_was_list_item = false;
    }

    segments
}

// ---------------------------------------------------------------------------
// Notion API Client & `ChannelSender` Implementation
// ---------------------------------------------------------------------------

/// Asynchronous Notion channel adapter targeting `Notion-Version: 2026-03-11`.
#[derive(Debug)]
pub struct NotionSender {
    api_base_url: String,
    direct_client: reqwest::Client,
    env_proxy_client: reqwest::Client,
    outbound_proxy: RwLock<Option<(String, reqwest::Client)>>,
    request_timeout: Duration,
    async_poll_delay: Duration,
    initial_async_polls: usize,
    resume_async_polls: usize,
}

impl NotionSender {
    /// Construct a `NotionSender` pointing to `https://api.notion.com` with an optional
    /// global HTTP/SOCKS5 outbound proxy URL.
    pub fn new(outbound_proxy: Option<String>) -> AppResult<Self> {
        Self::with_base_url(DEFAULT_NOTION_API_BASE_URL, outbound_proxy)
    }

    /// Construct a `NotionSender` with a custom base URL (e.g. `wiremock` server URI in tests)
    /// and an optional global HTTP/SOCKS5 outbound proxy URL.
    pub fn with_base_url(
        api_base_url: impl Into<String>,
        outbound_proxy: Option<String>,
    ) -> AppResult<Self> {
        let base = api_base_url.into().trim().trim_end_matches('/').to_string();
        if base.is_empty() {
            return Err(AppError::Config(
                "Notion API base URL must not be empty".to_string(),
            ));
        }

        let direct_client = build_reqwest_client(DEFAULT_REQUEST_TIMEOUT, None, false)?;
        let env_proxy_client = build_reqwest_client(DEFAULT_REQUEST_TIMEOUT, None, true)?;
        let proxy_pair = match outbound_proxy
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            Some(proxy_url) => {
                let client =
                    build_reqwest_client(DEFAULT_REQUEST_TIMEOUT, Some(&proxy_url), false)?;
                Some((proxy_url, client))
            }
            None => None,
        };

        Ok(Self {
            api_base_url: base,
            direct_client,
            env_proxy_client,
            outbound_proxy: RwLock::new(proxy_pair),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            async_poll_delay: DEFAULT_ASYNC_POLL_DELAY,
            initial_async_polls: DEFAULT_INITIAL_ASYNC_POLLS,
            resume_async_polls: DEFAULT_RESUME_ASYNC_POLLS,
        })
    }

    /// Override inline polling cadence for `202 Accepted` / `GET /v1/async_tasks/{id}`.
    pub fn with_async_poll_policy(
        mut self,
        poll_delay: Duration,
        initial_polls: usize,
        resume_polls: usize,
    ) -> Self {
        self.async_poll_delay = poll_delay;
        self.initial_async_polls = initial_polls;
        self.resume_async_polls = resume_polls;
        self
    }

    /// Dynamically update the global HTTP/SOCKS5 outbound proxy URL at runtime.
    pub fn set_outbound_proxy(&self, proxy_url: Option<String>) -> AppResult<()> {
        let next = match proxy_url
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            Some(url) => {
                let client = build_reqwest_client(self.request_timeout, Some(&url), false)?;
                Some((url, client))
            }
            None => None,
        };
        let mut guard = self
            .outbound_proxy
            .write()
            .map_err(|_| AppError::Internal(anyhow::anyhow!("outbound_proxy lock poisoned")))?;
        *guard = next;
        Ok(())
    }

    fn client_for(&self, use_proxy: bool) -> reqwest::Client {
        if !use_proxy {
            return self.direct_client.clone();
        }
        if let Ok(guard) = self.outbound_proxy.read()
            && let Some((_, ref client)) = *guard
        {
            return client.clone();
        }
        self.env_proxy_client.clone()
    }

    fn endpoint(&self, path: &str) -> String {
        let suffix = path.trim_start_matches('/');
        format!("{}/{}", self.api_base_url, suffix)
    }

    async fn execute_json(
        &self,
        method: Method,
        path: &str,
        secret: &str,
        use_proxy: bool,
        body: Option<&Value>,
    ) -> Result<NotionRawResponse, reqwest::Error> {
        let headers = build_notion_headers(secret);
        let client = self.client_for(use_proxy);
        let mut req = client.request(method, self.endpoint(path)).headers(headers);
        if let Some(payload) = body {
            req = req.json(payload);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let retry_after_secs = parse_retry_after_header(resp.headers());
        let raw_text = resp.text().await.unwrap_or_default();
        let json_body: Value = serde_json::from_str(&raw_text).unwrap_or(Value::Null);

        Ok(NotionRawResponse {
            status,
            retry_after_secs,
            body: json_body,
            raw_text,
        })
    }

    // -----------------------------------------------------------------------
    // Mode 1: Zero-Friction Auto-Create 7-Column Database (`POST /v1/databases`)
    // -----------------------------------------------------------------------

    /// Provision a new 7-column FeedCourier database under `parent_page_id` using
    /// `Notion-Version: 2026-03-11` (`initial_data_source.properties`) and bind its `data_source_id`.
    pub async fn init_database(
        &self,
        req: &NotionInitDbRequest,
    ) -> AppResult<NotionInitDbResponse> {
        let secret = req.secret.trim();
        if secret.is_empty() {
            return Err(AppError::Validation(
                "Notion integration secret must not be empty".to_string(),
            ));
        }
        let parent_page_id = normalize_notion_id(&req.parent_page_id)?;
        let db_title = req
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or(DEFAULT_NOTION_DB_TITLE);

        let properties = standard_7_column_schema_json(&NotionStandardFieldKey::ALL);
        let payload = json!({
            "parent": {
                "type": "page_id",
                "page_id": parent_page_id,
            },
            "title": [
                {
                    "type": "text",
                    "text": { "content": truncate_chars(db_title, MAX_NOTION_TEXT_FRAGMENT_CHARS) }
                }
            ],
            "initial_data_source": {
                "properties": properties,
            }
        });

        let resp = self
            .execute_json(
                Method::POST,
                "/v1/databases",
                secret,
                req.use_proxy,
                Some(&payload),
            )
            .await
            .map_err(|err| AppError::Upstream(format!("failed to reach Notion API: {err}")))?;

        if !resp.status.is_success() {
            return Err(resp.into_app_error("POST /v1/databases"));
        }

        let database_id = resp
            .body
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                AppError::Upstream("Notion POST /v1/databases response missing 'id'".to_string())
            })?
            .to_string();

        let database_url = resp
            .body
            .get("url")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(ToOwned::to_owned);

        let data_source_id = if let Some(ds_id) = extract_data_source_id_from_db(&resp.body) {
            ds_id
        } else {
            let get_db = self
                .execute_json(
                    Method::GET,
                    &format!("/v1/databases/{database_id}"),
                    secret,
                    req.use_proxy,
                    None,
                )
                .await
                .map_err(|err| {
                    AppError::Upstream(format!(
                        "failed to fetch newly created Notion database '{database_id}': {err}"
                    ))
                })?;

            if !get_db.status.is_success() {
                return Err(get_db.into_app_error("GET /v1/databases/{id}"));
            }
            extract_data_source_id_from_db(&get_db.body).ok_or_else(|| {
                AppError::Upstream(format!(
                    "Notion database '{database_id}' did not expose a primary data_source_id"
                ))
            })?
        };

        Ok(NotionInitDbResponse {
            database_id,
            data_source_id,
            database_url,
            column_mapping: NotionColumnMapping::standard(),
        })
    }

    // -----------------------------------------------------------------------
    // Mode 2: Bind & Inspect/Patch Existing Data Source (`inspect_schema`)
    // -----------------------------------------------------------------------

    /// Inspect an existing Notion `data_source_id` (or database ID/URL, resolving its primary
    /// `data_sources[0].id` automatically) and optionally patch missing standard columns via
    /// `PATCH /v1/data_sources/{ds_id}`.
    pub async fn inspect_schema(
        &self,
        req: &NotionInspectSchemaRequest,
    ) -> AppResult<NotionInspectSchemaResponse> {
        let secret = req.secret.trim();
        if secret.is_empty() {
            return Err(AppError::Validation(
                "Notion integration secret must not be empty".to_string(),
            ));
        }
        let target_id = normalize_notion_id(&req.data_source_or_db_id)?;
        let patch_missing = req.patch_missing.unwrap_or(false);

        let mut ds = self
            .resolve_data_source_metadata(secret, &target_id, req.use_proxy)
            .await?;

        let initial_eval = evaluate_schema_columns(&ds.properties);
        let mut patched_columns = Vec::new();

        if patch_missing {
            let missing_keys: Vec<NotionStandardFieldKey> = initial_eval
                .columns
                .iter()
                .filter(|col| {
                    col.status == NotionColumnCheckStatus::Missing
                        && !ds.properties.contains_key(&col.standard_name)
                })
                .map(|col| col.field)
                .collect();

            if !missing_keys.is_empty() {
                let patch_props = standard_7_column_schema_json(&missing_keys);
                let patch_payload = json!({ "properties": patch_props });

                let patch_resp = self
                    .execute_json(
                        Method::PATCH,
                        &format!("/v1/data_sources/{}", ds.data_source_id),
                        secret,
                        req.use_proxy,
                        Some(&patch_payload),
                    )
                    .await
                    .map_err(|err| {
                        AppError::Upstream(format!(
                            "failed to PATCH Notion data source '{}': {err}",
                            ds.data_source_id
                        ))
                    })?;

                if !patch_resp.status.is_success() {
                    return Err(patch_resp.into_app_error("PATCH /v1/data_sources/{id}"));
                }

                patched_columns = missing_keys
                    .iter()
                    .map(|k| k.standard_name().to_string())
                    .collect();

                let updated_props = extract_property_types(&patch_resp.body);
                if !updated_props.is_empty() {
                    ds.properties = updated_props;
                } else {
                    for key in &missing_keys {
                        ds.properties.insert(
                            key.standard_name().to_string(),
                            key.expected_type().to_string(),
                        );
                    }
                }
            }
        }

        let final_eval = if patched_columns.is_empty() {
            initial_eval
        } else {
            evaluate_schema_columns(&ds.properties)
        };

        Ok(NotionInspectSchemaResponse {
            data_source_id: ds.data_source_id,
            database_id: ds.database_id,
            title: ds.title,
            columns: final_eval.columns,
            available_properties: final_eval.available_properties,
            suggested_mapping: final_eval.suggested_mapping,
            patched_columns,
            ready: final_eval.ready,
        })
    }

    async fn resolve_data_source_metadata(
        &self,
        secret: &str,
        id: &str,
        use_proxy: bool,
    ) -> AppResult<ResolvedDataSource> {
        let ds_resp = self
            .execute_json(
                Method::GET,
                &format!("/v1/data_sources/{id}"),
                secret,
                use_proxy,
                None,
            )
            .await
            .map_err(|err| AppError::Upstream(format!("failed to reach Notion API: {err}")))?;

        if ds_resp.status.is_success() {
            return Ok(parse_data_source_object(&ds_resp.body, id, None, None));
        }

        let is_not_found = ds_resp.status == StatusCode::NOT_FOUND
            || ds_resp.error_code().as_deref() == Some("object_not_found");

        if !is_not_found {
            return Err(ds_resp.into_app_error("GET /v1/data_sources/{id}"));
        }

        // Fallback: caller provided a `database_id` (or Notion database URL); resolve primary data source.
        let db_resp = self
            .execute_json(
                Method::GET,
                &format!("/v1/databases/{id}"),
                secret,
                use_proxy,
                None,
            )
            .await
            .map_err(|err| AppError::Upstream(format!("failed to reach Notion API: {err}")))?;

        if !db_resp.status.is_success() {
            return Err(db_resp.into_app_error("GET /v1/databases/{id}"));
        }

        let database_id = db_resp
            .body
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_string();
        let db_title = extract_plain_title(&db_resp.body);
        let ds_id = extract_data_source_id_from_db(&db_resp.body).ok_or_else(|| {
            AppError::Validation(format!(
                "Notion database '{database_id}' contains no accessible data sources"
            ))
        })?;

        let resolved_ds = self
            .execute_json(
                Method::GET,
                &format!("/v1/data_sources/{ds_id}"),
                secret,
                use_proxy,
                None,
            )
            .await
            .map_err(|err| AppError::Upstream(format!("failed to reach Notion API: {err}")))?;

        if !resolved_ds.status.is_success() {
            return Err(resolved_ds.into_app_error("GET /v1/data_sources/{id}"));
        }

        Ok(parse_data_source_object(
            &resolved_ds.body,
            &ds_id,
            Some(database_id),
            db_title,
        ))
    }

    // -----------------------------------------------------------------------
    // Crash-Safe Helpers: Async Task Polling & `FC-ID` Dedup Query
    // -----------------------------------------------------------------------

    async fn poll_async_task_bounded(&self, params: AsyncPollParams<'_>) -> AsyncPollResolution {
        let AsyncPollParams {
            secret,
            data_source_id,
            fc_id_col,
            fingerprint,
            task_id,
            use_proxy,
            max_polls,
            wait_before_first,
        } = params;
        let attempts = max_polls.max(1);
        for idx in 0..attempts {
            if (idx > 0 || wait_before_first) && !self.async_poll_delay.is_zero() {
                tokio::time::sleep(self.async_poll_delay).await;
            }

            let resp = match self
                .execute_json(
                    Method::GET,
                    &format!("/v1/async_tasks/{task_id}"),
                    secret,
                    use_proxy,
                    None,
                )
                .await
            {
                Ok(r) => r,
                Err(err) => {
                    return AsyncPollResolution::TransientError {
                        status_code: None,
                        retry_after_secs: None,
                        cooldown: false,
                        diagnostic: format!(
                            "network error polling Notion async_task '{task_id}': {err}"
                        ),
                    };
                }
            };

            if resp.status == StatusCode::TOO_MANY_REQUESTS {
                return AsyncPollResolution::TransientError {
                    status_code: Some(429),
                    retry_after_secs: Some(resp.retry_after_secs.unwrap_or(5)),
                    cooldown: true,
                    diagnostic: resp.diagnostic_summary("GET /v1/async_tasks/{id}"),
                };
            }

            if resp.status == StatusCode::NOT_FOUND {
                return AsyncPollResolution::Failed(format!(
                    "Notion async_task '{task_id}' expired or not found"
                ));
            }

            if resp.status.is_server_error() || resp.status == StatusCode::REQUEST_TIMEOUT {
                return AsyncPollResolution::TransientError {
                    status_code: Some(i32::from(resp.status.as_u16())),
                    retry_after_secs: resp.retry_after_secs,
                    cooldown: false,
                    diagnostic: resp.diagnostic_summary("GET /v1/async_tasks/{id}"),
                };
            }

            if !resp.status.is_success() {
                return AsyncPollResolution::Failed(
                    resp.diagnostic_summary("GET /v1/async_tasks/{id}"),
                );
            }

            let state_raw = resp
                .body
                .get("status")
                .or_else(|| resp.body.get("state"))
                .and_then(Value::as_str)
                .unwrap_or("in_progress")
                .to_ascii_lowercase();

            match state_raw.as_str() {
                "succeeded" | "completed" | "complete" | "success" | "done" => {
                    if let Some(pid) = extract_page_id_from_async_task(&resp.body) {
                        return AsyncPollResolution::Succeeded(pid);
                    }
                    // Fallback: resolve created page_id via `FC-ID` lookup on the data source.
                    match self
                        .query_existing_page_by_fc_id(
                            secret,
                            data_source_id,
                            fc_id_col,
                            fingerprint,
                            use_proxy,
                        )
                        .await
                    {
                        FcIdLookupOutcome::Found(pid) => {
                            return AsyncPollResolution::Succeeded(pid);
                        }
                        FcIdLookupOutcome::NotFound => {
                            return AsyncPollResolution::Failed(format!(
                                "Notion async_task '{task_id}' reported '{state_raw}' without a page_id"
                            ));
                        }
                        FcIdLookupOutcome::Retry(outcome) => {
                            return AsyncPollResolution::Outcome(outcome);
                        }
                        FcIdLookupOutcome::PermanentFailure(outcome) => {
                            return AsyncPollResolution::Outcome(outcome);
                        }
                    }
                }
                "failed" | "error" | "canceled" | "cancelled" | "expired" => {
                    let detail = resp
                        .body
                        .get("error")
                        .and_then(|e| e.get("message").or(Some(e)))
                        .and_then(Value::as_str)
                        .unwrap_or("async page creation failed");
                    return AsyncPollResolution::Failed(format!(
                        "Notion async_task '{task_id}' ended in state '{state_raw}': {detail}"
                    ));
                }
                _ => {
                    // Still pending / in_progress; continue bounded loop if attempts remain.
                }
            }
        }

        AsyncPollResolution::StillRunning
    }

    async fn query_existing_page_by_fc_id(
        &self,
        secret: &str,
        data_source_id: &str,
        fc_id_col: &str,
        fingerprint: &str,
        use_proxy: bool,
    ) -> FcIdLookupOutcome {
        let query_body = json!({
            "filter": {
                "property": fc_id_col,
                "rich_text": {
                    "equals": fingerprint,
                }
            },
            "page_size": 1,
        });

        let checkpoint = Some(DeliveryCheckpoint::Notion {
            create_initiated: true,
            async_task_id: None,
            page_id: None,
        });

        let resp = match self
            .execute_json(
                Method::POST,
                &format!("/v1/data_sources/{data_source_id}/query"),
                secret,
                use_proxy,
                Some(&query_body),
            )
            .await
        {
            Ok(r) => r,
            Err(err) => {
                return FcIdLookupOutcome::Retry(DeliveryOutcome::Retry {
                    retry_after_secs: None,
                    status_code: None,
                    checkpoint,
                    diagnostic: format!(
                        "network error querying Notion FC-ID reconciliation: {err}"
                    ),
                    cooldown_credential_group: false,
                });
            }
        };

        if resp.status == StatusCode::TOO_MANY_REQUESTS {
            return FcIdLookupOutcome::Retry(DeliveryOutcome::Retry {
                retry_after_secs: Some(resp.retry_after_secs.unwrap_or(5)),
                status_code: Some(429),
                checkpoint,
                diagnostic: resp.diagnostic_summary("POST /v1/data_sources/{id}/query"),
                cooldown_credential_group: true,
            });
        }

        if resp.status.is_server_error()
            || resp.status == StatusCode::REQUEST_TIMEOUT
            || resp.status == StatusCode::CONFLICT
        {
            return FcIdLookupOutcome::Retry(DeliveryOutcome::Retry {
                retry_after_secs: resp.retry_after_secs,
                status_code: Some(i32::from(resp.status.as_u16())),
                checkpoint,
                diagnostic: resp.diagnostic_summary("POST /v1/data_sources/{id}/query"),
                cooldown_credential_group: false,
            });
        }

        if !resp.status.is_success() {
            return FcIdLookupOutcome::PermanentFailure(DeliveryOutcome::PermanentFailure {
                status_code: Some(i32::from(resp.status.as_u16())),
                checkpoint,
                diagnostic: resp.diagnostic_summary("POST /v1/data_sources/{id}/query"),
            });
        }

        if let Some(results) = resp.body.get("results").and_then(Value::as_array) {
            for item in results {
                let in_trash = item
                    .get("in_trash")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if !in_trash
                    && let Some(id) = item
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                {
                    return FcIdLookupOutcome::Found(id.to_string());
                }
            }
        }

        FcIdLookupOutcome::NotFound
    }
}

#[async_trait]
impl ChannelSender for NotionSender {
    fn kind(&self) -> ChannelKind {
        ChannelKind::Notion
    }

    async fn send(&self, ctx: &DeliveryContext<'_>) -> AppResult<DeliveryOutcome> {
        let ChannelConfig::Notion(cfg) = ctx.channel_config else {
            return Err(AppError::Validation(format!(
                "NotionSender received incompatible '{}' channel configuration",
                ctx.channel_config.kind()
            )));
        };
        cfg.column_mapping.validate()?;

        let secret = cfg.integration_secret.trim();
        let data_source_id = normalize_notion_id(&cfg.data_source_id)?;
        let use_proxy = ctx.channel.use_proxy;

        let effective_checkpoint = match &ctx.checkpoint {
            Some(cp) => Some(cp.clone()),
            None => ctx.task.checkpoint()?,
        };

        let (create_initiated, existing_async_task_id, existing_page_id) =
            match effective_checkpoint {
                Some(DeliveryCheckpoint::Notion {
                    create_initiated,
                    async_task_id,
                    page_id,
                }) => (create_initiated, async_task_id, page_id),
                _ => (false, None, None),
            };

        // 1a. Page already confirmed in checkpoint -> effective-once short-circuit.
        if let Some(page_id) = existing_page_id.filter(|s| !s.trim().is_empty()) {
            return Ok(DeliveryOutcome::Delivered {
                status_code: Some(200),
                checkpoint: Some(DeliveryCheckpoint::Notion {
                    create_initiated: true,
                    async_task_id: None,
                    page_id: Some(page_id.clone()),
                }),
                diagnostic: Some(format!("Notion page {page_id} already confirmed")),
                updated_capabilities: None,
            });
        }

        // 1b. In-flight `async_task_id` -> poll `/v1/async_tasks/{task_id}` first.
        let mut need_fc_id_reconcile = create_initiated;
        if let Some(task_id) = existing_async_task_id.filter(|s| !s.trim().is_empty()) {
            match self
                .poll_async_task_bounded(AsyncPollParams {
                    secret,
                    data_source_id: &data_source_id,
                    fc_id_col: &cfg.column_mapping.fc_id,
                    fingerprint: &ctx.entry.fingerprint_sha256,
                    task_id: &task_id,
                    use_proxy,
                    max_polls: self.resume_async_polls,
                    wait_before_first: false,
                })
                .await
            {
                AsyncPollResolution::Succeeded(page_id) => {
                    let done_cp = DeliveryCheckpoint::Notion {
                        create_initiated: true,
                        async_task_id: None,
                        page_id: Some(page_id.clone()),
                    };
                    if let Some(sink) = ctx.checkpoint_sink {
                        sink.save_checkpoint(&done_cp).await?;
                    }
                    return Ok(DeliveryOutcome::Delivered {
                        status_code: Some(200),
                        checkpoint: Some(done_cp),
                        diagnostic: Some(format!(
                            "Notion async task '{task_id}' completed (page {page_id})"
                        )),
                        updated_capabilities: None,
                    });
                }
                AsyncPollResolution::StillRunning => {
                    return Ok(DeliveryOutcome::Retry {
                        retry_after_secs: Some(5),
                        status_code: Some(202),
                        checkpoint: Some(DeliveryCheckpoint::Notion {
                            create_initiated: true,
                            async_task_id: Some(task_id),
                            page_id: None,
                        }),
                        diagnostic: "Notion async page creation still in progress".to_string(),
                        cooldown_credential_group: false,
                    });
                }
                AsyncPollResolution::TransientError {
                    status_code,
                    retry_after_secs,
                    cooldown,
                    diagnostic,
                } => {
                    return Ok(DeliveryOutcome::Retry {
                        retry_after_secs,
                        status_code,
                        checkpoint: Some(DeliveryCheckpoint::Notion {
                            create_initiated: true,
                            async_task_id: Some(task_id),
                            page_id: None,
                        }),
                        diagnostic,
                        cooldown_credential_group: cooldown,
                    });
                }
                AsyncPollResolution::Outcome(outcome) => return Ok(outcome),
                AsyncPollResolution::Failed(_reason) => {
                    // Clear stale async_task_id and reconcile via FC-ID before recreating.
                    need_fc_id_reconcile = true;
                }
            }
        }

        // 1c. Crash/timeout reconciliation: check `FC-ID` on data source before creating a duplicate.
        if need_fc_id_reconcile {
            match self
                .query_existing_page_by_fc_id(
                    secret,
                    &data_source_id,
                    &cfg.column_mapping.fc_id,
                    &ctx.entry.fingerprint_sha256,
                    use_proxy,
                )
                .await
            {
                FcIdLookupOutcome::Found(existing_id) => {
                    let done_cp = DeliveryCheckpoint::Notion {
                        create_initiated: true,
                        async_task_id: None,
                        page_id: Some(existing_id.clone()),
                    };
                    if let Some(sink) = ctx.checkpoint_sink {
                        sink.save_checkpoint(&done_cp).await?;
                    }
                    return Ok(DeliveryOutcome::Delivered {
                        status_code: Some(200),
                        checkpoint: Some(done_cp),
                        diagnostic: Some(format!(
                            "Reconciled existing Notion page {existing_id} via FC-ID"
                        )),
                        updated_capabilities: None,
                    });
                }
                FcIdLookupOutcome::NotFound => {}
                FcIdLookupOutcome::Retry(outcome) => return Ok(outcome),
                FcIdLookupOutcome::PermanentFailure(outcome) => return Ok(outcome),
            }
        }

        // 2. Flush pre-create checkpoint to SQLite BEFORE issuing `POST /v1/pages`.
        let pre_cp = DeliveryCheckpoint::Notion {
            create_initiated: true,
            async_task_id: None,
            page_id: None,
        };
        if let Some(sink) = ctx.checkpoint_sink {
            sink.save_checkpoint(&pre_cp).await?;
        }

        // 3. Build minimalist `POST /v1/pages` payload (`markdown_version: "v2"`).
        let page_payload = build_create_page_payload(cfg, &data_source_id, ctx.entry, ctx.feed);

        let create_resp = match self
            .execute_json(
                Method::POST,
                "/v1/pages",
                secret,
                use_proxy,
                Some(&page_payload),
            )
            .await
        {
            Ok(r) => r,
            Err(err) => {
                return Ok(DeliveryOutcome::Retry {
                    retry_after_secs: None,
                    status_code: None,
                    checkpoint: Some(pre_cp),
                    diagnostic: format!("Notion POST /v1/pages network error: {err}"),
                    cooldown_credential_group: false,
                });
            }
        };

        let status_code = i32::from(create_resp.status.as_u16());

        // Handle `202 Accepted` async page creation.
        if create_resp.status == StatusCode::ACCEPTED {
            if let Some(task_id) = extract_async_task_id(&create_resp.body) {
                let async_cp = DeliveryCheckpoint::Notion {
                    create_initiated: true,
                    async_task_id: Some(task_id.clone()),
                    page_id: None,
                };
                if let Some(sink) = ctx.checkpoint_sink {
                    sink.save_checkpoint(&async_cp).await?;
                }

                if self.initial_async_polls > 0 {
                    match self
                        .poll_async_task_bounded(AsyncPollParams {
                            secret,
                            data_source_id: &data_source_id,
                            fc_id_col: &cfg.column_mapping.fc_id,
                            fingerprint: &ctx.entry.fingerprint_sha256,
                            task_id: &task_id,
                            use_proxy,
                            max_polls: self.initial_async_polls,
                            wait_before_first: true,
                        })
                        .await
                    {
                        AsyncPollResolution::Succeeded(page_id) => {
                            let done_cp = DeliveryCheckpoint::Notion {
                                create_initiated: true,
                                async_task_id: None,
                                page_id: Some(page_id.clone()),
                            };
                            if let Some(sink) = ctx.checkpoint_sink {
                                sink.save_checkpoint(&done_cp).await?;
                            }
                            return Ok(DeliveryOutcome::Delivered {
                                status_code: Some(200),
                                checkpoint: Some(done_cp),
                                diagnostic: Some(format!(
                                    "Created Notion page {page_id} via async task '{task_id}'"
                                )),
                                updated_capabilities: None,
                            });
                        }
                        AsyncPollResolution::StillRunning => {
                            return Ok(DeliveryOutcome::Retry {
                                retry_after_secs: Some(4),
                                status_code: Some(202),
                                checkpoint: Some(async_cp),
                                diagnostic: format!(
                                    "Notion async page creation '{task_id}' accepted; awaiting completion"
                                ),
                                cooldown_credential_group: false,
                            });
                        }
                        AsyncPollResolution::TransientError {
                            status_code,
                            retry_after_secs,
                            cooldown,
                            diagnostic,
                        } => {
                            return Ok(DeliveryOutcome::Retry {
                                retry_after_secs: retry_after_secs.or(Some(4)),
                                status_code: status_code.or(Some(202)),
                                checkpoint: Some(async_cp),
                                diagnostic,
                                cooldown_credential_group: cooldown,
                            });
                        }
                        AsyncPollResolution::Outcome(outcome) => return Ok(outcome),
                        AsyncPollResolution::Failed(reason) => {
                            return Ok(DeliveryOutcome::Retry {
                                retry_after_secs: Some(4),
                                status_code: Some(202),
                                checkpoint: Some(pre_cp),
                                diagnostic: reason,
                                cooldown_credential_group: false,
                            });
                        }
                    }
                }

                return Ok(DeliveryOutcome::Retry {
                    retry_after_secs: Some(4),
                    status_code: Some(202),
                    checkpoint: Some(async_cp),
                    diagnostic: format!(
                        "Notion async page creation '{task_id}' accepted; awaiting completion"
                    ),
                    cooldown_credential_group: false,
                });
            }

            return Ok(DeliveryOutcome::Retry {
                retry_after_secs: Some(4),
                status_code: Some(202),
                checkpoint: Some(pre_cp),
                diagnostic:
                    "Notion returned 202 Accepted without task handle; will reconcile via FC-ID"
                        .to_string(),
                cooldown_credential_group: false,
            });
        }

        // Handle synchronous `200 OK` / `201 Created`.
        if create_resp.status.is_success() {
            let page_id = create_resp
                .body
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| {
                    AppError::Upstream("Notion POST /v1/pages response missing 'id'".to_string())
                })?
                .to_string();

            let done_cp = DeliveryCheckpoint::Notion {
                create_initiated: true,
                async_task_id: None,
                page_id: Some(page_id.clone()),
            };
            if let Some(sink) = ctx.checkpoint_sink {
                sink.save_checkpoint(&done_cp).await?;
            }

            return Ok(DeliveryOutcome::Delivered {
                status_code: Some(status_code),
                checkpoint: Some(done_cp),
                diagnostic: Some(format!("Created Notion page {page_id}")),
                updated_capabilities: None,
            });
        }

        // Handle `429 Too Many Requests` -> shared credential-group cooldown.
        if create_resp.status == StatusCode::TOO_MANY_REQUESTS {
            let retry_after = create_resp.retry_after_secs.unwrap_or(5);
            return Ok(DeliveryOutcome::Retry {
                retry_after_secs: Some(retry_after),
                status_code: Some(429),
                checkpoint: Some(pre_cp),
                diagnostic: create_resp.diagnostic_summary("POST /v1/pages"),
                cooldown_credential_group: true,
            });
        }

        // Handle transient `5xx` / `408` / `409 conflict_error` (preserving `create_initiated: true`).
        if create_resp.status.is_server_error()
            || create_resp.status == StatusCode::REQUEST_TIMEOUT
            || create_resp.status == StatusCode::CONFLICT
        {
            return Ok(DeliveryOutcome::Retry {
                retry_after_secs: create_resp.retry_after_secs,
                status_code: Some(status_code),
                checkpoint: Some(pre_cp),
                diagnostic: create_resp.diagnostic_summary("POST /v1/pages"),
                cooldown_credential_group: false,
            });
        }

        // Definitive `4xx` (`400`, `401`, `403`, `404`, etc.) -> permanent failure (`dead_letter`).
        Ok(DeliveryOutcome::PermanentFailure {
            status_code: Some(status_code),
            checkpoint: Some(DeliveryCheckpoint::Notion {
                create_initiated: false,
                async_task_id: None,
                page_id: None,
            }),
            diagnostic: create_resp.diagnostic_summary("POST /v1/pages"),
        })
    }

    async fn send_test(&self, config: &ChannelConfig, use_proxy: bool) -> AppResult<String> {
        let ChannelConfig::Notion(cfg) = config else {
            return Err(AppError::Validation(format!(
                "NotionSender::send_test expected 'notion' config, got '{}'",
                config.kind()
            )));
        };
        config.validate()?;

        let secret = cfg.integration_secret.trim();
        let data_source_id = normalize_notion_id(&cfg.data_source_id)?;

        let resp = self
            .execute_json(
                Method::GET,
                &format!("/v1/data_sources/{data_source_id}"),
                secret,
                use_proxy,
                None,
            )
            .await
            .map_err(|err| AppError::Upstream(format!("failed to reach Notion API: {err}")))?;

        if !resp.status.is_success() {
            return Err(resp.into_app_error("GET /v1/data_sources/{id}"));
        }

        let ds = parse_data_source_object(&resp.body, &data_source_id, None, None);
        let verified_cols = verify_configured_mapping(&ds.properties, &cfg.column_mapping)?;
        let ds_label = ds.title.as_deref().unwrap_or("Untitled Data Source");

        Ok(format!(
            "Connected to Notion data source \"{ds_label}\" ({}) via API {NOTION_API_VERSION}; verified {} mapped columns ({}).",
            ds.data_source_id,
            verified_cols.len(),
            verified_cols.join(", ")
        ))
    }
}

// ---------------------------------------------------------------------------
// Internal Notion Payload & Schema Helpers
// ---------------------------------------------------------------------------

fn build_reqwest_client(
    timeout: Duration,
    explicit_proxy: Option<&str>,
    allow_env_proxy: bool,
) -> AppResult<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .user_agent(format!("{APP_NAME}/{VERSION}"));

    if let Some(proxy_url) = explicit_proxy {
        let proxy = reqwest::Proxy::all(proxy_url).map_err(|err| {
            AppError::Validation(format!("invalid outbound proxy URL '{proxy_url}': {err}"))
        })?;
        builder = builder.proxy(proxy);
    } else if !allow_env_proxy {
        builder = builder.no_proxy();
    }

    builder
        .build()
        .map_err(|err| AppError::Config(format!("failed to initialize Notion HTTP client: {err}")))
}

fn build_notion_headers(secret: &str) -> HeaderMap {
    let mut headers = HeaderMap::with_capacity(3);
    if let Ok(auth) = HeaderValue::from_str(&format!("Bearer {}", secret.trim())) {
        headers.insert(AUTHORIZATION, auth);
    }
    headers.insert(
        "Notion-Version",
        HeaderValue::from_static(NOTION_API_VERSION),
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers
}

fn parse_retry_after_header(headers: &HeaderMap) -> Option<u64> {
    let raw = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(secs.max(1));
    }
    if let Ok(secs_f) = raw.parse::<f64>()
        && secs_f.is_finite()
        && secs_f > 0.0
    {
        return Some((secs_f.ceil() as u64).max(1));
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(raw) {
        let delta = (dt.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_seconds();
        return Some(u64::try_from(delta.max(1)).unwrap_or(5));
    }
    None
}

#[derive(Debug)]
struct NotionRawResponse {
    status: StatusCode,
    retry_after_secs: Option<u64>,
    body: Value,
    raw_text: String,
}

impl NotionRawResponse {
    fn error_code(&self) -> Option<String> {
        self.body
            .get("code")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    }

    fn error_message(&self) -> String {
        if let Some(msg) = self
            .body
            .get("message")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            return msg.trim().to_string();
        }
        let trimmed = self.raw_text.trim();
        if !trimmed.is_empty() {
            return truncate_chars(trimmed, 300);
        }
        format!("HTTP {}", self.status.as_u16())
    }

    fn diagnostic_summary(&self, endpoint_label: &str) -> String {
        let msg = self.error_message();
        match self.error_code() {
            Some(code) => format!(
                "Notion {endpoint_label} failed (HTTP {}, {code}): {msg}",
                self.status.as_u16()
            ),
            None => format!(
                "Notion {endpoint_label} failed (HTTP {}): {msg}",
                self.status.as_u16()
            ),
        }
    }

    fn into_app_error(self, endpoint_label: &str) -> AppError {
        let summary = self.diagnostic_summary(endpoint_label);
        match self.status {
            StatusCode::BAD_REQUEST => AppError::Validation(summary),
            StatusCode::UNAUTHORIZED => AppError::Unauthorized(summary),
            StatusCode::FORBIDDEN => AppError::Forbidden(summary),
            StatusCode::NOT_FOUND => AppError::NotFound(summary),
            StatusCode::CONFLICT => AppError::Conflict(summary),
            StatusCode::TOO_MANY_REQUESTS => AppError::RateLimited {
                message: summary,
                retry_after_secs: Some(self.retry_after_secs.unwrap_or(5)),
            },
            _ => AppError::Upstream(summary),
        }
    }
}

#[derive(Debug)]
struct ResolvedDataSource {
    data_source_id: String,
    database_id: Option<String>,
    title: Option<String>,
    properties: BTreeMap<String, String>,
}

fn standard_7_column_schema_json(keys: &[NotionStandardFieldKey]) -> Value {
    let mut props = Map::new();
    for key in keys {
        let def = match key {
            NotionStandardFieldKey::Title => json!({ "title": {} }),
            NotionStandardFieldKey::Link => json!({ "url": {} }),
            NotionStandardFieldKey::Source => json!({ "select": { "options": [] } }),
            NotionStandardFieldKey::Published => json!({ "date": {} }),
            NotionStandardFieldKey::State => json!({
                "select": {
                    "options": [
                        { "name": "UnRead", "color": "blue" },
                        { "name": "Read", "color": "gray" }
                    ]
                }
            }),
            NotionStandardFieldKey::Tag => json!({ "multi_select": { "options": [] } }),
            NotionStandardFieldKey::FcId => json!({ "rich_text": {} }),
        };
        props.insert(key.standard_name().to_string(), def);
    }
    Value::Object(props)
}

fn extract_data_source_id_from_db(db_json: &Value) -> Option<String> {
    if let Some(arr) = db_json.get("data_sources").and_then(Value::as_array) {
        for item in arr {
            if let Some(id) = item
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
            {
                return Some(id.to_string());
            }
        }
    }
    if let Some(id) = db_json
        .get("initial_data_source")
        .and_then(|ds| ds.get("id"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        return Some(id.to_string());
    }
    None
}

fn extract_plain_title(obj: &Value) -> Option<String> {
    let arr = obj.get("title")?.as_array()?;
    let mut out = String::new();
    for fragment in arr {
        if let Some(plain) = fragment.get("plain_text").and_then(Value::as_str) {
            out.push_str(plain);
        } else if let Some(content) = fragment
            .get("text")
            .and_then(|t| t.get("content"))
            .and_then(Value::as_str)
        {
            out.push_str(content);
        }
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn extract_property_types(obj: &Value) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Some(props) = obj.get("properties").and_then(Value::as_object) {
        for (name, val) in props {
            if let Some(prop_type) = val.get("type").and_then(Value::as_str) {
                map.insert(name.clone(), prop_type.to_string());
            } else {
                // Fallback: detect schema key when `type` is omitted in minimal mock payloads.
                for known in [
                    "title",
                    "rich_text",
                    "url",
                    "select",
                    "multi_select",
                    "date",
                    "checkbox",
                    "number",
                    "status",
                ] {
                    if val.get(known).is_some() {
                        map.insert(name.clone(), known.to_string());
                        break;
                    }
                }
            }
        }
    }
    map
}

fn parse_data_source_object(
    body: &Value,
    fallback_ds_id: &str,
    fallback_db_id: Option<String>,
    fallback_title: Option<String>,
) -> ResolvedDataSource {
    let data_source_id = body
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(fallback_ds_id)
        .to_string();

    let database_id = body
        .get("parent")
        .and_then(|p| p.get("database_id"))
        .and_then(Value::as_str)
        .or_else(|| {
            body.get("database_parent")
                .and_then(|p| p.get("database_id"))
                .and_then(Value::as_str)
        })
        .or_else(|| body.get("database_id").and_then(Value::as_str))
        .filter(|s| !s.trim().is_empty())
        .map(ToOwned::to_owned)
        .or(fallback_db_id);

    let title = extract_plain_title(body).or(fallback_title);
    let properties = extract_property_types(body);

    ResolvedDataSource {
        data_source_id,
        database_id,
        title,
        properties,
    }
}

struct SchemaEvaluation {
    columns: Vec<NotionColumnCheckItem>,
    available_properties: Vec<NotionPropertySummary>,
    suggested_mapping: NotionColumnMapping,
    ready: bool,
}

fn evaluate_schema_columns(properties: &BTreeMap<String, String>) -> SchemaEvaluation {
    let mut available_properties: Vec<NotionPropertySummary> = properties
        .iter()
        .map(|(name, property_type)| NotionPropertySummary {
            name: name.clone(),
            property_type: property_type.clone(),
        })
        .collect();
    available_properties.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });

    let mut columns = Vec::with_capacity(NotionStandardFieldKey::ALL.len());
    let mut suggested_mapping = NotionColumnMapping {
        title: String::new(),
        fc_id: String::new(),
        link: None,
        source: None,
        published: None,
        state: None,
        tag: None,
    };

    for field in NotionStandardFieldKey::ALL {
        let standard_name = field.standard_name();
        let expected_type = field.expected_type();
        let required = field.is_required();

        let item = if field == NotionStandardFieldKey::Title {
            // Every Notion data source has a single `title` column (often named `"Name"`).
            // Prefer `"Title"` if it has `type == "title"`, otherwise automatically bind whichever
            // existing property has `type == "title"`.
            if let Some(t) = properties.get(standard_name)
                && t == expected_type
            {
                NotionColumnCheckItem {
                    field,
                    standard_name: standard_name.to_string(),
                    expected_type: expected_type.to_string(),
                    actual_type: Some(expected_type.to_string()),
                    required,
                    status: NotionColumnCheckStatus::Matched,
                    mapped_property: Some(standard_name.to_string()),
                }
            } else if let Some((existing_title_name, _)) =
                properties.iter().find(|(_, ptype)| *ptype == "title")
            {
                NotionColumnCheckItem {
                    field,
                    standard_name: standard_name.to_string(),
                    expected_type: expected_type.to_string(),
                    actual_type: Some("title".to_string()),
                    required,
                    status: NotionColumnCheckStatus::Matched,
                    mapped_property: Some(existing_title_name.clone()),
                }
            } else if let Some(wrong_type) = properties.get(standard_name) {
                NotionColumnCheckItem {
                    field,
                    standard_name: standard_name.to_string(),
                    expected_type: expected_type.to_string(),
                    actual_type: Some(wrong_type.clone()),
                    required,
                    status: NotionColumnCheckStatus::Mismatched,
                    mapped_property: None,
                }
            } else {
                NotionColumnCheckItem {
                    field,
                    standard_name: standard_name.to_string(),
                    expected_type: expected_type.to_string(),
                    actual_type: None,
                    required,
                    status: NotionColumnCheckStatus::Missing,
                    mapped_property: None,
                }
            }
        } else if let Some(actual) = properties.get(standard_name) {
            if actual == expected_type {
                NotionColumnCheckItem {
                    field,
                    standard_name: standard_name.to_string(),
                    expected_type: expected_type.to_string(),
                    actual_type: Some(actual.clone()),
                    required,
                    status: NotionColumnCheckStatus::Matched,
                    mapped_property: Some(standard_name.to_string()),
                }
            } else {
                NotionColumnCheckItem {
                    field,
                    standard_name: standard_name.to_string(),
                    expected_type: expected_type.to_string(),
                    actual_type: Some(actual.clone()),
                    required,
                    status: NotionColumnCheckStatus::Mismatched,
                    mapped_property: None,
                }
            }
        } else if let Some((ci_name, ci_type)) = properties
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(standard_name))
        {
            if ci_type == expected_type {
                NotionColumnCheckItem {
                    field,
                    standard_name: standard_name.to_string(),
                    expected_type: expected_type.to_string(),
                    actual_type: Some(ci_type.clone()),
                    required,
                    status: NotionColumnCheckStatus::Matched,
                    mapped_property: Some(ci_name.clone()),
                }
            } else {
                NotionColumnCheckItem {
                    field,
                    standard_name: standard_name.to_string(),
                    expected_type: expected_type.to_string(),
                    actual_type: Some(ci_type.clone()),
                    required,
                    status: NotionColumnCheckStatus::Mismatched,
                    mapped_property: None,
                }
            }
        } else {
            NotionColumnCheckItem {
                field,
                standard_name: standard_name.to_string(),
                expected_type: expected_type.to_string(),
                actual_type: None,
                required,
                status: NotionColumnCheckStatus::Missing,
                mapped_property: None,
            }
        };

        if let Some(ref mapped) = item.mapped_property {
            match field {
                NotionStandardFieldKey::Title => suggested_mapping.title = mapped.clone(),
                NotionStandardFieldKey::FcId => suggested_mapping.fc_id = mapped.clone(),
                NotionStandardFieldKey::Link => suggested_mapping.link = Some(mapped.clone()),
                NotionStandardFieldKey::Source => suggested_mapping.source = Some(mapped.clone()),
                NotionStandardFieldKey::Published => {
                    suggested_mapping.published = Some(mapped.clone());
                }
                NotionStandardFieldKey::State => suggested_mapping.state = Some(mapped.clone()),
                NotionStandardFieldKey::Tag => suggested_mapping.tag = Some(mapped.clone()),
            }
        }

        columns.push(item);
    }

    let ready =
        !suggested_mapping.title.trim().is_empty() && !suggested_mapping.fc_id.trim().is_empty();

    SchemaEvaluation {
        columns,
        available_properties,
        suggested_mapping,
        ready,
    }
}

fn verify_configured_mapping(
    properties: &BTreeMap<String, String>,
    mapping: &NotionColumnMapping,
) -> AppResult<Vec<String>> {
    let mut verified = Vec::new();
    let checks: [(Option<&str>, &str, bool); 7] = [
        (Some(mapping.title.as_str()), "title", true),
        (Some(mapping.fc_id.as_str()), "rich_text", true),
        (mapping.link.as_deref(), "url", false),
        (mapping.source.as_deref(), "select", false),
        (mapping.published.as_deref(), "date", false),
        (mapping.state.as_deref(), "select", false),
        (mapping.tag.as_deref(), "multi_select", false),
    ];

    for (prop_opt, expected_type, required) in checks {
        let Some(prop_name) = prop_opt.map(str::trim).filter(|s| !s.is_empty()) else {
            if required {
                return Err(AppError::Validation(format!(
                    "required Notion '{expected_type}' column mapping is empty"
                )));
            }
            continue;
        };

        match properties.get(prop_name) {
            Some(actual_type) if actual_type == expected_type => {
                verified.push(prop_name.to_string());
            }
            Some(actual_type) => {
                return Err(AppError::Validation(format!(
                    "Notion property '{prop_name}' has type '{actual_type}', expected '{expected_type}'"
                )));
            }
            None => {
                return Err(AppError::Validation(format!(
                    "Notion property '{prop_name}' ({expected_type}) was not found on target data source"
                )));
            }
        }
    }

    Ok(verified)
}

fn is_valid_http_url(raw: &str) -> bool {
    let Ok(parsed) = Url::parse(raw.trim()) else {
        return false;
    };
    matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some()
}

/// Sanitize a Notion `select` / `multi_select` option name (stripping commas and control chars).
fn sanitize_select_option_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .map(|c| if c == ',' || c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        None
    } else {
        Some(truncate_chars(&collapsed, MAX_NOTION_SELECT_CHARS))
    }
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).collect()
}

fn build_create_page_payload(
    cfg: &NotionChannelConfig,
    data_source_id: &str,
    entry: &Entry,
    feed: &Feed,
) -> Value {
    let mapping = &cfg.column_mapping;
    let normalized = entry_to_notion_markdown_v2(entry);

    let title_text = entry
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("(Untitled)");

    let mut properties = Map::new();
    properties.insert(
        mapping.title.trim().to_string(),
        json!({
            "title": [
                {
                    "type": "text",
                    "text": { "content": truncate_chars(title_text, MAX_NOTION_TEXT_FRAGMENT_CHARS) }
                }
            ]
        }),
    );

    properties.insert(
        mapping.fc_id.trim().to_string(),
        json!({
            "rich_text": [
                {
                    "type": "text",
                    "text": { "content": truncate_chars(&entry.fingerprint_sha256, MAX_NOTION_TEXT_FRAGMENT_CHARS) }
                }
            ]
        }),
    );

    if let Some(link_col) = mapping
        .link
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        && let Some(url) = entry
            .canonical_url
            .as_deref()
            .map(str::trim)
            .filter(|u| is_valid_http_url(u))
    {
        properties.insert(link_col.to_string(), json!({ "url": url }));
    }

    if let Some(source_col) = mapping
        .source
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let raw_source = if !feed.title.trim().is_empty() {
            feed.title.as_str()
        } else {
            feed.site_url.as_deref().unwrap_or(&feed.url)
        };
        if let Some(source_name) = sanitize_select_option_name(raw_source) {
            properties.insert(
                source_col.to_string(),
                json!({ "select": { "name": source_name } }),
            );
        }
    }

    if let Some(pub_col) = mapping
        .published
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        && let Some(ts) = entry.published_at
    {
        properties.insert(
            pub_col.to_string(),
            json!({ "date": { "start": unix_to_rfc3339(ts) } }),
        );
    }

    if let Some(state_col) = mapping
        .state
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        properties.insert(
            state_col.to_string(),
            json!({ "select": { "name": "UnRead" } }),
        );
    }

    if let Some(tag_col) = mapping
        .tag
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let mut unique_tags: Vec<String> = Vec::new();
        for raw_tag in feed.tags() {
            if let Some(clean_tag) = sanitize_select_option_name(&raw_tag)
                && !unique_tags.contains(&clean_tag)
            {
                unique_tags.push(clean_tag);
            }
        }
        let tag_objs: Vec<Value> = unique_tags
            .into_iter()
            .map(|name| json!({ "name": name }))
            .collect();
        properties.insert(tag_col.to_string(), json!({ "multi_select": tag_objs }));
    }

    let mut payload = Map::new();
    payload.insert(
        "parent".to_string(),
        json!({
            "type": "data_source_id",
            "data_source_id": data_source_id,
        }),
    );

    if let Some(favicon) = feed
        .favicon_url
        .as_deref()
        .map(str::trim)
        .filter(|u| is_valid_http_url(u))
    {
        payload.insert(
            "icon".to_string(),
            json!({
                "type": "external",
                "external": { "url": favicon }
            }),
        );
    }

    payload.insert("properties".to_string(), Value::Object(properties));
    payload.insert(
        "markdown".to_string(),
        Value::String(normalized.markdown.clone()),
    );
    payload.insert(
        "markdown_version".to_string(),
        Value::String("v2".to_string()),
    );

    if normalized.should_allow_async() {
        payload.insert("allow_async".to_string(), Value::Bool(true));
    }

    Value::Object(payload)
}

fn extract_async_task_id(body: &Value) -> Option<String> {
    for key in ["async_task_id", "task_id", "id"] {
        if let Some(id) = body
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            return Some(id.to_string());
        }
    }
    None
}

fn extract_page_id_from_async_task(body: &Value) -> Option<String> {
    let candidates = [
        body.get("page_id"),
        body.get("result").and_then(|r| r.get("page_id")),
        body.get("result").and_then(|r| r.get("id")),
        body.get("page").and_then(|p| p.get("id")),
        body.get("data").and_then(|d| d.get("page_id")),
    ];
    for candidate in candidates.into_iter().flatten() {
        if let Some(id) = candidate.as_str().filter(|s| !s.trim().is_empty()) {
            return Some(id.to_string());
        }
    }
    None
}

struct AsyncPollParams<'a> {
    secret: &'a str,
    data_source_id: &'a str,
    fc_id_col: &'a str,
    fingerprint: &'a str,
    task_id: &'a str,
    use_proxy: bool,
    max_polls: usize,
    wait_before_first: bool,
}

enum AsyncPollResolution {
    Succeeded(String),
    StillRunning,
    Failed(String),
    TransientError {
        status_code: Option<i32>,
        retry_after_secs: Option<u64>,
        cooldown: bool,
        diagnostic: String,
    },
    Outcome(DeliveryOutcome),
}

enum FcIdLookupOutcome {
    Found(String),
    NotFound,
    Retry(DeliveryOutcome),
    PermanentFailure(DeliveryOutcome),
}
