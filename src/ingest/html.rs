//! HTML relative URL resolution, `ammonia` sanitization, media attachment extraction,
//! plain-text summary generation, and Unicode first-sentence segmentation
//! (`feedcourier.md` Sections 二.1, 四.1, 五.2).

use std::borrow::Cow;
use std::collections::HashSet;

use ammonia::{Builder, UrlRelative};
use unicode_segmentation::UnicodeSegmentation;
use url::Url;

use crate::db::models::{MediaAttachment, MediaAttachmentKind};

/// Maximum characters stored in `entries.summary_text`.
pub const MAX_SUMMARY_CHARS: usize = 600;

/// Maximum characters stored in `entries.first_sentence`.
pub const MAX_FIRST_SENTENCE_CHARS: usize = 280;

/// Sanitized article HTML output along with extracted plain-text summary, first sentence, and inline media.
#[derive(Debug, Clone, Default)]
pub struct SanitizedHtmlOutput {
    pub content_html: String,
    pub plain_text: String,
    pub summary_text: Option<String>,
    pub first_sentence: Option<String>,
    pub inline_media: Vec<MediaAttachment>,
}

/// Resolve all relative media/link URLs against `base_url`, strip 1x1 tracking pixels, sanitize
/// HTML via `ammonia`, and extract plain-text summary, Unicode first sentence, and inline media.
pub fn sanitize_and_extract_html(raw_html: &str, base_url: Option<&Url>) -> SanitizedHtmlOutput {
    if raw_html.trim().is_empty() {
        return SanitizedHtmlOutput::default();
    }

    // 1. Pre-resolve relative URLs on `<a href>`, `<img src/srcset>`, `<video src/poster>`,
    // `<audio src>`, `<source src/srcset>`, and strip obvious 1x1 tracking pixel `<img>` tags
    // BEFORE passing into `ammonia`.
    let pre_resolved = resolve_relative_urls_in_html(raw_html, base_url);

    // 2. Sanitize HTML through configured `ammonia::Builder`.
    let content_html = run_ammonia(&pre_resolved, base_url);

    // 3. Extract inline media attachments from the sanitized HTML.
    let inline_media = extract_inline_media_from_html(&content_html);

    // 4. Extract clean plain text, summary, and Unicode first sentence.
    let plain_text = html_to_plain_text(&content_html);
    let summary_text = extract_summary_text(&plain_text);
    let first_sentence = extract_first_sentence(&plain_text);

    SanitizedHtmlOutput {
        content_html,
        plain_text,
        summary_text,
        first_sentence,
        inline_media,
    }
}

fn run_ammonia(html: &str, base_url: Option<&Url>) -> String {
    let mut builder = Builder::default();

    builder.add_tags(&[
        "video", "audio", "source", "picture", "tfoot", "section", "main",
    ]);

    // Strip both the tag and all inner text for executable/embedded/interactive elements.
    // (`script` and `style` are already in `Builder::default()`'s `clean_content_tags`.)
    builder.add_clean_content_tags(&[
        "iframe", "noscript", "object", "embed", "applet", "frameset", "frame", "svg", "math",
        "form", "textarea", "button", "select",
    ]);

    // Semantic media & code attributes (`title` is already allowed in generic_attributes).
    builder.add_tag_attributes("img", &["src", "srcset", "alt", "width", "height"]);
    builder.add_tag_attributes(
        "video",
        &["src", "poster", "controls", "width", "height", "preload"],
    );
    builder.add_tag_attributes("audio", &["src", "controls", "preload"]);
    builder.add_tag_attributes("source", &["src", "srcset", "type", "media"]);
    builder.add_tag_attributes("code", &["class"]);
    builder.add_tag_attributes("pre", &["class"]);

    builder.url_schemes(HashSet::from(["http", "https", "mailto"]));

    let base_for_filter = base_url.cloned();
    builder.attribute_filter(move |element, attribute, value| {
        if element == "video" && attribute == "poster" {
            let resolved = resolve_url_value(value, base_for_filter.as_ref())?;
            let parsed = Url::parse(&resolved).ok()?;
            if matches!(parsed.scheme(), "http" | "https") {
                return Some(Cow::Owned(resolved));
            }
            return None;
        }
        Some(Cow::Borrowed(value))
    });

    if let Some(base) = base_url {
        builder.url_relative(UrlRelative::RewriteWithBase(base.clone()));
    } else {
        builder.url_relative(UrlRelative::PassThrough);
    }

    builder.clean(html).to_string()
}

