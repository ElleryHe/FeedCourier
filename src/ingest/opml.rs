//! OPML 2.0 subscription import and export (`quick-xml`), mapping nested `<outline>` folders to
//! feed `tags` (`feedcourier.md` Sections 二.2, 三.2, 四.2).

use std::collections::BTreeMap;

use anyhow::anyhow;
use chrono::Utc;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer, XmlVersion};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::db::Database;
use crate::db::models::{Feed, NewFeed};
use crate::error::{AppError, AppResult};

/// Default polling interval (`30m`) assigned to newly imported OPML feeds.
pub const DEFAULT_IMPORTED_POLL_INTERVAL_SECS: i64 = 1800;

/// Single feed subscription extracted from an OPML `<outline xmlUrl="..."/>` node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpmlFeedOutline {
    pub xml_url: String,
    pub title: String,
    pub html_url: Option<String>,
    pub tags: Vec<String>,
}

/// Result of importing an OPML document into SQLite (matches `OpmlImportResponse` in `web/src/api/types.ts`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpmlImportResult {
    pub imported_count: usize,
    pub skipped_existing_count: usize,
    pub feeds: Vec<Feed>,
}

/// Parse an OPML 1.0 / 2.0 XML document into deduplicated [`OpmlFeedOutline`] entries.
///
/// - Supports flat `<outline xmlUrl="..."/>` nodes as well as arbitrarily nested folder trees
///   (`<outline text="Tech"><outline text="Rust"><outline xmlUrl="..."/></outline></outline>`),
///   converting ancestor folder names (and optional `category` attributes) into `tags`.
/// - If the same `xmlUrl` appears under multiple folders in one OPML document, its tags are merged
///   rather than duplicating the feed URL.
pub fn parse_opml(opml_xml: &str) -> AppResult<Vec<OpmlFeedOutline>> {
    let trimmed = opml_xml.trim_start_matches('\u{feff}').trim();
    if trimmed.is_empty() {
        return Err(AppError::Validation(
            "OPML XML document is empty".to_string(),
        ));
    }

    let mut reader = Reader::from_str(trimmed);
    reader.config_mut().trim_text(true);

    let mut saw_opml_root = false;
    let mut folder_stack: Vec<Option<String>> = Vec::new();
    let mut outlines: Vec<OpmlFeedOutline> = Vec::new();
    let mut url_index: BTreeMap<String, usize> = BTreeMap::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(elem)) => {
                let local_lower = local_tag_name_lower(&elem);
                if local_lower == "opml" {
                    saw_opml_root = true;
                } else if local_lower == "outline" {
                    let attrs = parse_outline_attrs(&elem)?;
                    if let Some(feed_outline) = build_feed_outline(&attrs, &folder_stack)? {
                        upsert_outline(&mut outlines, &mut url_index, feed_outline);
                        folder_stack.push(None);
                    } else {
                        let folder_name = attrs
                            .title
                            .or(attrs.text)
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty());
                        folder_stack.push(folder_name);
                    }
                }
            }
            Ok(Event::Empty(elem)) => {
                let local_lower = local_tag_name_lower(&elem);
                if local_lower == "opml" {
                    saw_opml_root = true;
                } else if local_lower == "outline" {
                    let attrs = parse_outline_attrs(&elem)?;
                    if let Some(feed_outline) = build_feed_outline(&attrs, &folder_stack)? {
                        upsert_outline(&mut outlines, &mut url_index, feed_outline);
                    }
                }
            }
            Ok(Event::End(elem)) => {
                let local_name = elem.local_name();
                if local_name.as_ref().eq_ignore_ascii_case("outline") {
                    folder_stack.pop();
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(err) => {
                return Err(AppError::Validation(format!("malformed OPML XML: {err}")));
            }
        }
    }

    if !saw_opml_root {
        return Err(AppError::Validation(
            "invalid OPML document: missing <opml> root element".to_string(),
        ));
    }

    Ok(outlines)
}

