//! Content filter engine (case-insensitive keywords + bounded `regex::Regex`) and route-to-feed
//! matching helpers (`feedcourier.md` Sections 三.2, 六.1).

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::db::models::{Feed, Route};
use crate::error::{AppError, AppResult};

/// Maximum compiled NFA/DFA size (`1 MiB`) enforced on user-supplied regular expressions.
pub const REGEX_SIZE_LIMIT_BYTES: usize = 1 << 20;

/// Maximum source pattern length accepted for a route/preview regex.
pub const MAX_REGEX_PATTERN_CHARS: usize = 4096;

/// Reusable content filter rule set shared by `POST /api/v1/feeds/preview`, MCP `preview_feed_latest`,
/// and persisted `routes`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentFilterRules {
    #[serde(default)]
    pub include_keywords: Vec<String>,
    #[serde(default)]
    pub exclude_keywords: Vec<String>,
    #[serde(default)]
    pub include_regex: Option<String>,
    #[serde(default)]
    pub exclude_regex: Option<String>,
}

impl ContentFilterRules {
    /// Extract [`ContentFilterRules`] from a persisted [`Route`] row.
    pub fn from_route(route: &Route) -> Self {
        Self {
            include_keywords: route.include_keywords(),
            exclude_keywords: route.exclude_keywords(),
            include_regex: route.include_regex.clone(),
            exclude_regex: route.exclude_regex.clone(),
        }
    }

    /// Validate and compile this rule set into a reusable [`CompiledContentFilter`].
    pub fn compile(&self) -> AppResult<CompiledContentFilter> {
        CompiledContentFilter::new(self)
    }

    /// Convenience one-shot evaluation returning `(matched, reason)`.
    pub fn evaluate(
        &self,
        title: Option<&str>,
        body_text: Option<&str>,
    ) -> AppResult<(bool, Option<String>)> {
        let compiled = self.compile()?;
        Ok(compiled.evaluate(title, body_text))
    }
}

/// Pre-compiled, ReDoS-bounded content filter ready for fast multi-entry evaluation.
#[derive(Debug, Clone)]
pub struct CompiledContentFilter {
    include_keywords_lower: Vec<(String, String)>,
    exclude_keywords_lower: Vec<(String, String)>,
    include_regex: Option<(String, Regex)>,
    exclude_regex: Option<(String, Regex)>,
}

impl CompiledContentFilter {
    /// Compile keyword lists and regular expressions with strict NFA/DFA memory bounds.
    pub fn new(rules: &ContentFilterRules) -> AppResult<Self> {
        let include_keywords_lower = normalize_keywords(&rules.include_keywords);
        let exclude_keywords_lower = normalize_keywords(&rules.exclude_keywords);

        let include_regex =
            compile_optional_regex(rules.include_regex.as_deref(), "include_regex")?;
        let exclude_regex =
            compile_optional_regex(rules.exclude_regex.as_deref(), "exclude_regex")?;

        Ok(Self {
            include_keywords_lower,
            exclude_keywords_lower,
            include_regex,
            exclude_regex,
        })
    }

    /// Evaluate `(title, body_text)` against the compiled filter, returning `(matched, reason)`.
    ///
    /// When `matched == true`, `reason` is `None`. When `matched == false`, `reason` explains
    /// which rule rejected or failed to match the entry.
    pub fn evaluate(&self, title: Option<&str>, body_text: Option<&str>) -> (bool, Option<String>) {
        let haystack = build_filter_haystack(title, body_text);
        let haystack_lower = haystack.to_lowercase();

        // 1. Exclude keywords (case-insensitive substring match).
        for (raw_kw, lower_kw) in &self.exclude_keywords_lower {
            if haystack_lower.contains(lower_kw.as_str()) {
                return (false, Some(format!("excluded by keyword '{raw_kw}'")));
            }
        }

        // 2. Exclude regex.
        if let Some((pat, re)) = &self.exclude_regex
            && re.is_match(&haystack)
        {
            return (false, Some(format!("excluded by regex '{pat}'")));
        }

        // 3. Include keywords (at least one must match case-insensitively when non-empty).
        if !self.include_keywords_lower.is_empty() {
            let any_hit = self
                .include_keywords_lower
                .iter()
                .any(|(_, lower_kw)| haystack_lower.contains(lower_kw.as_str()));
            if !any_hit {
                return (
                    false,
                    Some("did not match any include_keywords".to_string()),
                );
            }
        }

        // 4. Include regex (must match when specified).
        if let Some((pat, re)) = &self.include_regex
            && !re.is_match(&haystack)
        {
            return (false, Some(format!("did not match include_regex '{pat}'")));
        }

        (true, None)
    }
}