/// Resolve relative `<a href>`, `<img src/srcset>`, `<video src/poster>`, `<audio src>`, and
/// `<source src/srcset>` attributes against `base_url`, promote lazy-loaded `data-src` on `<img>`,
/// and drop obvious 1x1 / hidden tracking pixels prior to running `ammonia`.
pub fn resolve_relative_urls_in_html(raw_html: &str, base_url: Option<&Url>) -> String {
    let mut out = String::with_capacity(raw_html.len());
    let bytes = raw_html.as_bytes();
    let mut idx = 0;

    while idx < bytes.len() {
        let Some(rel_lt) = raw_html[idx..].find('<') else {
            out.push_str(&raw_html[idx..]);
            break;
        };
        let lt = idx + rel_lt;
        out.push_str(&raw_html[idx..lt]);

        // Skip HTML comments `<!-- ... -->` verbatim (ammonia strips comments).
        if raw_html[lt..].starts_with("<!--") {
            if let Some(end_comment) = raw_html[lt + 4..].find("-->") {
                let end = lt + 4 + end_comment + 3;
                out.push_str(&raw_html[lt..end]);
                idx = end;
                continue;
            } else {
                out.push_str(&raw_html[lt..]);
                break;
            }
        }

        let Some(tag_end) = find_tag_end(&raw_html[lt..]) else {
            out.push_str(&raw_html[lt..]);
            break;
        };
        let full_tag = &raw_html[lt..=lt + tag_end];
        idx = lt + tag_end + 1;

        if let Some(rewritten) = rewrite_start_tag(full_tag, base_url) {
            out.push_str(&rewritten);
        } else {
            // `None` indicates the tag was dropped (e.g. 1x1 tracking pixel `<img>`).
        }
    }

    out
}