/// Export a slice of [`Feed`] rows as valid, indented UTF-8 OPML 2.0 XML.
///
/// - Feeds with one or more tags are grouped into `<outline text="{primary_tag}" title="{primary_tag}">`
///   folders (preserving any secondary tags in `category="..."` for lossless round-trip import).
/// - Untagged feeds are emitted directly at the root of `<body>`.
pub fn export_opml(feeds: &[Feed]) -> AppResult<String> {
    let mut writer = Writer::new_with_indent(Vec::new(), b' ', 2);

    writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;

    let mut opml_start = BytesStart::new("opml");
    opml_start.push_attribute(("version", "2.0"));
    writer.write_event(Event::Start(opml_start))?;

    // <head>
    writer.write_event(Event::Start(BytesStart::new("head")))?;
    write_text_element(&mut writer, "title", "FeedCourier Subscriptions")?;
    write_text_element(&mut writer, "dateCreated", &Utc::now().to_rfc2822())?;
    writer.write_event(Event::End(BytesEnd::new("head")))?;

    // <body>
    writer.write_event(Event::Start(BytesStart::new("body")))?;

    let mut grouped: BTreeMap<String, Vec<&Feed>> = BTreeMap::new();
    let mut untagged: Vec<&Feed> = Vec::new();

    for feed in feeds {
        let clean_tags: Vec<String> = feed
            .tags()
            .into_iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        if let Some(primary) = clean_tags.first() {
            grouped.entry(primary.clone()).or_default().push(feed);
        } else {
            untagged.push(feed);
        }
    }

    for (folder_tag, folder_feeds) in grouped {
        let mut folder_elem = BytesStart::new("outline");
        folder_elem.push_attribute(("text", folder_tag.as_str()));
        folder_elem.push_attribute(("title", folder_tag.as_str()));
        writer.write_event(Event::Start(folder_elem))?;

        for feed in folder_feeds {
            write_feed_outline(&mut writer, feed)?;
        }

        writer.write_event(Event::End(BytesEnd::new("outline")))?;
    }

    for feed in untagged {
        write_feed_outline(&mut writer, feed)?;
    }

    writer.write_event(Event::End(BytesEnd::new("body")))?;
    writer.write_event(Event::End(BytesEnd::new("opml")))?;

    let bytes = writer.into_inner();
    let mut xml = String::from_utf8(bytes)
        .map_err(|e| AppError::Internal(anyhow!("OPML UTF-8 conversion error: {e}")))?;
    xml.push('\n');
    Ok(xml)
}

/// Parse OPML XML and insert any non-duplicate feeds into SQLite (`silent_init` controls whether
/// initial historical items are silently fingerprinted on the first poll).
pub async fn import_opml_into_db(
    db: &Database,
    opml_xml: &str,
    silent_init: bool,
) -> AppResult<OpmlImportResult> {
    let outlines = parse_opml(opml_xml)?;
    let mut imported_count = 0usize;
    let mut skipped_existing_count = 0usize;
    let mut created_feeds = Vec::new();

    for outline in outlines {
        if db.get_feed_by_url(&outline.xml_url).await?.is_some() {
            skipped_existing_count += 1;
            continue;
        }

        let mut new_feed = NewFeed::new(outline.xml_url);
        new_feed.title = outline.title;
        new_feed.site_url = outline.html_url;
        new_feed.tags = outline.tags;
        new_feed.poll_interval_secs = DEFAULT_IMPORTED_POLL_INTERVAL_SECS;
        new_feed.silent_init = silent_init;

        let feed = db.create_feed(&new_feed).await?;
        imported_count += 1;
        created_feeds.push(feed);
    }

    Ok(OpmlImportResult {
        imported_count,
        skipped_existing_count,
        feeds: created_feeds,
    })
}

#[derive(Default)]
struct RawOutlineAttrs {
    xml_url: Option<String>,
    html_url: Option<String>,
    title: Option<String>,
    text: Option<String>,
    category: Option<String>,
}

fn local_tag_name_lower(elem: &BytesStart<'_>) -> String {
    elem.local_name().as_ref().to_ascii_lowercase()
}