/// Check whether `route` is enabled and targets `feed` by `feed_ids` or `match_tags`.
///
/// - If `route.enabled == false`, always returns `false`.
/// - If **both** `feed_ids` and `match_tags` on the route are empty, matches **all** feeds.
/// - Otherwise matches if `feed.id` is in `feed_ids` **or** any tag on `feed` intersects
///   `route.match_tags()` (case-insensitively).
pub fn route_matches_feed(route: &Route, feed: &Feed) -> bool {
    if !route.enabled {
        return false;
    }

    let route_feed_ids = route.feed_ids();
    let route_tags: Vec<String> = route
        .match_tags()
        .into_iter()
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();

    if route_feed_ids.is_empty() && route_tags.is_empty() {
        return true;
    }

    if route_feed_ids.contains(&feed.id) {
        return true;
    }

    if !route_tags.is_empty() {
        let feed_tags = feed.tags();
        let intersects = feed_tags.iter().any(|ft| {
            let norm = ft.trim().to_lowercase();
            !norm.is_empty() && route_tags.iter().any(|rt| rt == &norm)
        });
        if intersects {
            return true;
        }
    }

    false
}

/// Evaluate whether `route` matches both `feed` (enabled + `feed_ids`/`match_tags`) and the
/// entry's `(title, body_text)` content filter rules.
pub fn route_matches_entry(
    route: &Route,
    feed: &Feed,
    title: Option<&str>,
    body_text: Option<&str>,
) -> AppResult<(bool, Option<String>)> {
    if !route.enabled {
        return Ok((false, Some("route is disabled".to_string())));
    }
    if !route_matches_feed(route, feed) {
        return Ok((
            false,
            Some("feed does not match route feed_ids or match_tags".to_string()),
        ));
    }
    ContentFilterRules::from_route(route).evaluate(title, body_text)
}

fn normalize_keywords(raw: &[String]) -> Vec<(String, String)> {
    raw.iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| (s.to_string(), s.to_lowercase()))
        .collect()
}

/// Validate and compile a single optional regular expression with bounded NFA/DFA size.
pub fn compile_optional_regex(
    pattern: Option<&str>,
    field_name: &str,
) -> AppResult<Option<(String, Regex)>> {
    let Some(trimmed) = pattern.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };

    if trimmed.chars().count() > MAX_REGEX_PATTERN_CHARS {
        return Err(AppError::Validation(format!(
            "{field_name} exceeds maximum length of {MAX_REGEX_PATTERN_CHARS} characters"
        )));
    }

    let regex = RegexBuilder::new(trimmed)
        .size_limit(REGEX_SIZE_LIMIT_BYTES)
        .dfa_size_limit(REGEX_SIZE_LIMIT_BYTES)
        .build()
        .map_err(|err| AppError::Validation(format!("invalid {field_name} '{trimmed}': {err}")))?;

    Ok(Some((trimmed.to_string(), regex)))
}

fn build_filter_haystack(title: Option<&str>, body_text: Option<&str>) -> String {
    let t = title.map(str::trim).unwrap_or("");
    let b = body_text.map(str::trim).unwrap_or("");
    match (t.is_empty(), b.is_empty()) {
        (true, true) => String::new(),
        (false, true) => t.to_string(),
        (true, false) => b.to_string(),
        (false, false) => format!("{t}\n{b}"),
    }
}