/// Locate the closing `>` of an HTML tag while respecting single- and double-quoted attribute values.
fn find_tag_end(slice_from_lt: &str) -> Option<usize> {
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

#[derive(Debug, Clone)]
struct ParsedAttr {
    name: String,
    value: Option<String>,
}

/// Parse an opening tag `<tag attr="val" ...>`; returns `Some(rewritten)` or `None` to drop the tag.
fn rewrite_start_tag(tag_str: &str, base_url: Option<&Url>) -> Option<String> {
    let inner = tag_str.strip_prefix('<')?.strip_suffix('>')?;
    let trimmed = inner.trim_start();
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.starts_with('!')
        || trimmed.starts_with('?')
    {
        return Some(tag_str.to_string());
    }

    let self_closing = inner.trim_end().ends_with('/');
    let body_without_slash = if self_closing {
        inner.trim_end().strip_suffix('/').unwrap_or(inner)
    } else {
        inner
    };

    let name_end = body_without_slash
        .find(|c: char| c.is_ascii_whitespace() || c == '/')
        .unwrap_or(body_without_slash.len());
    let tag_name = &body_without_slash[..name_end];
    let tag_lower = tag_name.to_ascii_lowercase();

    if !matches!(
        tag_lower.as_str(),
        "a" | "img" | "video" | "audio" | "source"
    ) {
        return Some(tag_str.to_string());
    }

    let attr_str = &body_without_slash[name_end..];
    let mut attrs = parse_html_attrs(attr_str);

    if tag_lower == "img" {
        if is_tracking_pixel_img(&attrs) {
            return None;
        }
        // Promote lazy-load `data-src` if `src` is missing or a `data:` placeholder.
        let has_usable_src = attrs.iter().any(|a| {
            a.name.eq_ignore_ascii_case("src")
                && a.value
                    .as_deref()
                    .map(str::trim)
                    .is_some_and(|v| !v.is_empty() && !v.to_ascii_lowercase().starts_with("data:"))
        });
        if !has_usable_src
            && let Some(data_src) = attrs
                .iter()
                .find(|a| a.name.eq_ignore_ascii_case("data-src"))
                .and_then(|a| a.value.clone())
                .filter(|v| !v.trim().is_empty())
        {
            if let Some(existing) = attrs
                .iter_mut()
                .find(|a| a.name.eq_ignore_ascii_case("src"))
            {
                existing.value = Some(data_src);
            } else {
                attrs.insert(
                    0,
                    ParsedAttr {
                        name: "src".to_string(),
                        value: Some(data_src),
                    },
                );
            }
        }
    }

    if let Some(base) = base_url {
        for attr in &mut attrs {
            let attr_lower = attr.name.to_ascii_lowercase();
            let is_single_url = matches!(
                (tag_lower.as_str(), attr_lower.as_str()),
                ("a", "href")
                    | ("img", "src")
                    | ("video", "src")
                    | ("video", "poster")
                    | ("audio", "src")
                    | ("source", "src")
            );
            let is_srcset = matches!(
                (tag_lower.as_str(), attr_lower.as_str()),
                ("img", "srcset") | ("source", "srcset")
            );

            if is_single_url {
                if let Some(val) = attr.value.as_deref()
                    && let Some(resolved) = resolve_url_value(val, Some(base))
                {
                    attr.value = Some(resolved);
                }
            } else if is_srcset && let Some(val) = attr.value.as_deref() {
                attr.value = Some(resolve_srcset_value(val, base));
            }
        }
    }

    let mut rebuilt = String::with_capacity(tag_str.len() + 32);
    rebuilt.push('<');
    rebuilt.push_str(&tag_lower);
    for attr in &attrs {
        rebuilt.push(' ');
        rebuilt.push_str(&attr.name);
        if let Some(val) = &attr.value {
            rebuilt.push_str("=\"");
            push_escaped_attr(&mut rebuilt, val);
            rebuilt.push('"');
        }
    }
    if self_closing {
        rebuilt.push_str(" />");
    } else {
        rebuilt.push('>');
    }
    Some(rebuilt)
}

fn is_tracking_pixel_img(attrs: &[ParsedAttr]) -> bool {
    let get_attr = |name: &str| -> Option<&str> {
        attrs
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(name))
            .and_then(|a| a.value.as_deref())
            .map(str::trim)
    };

    let is_zero_or_one = |val: Option<&str>| -> bool {
        matches!(
            val.map(|s| s.to_ascii_lowercase()).as_deref(),
            Some("0" | "1" | "0px" | "1px")
        )
    };

    if is_zero_or_one(get_attr("width")) && is_zero_or_one(get_attr("height")) {
        return true;
    }

    if let Some(style) = get_attr("style") {
        let compact: String = style
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        if compact.contains("display:none")
            || compact.contains("visibility:hidden")
            || ((compact.contains("width:1px") || compact.contains("width:0"))
                && (compact.contains("height:1px") || compact.contains("height:0")))
        {
            return true;
        }
    }

    false
}

fn resolve_url_value(raw_val: &str, base: Option<&Url>) -> Option<String> {
    let decoded = decode_basic_html_entities(raw_val);
    let trimmed = decoded.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Do not attempt relative resolution on explicit non-HTTP schemes like `javascript:` or `mailto:`.
    if let Ok(parsed) = Url::parse(trimmed) {
        return Some(parsed.to_string());
    }

    if let Some(base_url) = base
        && let Ok(joined) = base_url.join(trimmed)
    {
        return Some(joined.to_string());
    }

    Some(trimmed.to_string())
}

fn resolve_srcset_value(raw_srcset: &str, base: &Url) -> String {
    let mut parts = Vec::new();
    for candidate in raw_srcset.split(',') {
        let trimmed = candidate.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut iter = trimmed.split_ascii_whitespace();
        let Some(url_part) = iter.next() else {
            continue;
        };
        let descriptor: Vec<&str> = iter.collect();
        let resolved =
            resolve_url_value(url_part, Some(base)).unwrap_or_else(|| url_part.to_string());
        if descriptor.is_empty() {
            parts.push(resolved);
        } else {
            parts.push(format!("{resolved} {}", descriptor.join(" ")));
        }
    }
    parts.join(", ")
}

fn parse_html_attrs(s: &str) -> Vec<ParsedAttr> {
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
                out.push(ParsedAttr {
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

            out.push(ParsedAttr {
                name,
                value: Some(val),
            });
        } else {
            out.push(ParsedAttr { name, value: None });
        }
    }

    out
}

fn push_escaped_attr(out: &mut String, val: &str) {
    for ch in val.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            other => out.push(other),
        }
    }
}