fn parse_outline_attrs(elem: &BytesStart<'_>) -> AppResult<RawOutlineAttrs> {
    let mut out = RawOutlineAttrs::default();
    for attr_res in elem.attributes() {
        let attr = attr_res
            .map_err(|err| AppError::Validation(format!("invalid OPML attribute syntax: {err}")))?;
        let key_lower = attr.key.local_name().as_ref().to_ascii_lowercase();
        let val = attr
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|err| AppError::Validation(format!("invalid OPML XML entity: {err}")))?
            .into_owned();

        match key_lower.as_str() {
            "xmlurl" => out.xml_url = Some(val),
            "htmlurl" => out.html_url = Some(val),
            "title" => out.title = Some(val),
            "text" => out.text = Some(val),
            "category" => out.category = Some(val),
            _ => {}
        }
    }
    Ok(out)
}

fn build_feed_outline(
    attrs: &RawOutlineAttrs,
    folder_stack: &[Option<String>],
) -> AppResult<Option<OpmlFeedOutline>> {
    let Some(raw_xml_url) = attrs
        .xml_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };

    let parsed_xml_url = Url::parse(raw_xml_url).map_err(|err| {
        AppError::Validation(format!(
            "invalid feed xmlUrl '{raw_xml_url}' in OPML: {err}"
        ))
    })?;
    if !matches!(parsed_xml_url.scheme(), "http" | "https") {
        return Err(AppError::Validation(format!(
            "unsupported feed xmlUrl scheme '{}' in OPML (expected http or https)",
            parsed_xml_url.scheme()
        )));
    }

    let html_url = attrs
        .html_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|raw| Url::parse(raw).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .map(|u| u.to_string());

    let title = attrs
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            attrs
                .text
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("")
        .to_string();

    let mut tags = Vec::new();
    for folder in folder_stack.iter().flatten() {
        push_unique_tag(&mut tags, folder);
    }
    if let Some(cat_str) = attrs.category.as_deref() {
        for piece in cat_str.split([',', '/']) {
            push_unique_tag(&mut tags, piece);
        }
    }

    Ok(Some(OpmlFeedOutline {
        xml_url: raw_xml_url.to_string(),
        title,
        html_url,
        tags,
    }))
}

fn upsert_outline(
    outlines: &mut Vec<OpmlFeedOutline>,
    url_index: &mut BTreeMap<String, usize>,
    incoming: OpmlFeedOutline,
) {
    if let Some(&idx) = url_index.get(&incoming.xml_url) {
        let existing = &mut outlines[idx];
        if existing.title.is_empty() && !incoming.title.is_empty() {
            existing.title = incoming.title;
        }
        if existing.html_url.is_none() && incoming.html_url.is_some() {
            existing.html_url = incoming.html_url;
        }
        for tag in incoming.tags {
            push_unique_tag(&mut existing.tags, &tag);
        }
    } else {
        let idx = outlines.len();
        url_index.insert(incoming.xml_url.clone(), idx);
        outlines.push(incoming);
    }
}

fn push_unique_tag(tags: &mut Vec<String>, candidate: &str) {
    let trimmed = candidate.trim();
    if trimmed.is_empty() {
        return;
    }
    let lower = trimmed.to_lowercase();
    if !tags.iter().any(|existing| existing.to_lowercase() == lower) {
        tags.push(trimmed.to_string());
    }
}

fn write_text_element(writer: &mut Writer<Vec<u8>>, tag: &str, text: &str) -> AppResult<()> {
    writer.write_event(Event::Start(BytesStart::new(tag)))?;
    writer.write_event(Event::Text(BytesText::new(text)))?;
    writer.write_event(Event::End(BytesEnd::new(tag)))?;
    Ok(())
}

fn write_feed_outline(writer: &mut Writer<Vec<u8>>, feed: &Feed) -> AppResult<()> {
    let display_title = if feed.title.trim().is_empty() {
        feed.url.as_str()
    } else {
        feed.title.trim()
    };

    let mut elem = BytesStart::new("outline");
    elem.push_attribute(("type", "rss"));
    elem.push_attribute(("text", display_title));
    elem.push_attribute(("title", display_title));
    elem.push_attribute(("xmlUrl", feed.url.trim()));

    if let Some(site_url) = feed
        .site_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        elem.push_attribute(("htmlUrl", site_url));
    }

    let clean_tags: Vec<String> = feed
        .tags()
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if clean_tags.len() > 1 {
        let category_csv = clean_tags.join(",");
        elem.push_attribute(("category", category_csv.as_str()));
    }

    writer.write_event(Event::Empty(elem))?;
    Ok(())
}