/// Extract inline `<img>`, `<video>`, `<audio>`, and `<source>` media attachments from sanitized HTML.
pub fn extract_inline_media_from_html(sanitized_html: &str) -> Vec<MediaAttachment> {
    let mut media = Vec::new();
    let mut seen_urls = HashSet::new();
    let mut media_context: Option<MediaAttachmentKind> = None;

    let bytes = sanitized_html.as_bytes();
    let mut idx = 0;

    while idx < bytes.len() {
        let Some(rel_lt) = sanitized_html[idx..].find('<') else {
            break;
        };
        let lt = idx + rel_lt;
        let Some(tag_end) = find_tag_end(&sanitized_html[lt..]) else {
            break;
        };
        let full_tag = &sanitized_html[lt + 1..lt + tag_end];
        idx = lt + tag_end + 1;

        let trimmed = full_tag.trim();
        if let Some(closing) = trimmed.strip_prefix('/') {
            let close_name = closing.trim().to_ascii_lowercase();
            if matches!(close_name.as_str(), "video" | "audio" | "picture") {
                media_context = None;
            }
            continue;
        }

        let self_closing = trimmed.ends_with('/');
        let body = trimmed.strip_suffix('/').unwrap_or(trimmed);
        let name_end = body
            .find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(body.len());
        let tag_lower = body[..name_end].to_ascii_lowercase();
        let attrs = parse_html_attrs(&body[name_end..]);

        let attr_val = |key: &str| -> Option<String> {
            attrs
                .iter()
                .find(|a| a.name.eq_ignore_ascii_case(key))
                .and_then(|a| a.value.as_deref())
                .map(decode_basic_html_entities)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };

        match tag_lower.as_str() {
            "img" => {
                if let Some(src) = attr_val("src")
                    && is_http_or_https(&src)
                    && seen_urls.insert(src.clone())
                {
                    let caption = attr_val("alt").or_else(|| attr_val("title"));
                    media.push(MediaAttachment {
                        mime_type: guess_mime_from_url(&src),
                        url: src,
                        kind: MediaAttachmentKind::Image,
                        length_bytes: None,
                        caption,
                    });
                }
            }
            "video" => {
                if !self_closing {
                    media_context = Some(MediaAttachmentKind::Video);
                }
                if let Some(src) = attr_val("src")
                    && is_http_or_https(&src)
                    && seen_urls.insert(src.clone())
                {
                    media.push(MediaAttachment {
                        mime_type: guess_mime_from_url(&src),
                        url: src,
                        kind: MediaAttachmentKind::Video,
                        length_bytes: None,
                        caption: attr_val("title"),
                    });
                }
            }
            "audio" => {
                if !self_closing {
                    media_context = Some(MediaAttachmentKind::Audio);
                }
                if let Some(src) = attr_val("src")
                    && is_http_or_https(&src)
                    && seen_urls.insert(src.clone())
                {
                    media.push(MediaAttachment {
                        mime_type: guess_mime_from_url(&src),
                        url: src,
                        kind: MediaAttachmentKind::Audio,
                        length_bytes: None,
                        caption: attr_val("title"),
                    });
                }
            }
            "picture" => {
                if !self_closing {
                    media_context = Some(MediaAttachmentKind::Image);
                }
            }
            "source" => {
                // Skip `<picture><source>` responsive duplicates so `<picture>` yields a single `<img>`.
                if media_context == Some(MediaAttachmentKind::Image) {
                    continue;
                }
                if let Some(src) = attr_val("src")
                    && is_http_or_https(&src)
                    && seen_urls.insert(src.clone())
                {
                    let mime = attr_val("type").or_else(|| guess_mime_from_url(&src));
                    let kind =
                        media_context.unwrap_or_else(|| classify_media_kind(mime.as_deref(), &src));
                    media.push(MediaAttachment {
                        url: src,
                        mime_type: mime,
                        kind,
                        length_bytes: None,
                        caption: None,
                    });
                }
            }
            _ => {}
        }
    }

    media
}

/// Classify [`MediaAttachmentKind`] from optional MIME type and URL file extension.
pub fn classify_media_kind(mime_type: Option<&str>, url: &str) -> MediaAttachmentKind {
    if let Some(mime) = mime_type {
        let lower = mime.trim().to_ascii_lowercase();
        let essence = lower.split(';').next().unwrap_or(&lower).trim();
        if essence.starts_with("image/") {
            return MediaAttachmentKind::Image;
        }
        if essence.starts_with("video/") {
            return MediaAttachmentKind::Video;
        }
        if essence.starts_with("audio/") {
            return MediaAttachmentKind::Audio;
        }
        if !essence.is_empty() && essence != "application/octet-stream" {
            return MediaAttachmentKind::Document;
        }
    }

    let path = Url::parse(url)
        .ok()
        .map(|u| u.path().to_ascii_lowercase())
        .unwrap_or_else(|| url.to_ascii_lowercase());

    if path.ends_with(".jpg")
        || path.ends_with(".jpeg")
        || path.ends_with(".png")
        || path.ends_with(".gif")
        || path.ends_with(".webp")
        || path.ends_with(".avif")
        || path.ends_with(".bmp")
    {
        MediaAttachmentKind::Image
    } else if path.ends_with(".mp4")
        || path.ends_with(".webm")
        || path.ends_with(".mov")
        || path.ends_with(".m4v")
        || path.ends_with(".mkv")
    {
        MediaAttachmentKind::Video
    } else if path.ends_with(".mp3")
        || path.ends_with(".m4a")
        || path.ends_with(".ogg")
        || path.ends_with(".opus")
        || path.ends_with(".wav")
        || path.ends_with(".flac")
        || path.ends_with(".aac")
    {
        MediaAttachmentKind::Audio
    } else {
        MediaAttachmentKind::Document
    }
}

fn guess_mime_from_url(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    let guess = mime_guess::from_path(parsed.path()).first()?;
    Some(guess.essence_str().to_string())
}

fn is_http_or_https(url: &str) -> bool {
    Url::parse(url)
        .ok()
        .is_some_and(|u| matches!(u.scheme(), "http" | "https"))
}

/// Deduplicate media attachments by normalized URL while preserving discovery order.
pub fn deduplicate_media(items: impl IntoIterator<Item = MediaAttachment>) -> Vec<MediaAttachment> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for item in items {
        let trimmed = item.url.trim();
        if !is_http_or_https(trimmed) {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            out.push(MediaAttachment {
                url: trimmed.to_string(),
                ..item
            });
        }
    }
    out
}

/// Convert HTML into clean, human-readable plain text without Markdown link footnotes or tags.
pub fn html_to_plain_text(html: &str) -> String {
    if html.trim().is_empty() {
        return String::new();
    }

    let mut raw_text = String::with_capacity(html.len());
    let bytes = html.as_bytes();
    let mut idx = 0;
    let mut skip_until_close: Option<&'static str> = None;

    while idx < bytes.len() {
        if let Some(close_tag) = skip_until_close {
            let pat = format!("</{close_tag}");
            if let Some(pos) = html[idx..].to_ascii_lowercase().find(&pat) {
                let close_start = idx + pos;
                if let Some(end_gt) = html[close_start..].find('>') {
                    idx = close_start + end_gt + 1;
                    skip_until_close = None;
                    continue;
                }
            }
            break;
        }

        let Some(rel_lt) = html[idx..].find('<') else {
            raw_text.push_str(&decode_basic_html_entities(&html[idx..]));
            break;
        };
        let lt = idx + rel_lt;
        if lt > idx {
            raw_text.push_str(&decode_basic_html_entities(&html[idx..lt]));
        }

        if html[lt..].starts_with("<!--") {
            if let Some(end_comment) = html[lt + 4..].find("-->") {
                idx = lt + 4 + end_comment + 3;
                continue;
            }
            break;
        }

        let Some(tag_end) = find_tag_end(&html[lt..]) else {
            break;
        };
        let tag_content = html[lt + 1..lt + tag_end].trim();
        idx = lt + tag_end + 1;

        let is_closing = tag_content.starts_with('/');
        let clean = tag_content
            .trim_start_matches('/')
            .trim_end_matches('/')
            .trim();
        let name_end = clean
            .find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(clean.len());
        let tag_lower = clean[..name_end].to_ascii_lowercase();

        if !is_closing && !tag_content.ends_with('/') {
            match tag_lower.as_str() {
                "script" => {
                    skip_until_close = Some("script");
                    continue;
                }
                "style" => {
                    skip_until_close = Some("style");
                    continue;
                }
                _ => {}
            }
        }

        match tag_lower.as_str() {
            "p" | "br" | "div" | "article" | "section" | "header" | "footer" | "blockquote"
            | "pre" | "li" | "ul" | "ol" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
            | "hr" | "figcaption" | "figure" => {
                raw_text.push('\n');
            }
            "td" | "th" => {
                raw_text.push(' ');
            }
            _ => {}
        }
    }

    let mut paragraphs = Vec::new();
    for line in raw_text.lines() {
        let collapsed: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !collapsed.is_empty() {
            paragraphs.push(collapsed);
        }
    }
    paragraphs.join("\n")
}

/// Produce a bounded plain-text summary (`<= MAX_SUMMARY_CHARS`).
pub fn extract_summary_text(plain_text: &str) -> Option<String> {
    let collapsed = plain_text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let char_count = collapsed.chars().count();
    if char_count <= MAX_SUMMARY_CHARS {
        return Some(collapsed);
    }
    let truncated: String = collapsed.chars().take(MAX_SUMMARY_CHARS - 1).collect();
    Some(format!("{}…", truncated.trim_end()))
}

/// Extract the first complete sentence using Unicode UAX #29 sentence boundaries
/// (`unicode_segmentation::UnicodeSegmentation::unicode_sentences`), stripping any trailing
/// English period (`.`), CJK period (`。`), or whitespace (`feedcourier.md` Section 五.2).
pub fn extract_first_sentence(plain_text: &str) -> Option<String> {
    let first_paragraph = plain_text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;

    let segments: Vec<&str> = first_paragraph
        .unicode_sentences()
        .map(str::trim)
        .filter(|seg| !seg.is_empty())
        .collect();

    if segments.is_empty() {
        return strip_trailing_sentence_period(first_paragraph);
    }

    let mut combined = String::from(segments[0]);
    for &next_seg in &segments[1..] {
        if ends_with_common_abbreviation(&combined) {
            combined.push(' ');
            combined.push_str(next_seg);
        } else {
            break;
        }
    }

    let cleaned = strip_trailing_sentence_period(&combined)?;
    if cleaned.chars().count() <= MAX_FIRST_SENTENCE_CHARS {
        Some(cleaned)
    } else {
        let truncated: String = cleaned.chars().take(MAX_FIRST_SENTENCE_CHARS).collect();
        strip_trailing_sentence_period(&truncated)
    }
}

/// Strip trailing `.` and `。` (plus surrounding whitespace) cleanly for English and CJK text.
pub fn strip_trailing_sentence_period(text: &str) -> Option<String> {
    let stripped = text
        .trim()
        .trim_end_matches(|c: char| c.is_whitespace() || c == '.' || c == '。')
        .trim();
    if stripped.is_empty() {
        None
    } else {
        Some(stripped.to_string())
    }
}

fn ends_with_common_abbreviation(s: &str) -> bool {
    let last_word = s
        .split_whitespace()
        .last()
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        last_word.as_str(),
        "mr." | "mrs." | "ms." | "dr." | "prof." | "sr." | "jr." | "st." | "vs." | "e.g." | "i.e."
    )
}

fn decode_basic_html_entities(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }

    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(amp_idx) = rest.find('&') {
        out.push_str(&rest[..amp_idx]);
        let after_amp = &rest[amp_idx + 1..];
        let max_entity_len = after_amp.len().min(16);
        if let Some(semi_idx) = after_amp[..max_entity_len].find(';') {
            let entity = &after_amp[..semi_idx];
            if let Some(decoded) = resolve_html_entity(entity) {
                out.push_str(&decoded);
                rest = &after_amp[semi_idx + 1..];
                continue;
            }
        }
        out.push('&');
        rest = after_amp;
    }
    out.push_str(rest);
    out
}

fn resolve_html_entity(entity: &str) -> Option<String> {
    if let Some(hex_str) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        let code = u32::from_str_radix(hex_str, 16).ok()?;
        let ch = char::from_u32(code)?;
        return Some(ch.to_string());
    }
    if let Some(dec_str) = entity.strip_prefix('#') {
        let code = dec_str.parse::<u32>().ok()?;
        let ch = char::from_u32(code)?;
        return Some(ch.to_string());
    }
    let named = match entity {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" | "#39" => "'",
        "nbsp" => " ",
        "mdash" => "—",
        "ndash" => "–",
        "hellip" => "…",
        "rsquo" => "\u{2019}",
        "lsquo" => "\u{2018}",
        "rdquo" => "\u{201d}",
        "ldquo" => "\u{201c}",
        "copy" => "©",
        "reg" => "®",
        _ => return None,
    };
    Some(named.to_string())
}
