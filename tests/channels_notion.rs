//! Comprehensive unit and `wiremock` integration tests for the Notion Channel Adapter
//! (`Notion-Version: 2026-03-11`, Markdown v2 normalizer, dual onboarding helpers, and
//! crash-safe effective-once outbox state machine).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_test as _;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use feedcourier::channels::notion::{
    MAX_MARKDOWN_BYTES, MAX_NOTION_BLOCKS, NOTION_API_VERSION, NotionColumnCheckStatus,
    NotionInitDbRequest, NotionInspectSchemaRequest, NotionSender, NotionStandardFieldKey,
    entry_to_notion_markdown_v2, html_to_notion_markdown_v2, normalize_markdown_v2,
    normalize_notion_id,
};
use feedcourier::channels::{
    ChannelConfig, ChannelKind, ChannelSender, CheckpointSink, DeliveryCheckpoint, DeliveryContext,
    DeliveryOutcome, NotionChannelConfig, NotionColumnMapping, RouteOptions,
};
use feedcourier::config::AppConfig;
use feedcourier::crypto::{MasterKey, sha256_hex};
use feedcourier::db::Database;
use feedcourier::db::models::{
    Channel, DeliveryTask, DeliveryTaskStatus, Entry, EntryInsertOutcome, Feed, FeedHealthStatus,
    NewChannel, NewEntry, NewFeed, NewRoute, Route,
};
use feedcourier::error::AppResult;

#[derive(Default)]
struct RecordingCheckpointSink {
    saved: Mutex<Vec<DeliveryCheckpoint>>,
}

impl RecordingCheckpointSink {
    fn history(&self) -> Vec<DeliveryCheckpoint> {
        self.saved.lock().expect("checkpoint mutex").clone()
    }
}

#[async_trait]
impl CheckpointSink for RecordingCheckpointSink {
    async fn save_checkpoint(&self, checkpoint: &DeliveryCheckpoint) -> AppResult<()> {
        self.saved
            .lock()
            .expect("checkpoint mutex")
            .push(checkpoint.clone());
        Ok(())
    }
}

struct SampleFixtures {
    feed: Feed,
    entry: Entry,
    channel: Channel,
    channel_config: ChannelConfig,
    route: Route,
    task: DeliveryTask,
}

fn sample_fixtures(data_source_id: &str) -> SampleFixtures {
    let cfg = ChannelConfig::Notion(NotionChannelConfig {
        integration_secret: "ntn_secret_test_token_123456".to_string(),
        data_source_id: data_source_id.to_string(),
        database_id: Some("11111111-2222-3333-4444-555555555555".to_string()),
        parent_page_id: Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string()),
        column_mapping: NotionColumnMapping::standard(),
    });

    let feed = Feed {
        id: 1,
        url: "https://blog.example.com/atom.xml".to_string(),
        title: "Rust, Systems, & Co.".to_string(),
        site_url: Some("https://blog.example.com".to_string()),
        favicon_url: Some("https://blog.example.com/favicon.ico".to_string()),
        tags_json: serde_json::to_string(&vec!["rust", "distributed, systems", "rust"])
            .expect("tags json"),
        poll_interval_secs: 1800,
        fetch_full_content: false,
        use_proxy: false,
        etag: None,
        last_modified: None,
        health_status: FeedHealthStatus::Active,
        consecutive_failures: 0,
        next_poll_at: None,
        last_error: None,
        silent_init: true,
        created_at: 1_700_000_000,
        updated_at: 1_700_000_000,
    };

    let entry = Entry {
        id: 10,
        feed_id: 1,
        fingerprint_sha256: sha256_hex(b"https://blog.example.com/posts/notion-2026"),
        published_at: Some(1_762_992_000),
        discovered_at: 1_762_992_060,
        payload_purged_at: None,
        title: Some("Zero-Bloat Notion Delivery".to_string()),
        canonical_url: Some("https://blog.example.com/posts/notion-2026".to_string()),
        author: Some("Ellery".to_string()),
        content_html: Some(
            "<h2>Overview</h2><p>Streaming RSS straight into Notion <code>data_source_id</code>.</p>"
                .to_string(),
        ),
        first_sentence: Some("Streaming RSS straight into Notion.".to_string()),
        summary_text: Some("Summary fallback".to_string()),
        media_json: None,
    };

    let channel = Channel {
        id: 5,
        name: "Knowledge Base".to_string(),
        kind: ChannelKind::Notion,
        encrypted_config: Vec::new(),
        credential_group_key: cfg.credential_group_key(),
        capabilities_json: "{}".to_string(),
        min_interval_secs: 2,
        jitter_max_secs: 2,
        hourly_quota: None,
        daily_quota: None,
        use_proxy: false,
        cooldown_until: None,
        enabled: true,
        created_at: 1_700_000_000,
        updated_at: 1_700_000_000,
    };

    let route = Route {
        id: 7,
        name: "Tech -> Notion".to_string(),
        feed_ids_json: "[1]".to_string(),
        match_tags_json: "[]".to_string(),
        channel_id: 5,
        include_keywords_json: "[]".to_string(),
        exclude_keywords_json: "[]".to_string(),
        include_regex: None,
        exclude_regex: None,
        options_json: "{}".to_string(),
        enabled: true,
        created_at: 1_700_000_000,
        updated_at: 1_700_000_000,
    };

    let task = DeliveryTask {
        id: 99,
        route_id: 7,
        channel_id: 5,
        entry_id: 10,
        status: DeliveryTaskStatus::Delivering,
        lease_until: Some(1_762_992_300),
        checkpoint_json: None,
        attempt_count: 1,
        next_attempt_at: 1_762_992_060,
        last_status_code: None,
        diagnostic_log: None,
        completed_at: None,
        created_at: 1_762_992_060,
    };

    SampleFixtures {
        feed,
        entry,
        channel,
        channel_config: cfg,
        route,
        task,
    }
}

#[test]
fn normalize_notion_id_extracts_uuids_from_urls_and_hex() {
    let expected = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";

    assert_eq!(
        normalize_notion_id("a1b2c3d4e5f67890abcdef1234567890").unwrap(),
        expected
    );
    assert_eq!(
        normalize_notion_id("  A1B2C3D4-E5F6-7890-ABCD-EF1234567890  ").unwrap(),
        expected
    );
    // Must pick the database/page UUID from the path rather than the `?v=` view query param.
    assert_eq!(
        normalize_notion_id(
            "https://www.notion.so/myworkspace/FeedCourier-Inbox-a1b2c3d4e5f67890abcdef1234567890?v=99999999999999999999999999999999&pvs=4"
        )
        .unwrap(),
        expected
    );
    assert_eq!(
        normalize_notion_id("collection://a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap(),
        expected
    );

    assert!(normalize_notion_id("").is_err());
    assert!(normalize_notion_id("   ").is_err());
    assert!(normalize_notion_id("https://www.notion.so/workspace/no-uuid-here").is_err());
}

#[test]
fn normalize_markdown_v2_expands_outer_code_fences_for_inner_backticks() {
    // 1. Raw Markdown containing a nested ```rust ... ``` example inside ```markdown ... ```,
    //    followed by an ordinary sequential ```go ... ``` block.
    let raw_md = "\
# Fence Rule

```markdown
Embed Rust inside Markdown like so:
```rust
fn main() {
    println!(\"hello\");
}
```
 End of nested snippet.
```

Regular block without inner triple-backticks:

```go
package main
```";

    let normalized = normalize_markdown_v2(raw_md, Some("https://example.com/fences"));
    assert!(!normalized.truncated);
    assert!(
        normalized.markdown.contains("````markdown\nEmbed Rust"),
        "outer opening fence must expand to 4 backticks:\n{}",
        normalized.markdown
    );
    assert!(
        normalized
            .markdown
            .contains("End of nested snippet.\n````\n\nRegular block"),
        "outer closing fence must expand to 4 backticks without swallowing subsequent blocks:\n{}",
        normalized.markdown
    );
    assert!(
        normalized.markdown.contains("```go\npackage main\n```"),
        "ordinary code block without inner >=3 backticks stays at 3 backticks:\n{}",
        normalized.markdown
    );

    // 2. HTML `<pre><code>` containing 4 consecutive backticks must expand outer fence to 5!
    let html = "<pre><code class=\"language-md\">Outer example:\n````rust\nlet raw = \"```\";\n````\n</code></pre>";
    let from_html = html_to_notion_markdown_v2(html, None);
    assert!(
        from_html.markdown.starts_with("`````md\n"),
        "4 inner backticks must expand outer fence to 5 backticks:\n{}",
        from_html.markdown
    );
    assert!(
        from_html.markdown.ends_with("\n`````"),
        "closing fence must match 5 backticks:\n{}",
        from_html.markdown
    );

    // 3. Tilde fence `~~~` containing triple backticks converts to 4-backtick Notion v2 fence.
    let tilde_md = "~~~sh\ncat <<'EOF'\n```json\n{\"ok\":true}\n```\nEOF\n~~~";
    let from_tilde = normalize_markdown_v2(tilde_md, None);
    assert!(from_tilde.markdown.starts_with("````sh\n"));
    assert!(from_tilde.markdown.ends_with("\n````"));
}

#[test]
fn normalize_markdown_v2_truncates_over_5000_blocks_and_byte_caps_cleanly() {
    let canonical = "https://example.com/articles/deep-dive";

    // Build 5,250 distinct paragraph blocks (> Notion's 5,000 block limit & MAX_NOTION_BLOCKS = 4,800).
    let mut blocks = Vec::with_capacity(5_250);
    for idx in 1..=5_250 {
        blocks.push(format!("Paragraph block #{idx} with clean text."));
    }
    let huge_md = blocks.join("\n\n");

    let res = normalize_markdown_v2(&huge_md, Some(canonical));
    assert!(res.truncated, "5,250 blocks must trigger truncation");
    assert_eq!(
        res.block_count, MAX_NOTION_BLOCKS,
        "truncated output must stay within MAX_NOTION_BLOCKS ({MAX_NOTION_BLOCKS})"
    );
    assert!(
        res.markdown
            .contains("Paragraph block #4798 with clean text."),
        "must retain blocks right up to the budget boundary"
    );
    assert!(
        !res.markdown.contains("Paragraph block #4799"),
        "blocks past the boundary must be dropped cleanly"
    );
    let expected_footer = format!(
        "\n\n---\n\n> Content truncated due to Notion block limit. [Read full original article]({canonical})"
    );
    assert!(
        res.markdown.ends_with(&expected_footer),
        "must append exact canonical URL truncation notice"
    );

    // Also verify byte cap (`MAX_MARKDOWN_BYTES = 450_000`) when block count is small (e.g. 600 × 1 KiB).
    let wide_para = "这是一个包含多字节 UTF-8 中文字符的段落。".repeat(30); // ~1.7 KiB each
    let byte_heavy_md = (0..350)
        .map(|i| format!("Block {i}: {wide_para}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    assert!(byte_heavy_md.len() > MAX_MARKDOWN_BYTES);

    let byte_res = normalize_markdown_v2(&byte_heavy_md, Some(canonical));
    assert!(byte_res.truncated);
    assert!(
        byte_res.markdown.len() <= MAX_MARKDOWN_BYTES,
        "byte-capped markdown ({}) must not exceed MAX_MARKDOWN_BYTES ({MAX_MARKDOWN_BYTES})",
        byte_res.markdown.len()
    );
    assert!(byte_res.markdown.ends_with(&expected_footer));
}

#[test]
fn entry_to_notion_markdown_v2_respects_fallback_hierarchy() {
    let (_, mut entry) = {
        let f = sample_fixtures("ds_1");
        (f.feed, f.entry)
    };
    entry.content_html = Some("   ".to_string());
    entry.summary_text = Some("Summary with ```\ninner\n```".to_string());
    let from_summary = entry_to_notion_markdown_v2(&entry);
    assert!(from_summary.markdown.contains("Summary"));

    entry.summary_text = None;
    entry.first_sentence = Some("First sentence fallback".to_string());
    let from_first = entry_to_notion_markdown_v2(&entry);
    assert_eq!(from_first.markdown, "First sentence fallback");
}

#[tokio::test]
async fn init_database_sends_2026_03_11_and_nests_7_columns_in_initial_data_source() {
    let server = MockServer::start().await;
    let parent_uuid = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
    let created_db_id = "12345678-1234-1234-1234-1234567890ab";
    let created_ds_id = "87654321-4321-4321-4321-ba0987654321";

    Mock::given(method("POST"))
        .and(path("/v1/databases"))
        .and(header("Authorization", "Bearer ntn_init_secret"))
        .and(header("Notion-Version", NOTION_API_VERSION))
        .and(header("Content-Type", "application/json"))
        .respond_with(move |req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).expect("json body");

            // Must NOT send legacy top-level `properties` or `archived`.
            assert!(
                body.get("properties").is_none(),
                "2026-03-11 forbids top-level database properties: {body}"
            );
            assert!(
                body.get("archived").is_none(),
                "2026-03-11 forbids legacy archived field: {body}"
            );

            assert_eq!(body["parent"]["type"], "page_id");
            assert_eq!(body["parent"]["page_id"], parent_uuid);
            assert_eq!(body["title"][0]["text"]["content"], "FeedCourier Inbox");

            let props = body["initial_data_source"]["properties"]
                .as_object()
                .expect("initial_data_source.properties object");
            assert_eq!(props.len(), 7);
            assert!(props["Title"]["title"].is_object());
            assert!(props["Link"]["url"].is_object());
            assert!(props["Source"]["select"]["options"].is_array());
            assert!(props["Published"]["date"].is_object());
            assert_eq!(props["State"]["select"]["options"][0]["name"], "UnRead");
            assert_eq!(props["State"]["select"]["options"][1]["name"], "Read");
            assert!(props["Tag"]["multi_select"]["options"].is_array());
            assert!(props["FC-ID"]["rich_text"].is_object());

            ResponseTemplate::new(200).set_body_json(json!({
                "object": "database",
                "id": created_db_id,
                "url": "https://www.notion.so/123456781234123412341234567890ab",
                "in_trash": false,
                "data_sources": [
                    { "id": created_ds_id, "name": "FeedCourier Inbox" }
                ]
            }))
        })
        .expect(1)
        .mount(&server)
        .await;

    let sender = NotionSender::with_base_url(server.uri(), None).expect("sender");
    let resp = sender
        .init_database(&NotionInitDbRequest {
            secret: "ntn_init_secret".to_string(),
            parent_page_id:
                "https://www.notion.so/team/Reading-Room-a1b2c3d4e5f67890abcdef1234567890?pvs=4"
                    .to_string(),
            title: None,
            use_proxy: false,
        })
        .await
        .expect("init_database should succeed");

    assert_eq!(resp.database_id, created_db_id);
    assert_eq!(resp.data_source_id, created_ds_id);
    assert_eq!(
        resp.database_url.as_deref(),
        Some("https://www.notion.so/123456781234123412341234567890ab")
    );
    assert_eq!(resp.column_mapping, NotionColumnMapping::standard());
}

#[tokio::test]
async fn inspect_schema_resolves_db_to_data_source_maps_name_title_and_patches_missing() {
    let server = MockServer::start().await;
    let db_id = "aaaaaaaa-1111-2222-3333-bbbbbbbbbbbb";
    let ds_id = "cccccccc-4444-5555-6666-dddddddddddd";

    // User pasted a database URL -> first GET /v1/data_sources/{db_id} returns 404 object_not_found,
    // triggering automatic fallback to GET /v1/databases/{db_id} -> GET /v1/data_sources/{ds_id}!
    Mock::given(method("GET"))
        .and(path(format!("/v1/data_sources/{db_id}")))
        .and(header("Notion-Version", NOTION_API_VERSION))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "object": "error",
            "status": 404,
            "code": "object_not_found",
            "message": "Could not find data_source with ID."
        })))
        .expect(2)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!("/v1/databases/{db_id}")))
        .and(header("Notion-Version", NOTION_API_VERSION))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "database",
            "id": db_id,
            "title": [{ "plain_text": "My Reading Tracker" }],
            "data_sources": [{ "id": ds_id, "name": "Articles" }]
        })))
        .expect(2)
        .mount(&server)
        .await;

    // Existing user data source has `"Name"` (`title`), `"Link"` (`url`), and a mismatched
    // `"Source"` (`rich_text` instead of `select`), while `"FC-ID"`, `"Published"`, `"State"`, `"Tag"` are missing.
    Mock::given(method("GET"))
        .and(path(format!("/v1/data_sources/{ds_id}")))
        .and(header("Notion-Version", NOTION_API_VERSION))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "data_source",
            "id": ds_id,
            "parent": { "type": "database_id", "database_id": db_id },
            "title": [{ "plain_text": "Articles" }],
            "properties": {
                "Name": { "id": "title", "type": "title", "title": {} },
                "Link": { "id": "lnk", "type": "url", "url": {} },
                "Source": { "id": "src", "type": "rich_text", "rich_text": {} }
            }
        })))
        .expect(2)
        .mount(&server)
        .await;

    let sender = NotionSender::with_base_url(server.uri(), None).expect("sender");

    // Step 1: Preview without patching (`patch_missing: false`).
    let preview = sender
        .inspect_schema(&NotionInspectSchemaRequest {
            secret: "ntn_inspect_secret".to_string(),
            data_source_or_db_id: format!(
                "https://www.notion.so/ws/My-Reading-Tracker-{}",
                db_id.replace('-', "")
            ),
            patch_missing: Some(false),
            use_proxy: false,
            channel_id: None,
        })
        .await
        .expect("inspect preview");

    assert_eq!(preview.data_source_id, ds_id);
    assert_eq!(preview.database_id.as_deref(), Some(db_id));
    assert_eq!(preview.title.as_deref(), Some("Articles"));
    assert!(
        !preview.ready,
        "must not be ready while FC-ID column is missing"
    );
    assert!(preview.patched_columns.is_empty());
    assert_eq!(
        preview.suggested_mapping.title, "Name",
        "must auto-bind existing 'Name' title column"
    );
    assert_eq!(preview.suggested_mapping.fc_id, "");
    assert_eq!(preview.suggested_mapping.link.as_deref(), Some("Link"));
    assert_eq!(
        preview.suggested_mapping.source, None,
        "mismatched 'Source' (rich_text) must be left unmapped rather than crashing"
    );

    let fc_id_check = preview
        .columns
        .iter()
        .find(|c| c.field == NotionStandardFieldKey::FcId)
        .expect("fc_id item");
    assert_eq!(fc_id_check.status, NotionColumnCheckStatus::Missing);
    assert!(fc_id_check.required);

    let source_check = preview
        .columns
        .iter()
        .find(|c| c.field == NotionStandardFieldKey::Source)
        .expect("source item");
    assert_eq!(source_check.status, NotionColumnCheckStatus::Mismatched);
    assert_eq!(source_check.actual_type.as_deref(), Some("rich_text"));

    // Step 2: Patch missing columns (`patch_missing: true`).
    // Verify PATCH adds ONLY missing columns (`FC-ID`, `Published`, `State`, `Tag`) and NEVER
    // overwrites `Name`/`Title` or the existing mismatched `Source`!
    Mock::given(method("PATCH"))
        .and(path(format!("/v1/data_sources/{ds_id}")))
        .and(header("Notion-Version", NOTION_API_VERSION))
        .respond_with(move |req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).expect("patch json");
            let props = body["properties"].as_object().expect("patch properties");
            assert_eq!(props.len(), 4);
            assert!(props.contains_key("FC-ID"));
            assert!(props.contains_key("Published"));
            assert!(props.contains_key("State"));
            assert!(props.contains_key("Tag"));
            assert!(
                !props.contains_key("Title"),
                "must never attempt to create a second title column"
            );
            assert!(
                !props.contains_key("Source"),
                "must never overwrite an existing mismatched user column"
            );

            ResponseTemplate::new(200).set_body_json(json!({
                "object": "data_source",
                "id": ds_id,
                "properties": {
                    "Name": { "type": "title", "title": {} },
                    "Link": { "type": "url", "url": {} },
                    "Source": { "type": "rich_text", "rich_text": {} },
                    "FC-ID": { "type": "rich_text", "rich_text": {} },
                    "Published": { "type": "date", "date": {} },
                    "State": { "type": "select", "select": {} },
                    "Tag": { "type": "multi_select", "multi_select": {} }
                }
            }))
        })
        .expect(1)
        .mount(&server)
        .await;

    let patched = sender
        .inspect_schema(&NotionInspectSchemaRequest {
            secret: "ntn_inspect_secret".to_string(),
            data_source_or_db_id: db_id.to_string(),
            patch_missing: Some(true),
            use_proxy: false,
            channel_id: None,
        })
        .await
        .expect("inspect + patch");

    assert!(patched.ready, "ready must be true once Name + FC-ID exist");
    assert_eq!(
        patched.patched_columns,
        vec!["FC-ID", "Published", "State", "Tag"]
    );
    assert_eq!(patched.suggested_mapping.title, "Name");
    assert_eq!(patched.suggested_mapping.fc_id, "FC-ID");
    assert_eq!(
        patched.suggested_mapping.published.as_deref(),
        Some("Published")
    );
    assert_eq!(patched.suggested_mapping.state.as_deref(), Some("State"));
    assert_eq!(patched.suggested_mapping.tag.as_deref(), Some("Tag"));
    assert_eq!(patched.suggested_mapping.source, None);
}

#[tokio::test]
async fn send_creates_minimalist_v2_page_sanitizes_commas_and_flushes_checkpoints() {
    let server = MockServer::start().await;
    let ds_id = "22222222-3333-4444-5555-666666666666";
    let fx = sample_fixtures(ds_id);
    let expected_fp = fx.entry.fingerprint_sha256.clone();

    Mock::given(method("POST"))
        .and(path("/v1/pages"))
        .and(header(
            "Authorization",
            "Bearer ntn_secret_test_token_123456",
        ))
        .and(header("Notion-Version", NOTION_API_VERSION))
        .and(header("Content-Type", "application/json"))
        .respond_with(move |req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).expect("page json");
            assert_eq!(body["parent"]["type"], "data_source_id");
            assert_eq!(body["parent"]["data_source_id"], ds_id);
            assert_eq!(
                body["icon"]["external"]["url"],
                "https://blog.example.com/favicon.ico"
            );
            assert!(body.get("cover").is_none(), "must never set a cover image");
            assert!(
                body.get("archived").is_none(),
                "must never send legacy archived"
            );
            assert_eq!(body["markdown_version"], "v2");
            assert!(
                body["markdown"].as_str().unwrap().contains("## Overview"),
                "markdown body should contain converted article content"
            );

            // Commas must be sanitized out of `select` and `multi_select` option names.
            assert_eq!(
                body["properties"]["Source"]["select"]["name"],
                "Rust Systems & Co."
            );
            let tags = body["properties"]["Tag"]["multi_select"]
                .as_array()
                .expect("tags array");
            assert_eq!(tags.len(), 2, "duplicate tags must be deduplicated");
            assert_eq!(tags[0]["name"], "rust");
            assert_eq!(tags[1]["name"], "distributed systems");

            assert_eq!(
                body["properties"]["FC-ID"]["rich_text"][0]["text"]["content"],
                expected_fp
            );
            assert_eq!(body["properties"]["State"]["select"]["name"], "UnRead");
            assert_eq!(
                body["properties"]["Link"]["url"],
                "https://blog.example.com/posts/notion-2026"
            );
            assert!(
                body["properties"]["Published"]["date"]["start"]
                    .as_str()
                    .unwrap()
                    .ends_with('Z')
            );

            ResponseTemplate::new(200).set_body_json(json!({
                "object": "page",
                "id": "page-created-001",
                "in_trash": false
            }))
        })
        .expect(1)
        .mount(&server)
        .await;

    let sender = NotionSender::with_base_url(server.uri(), None).expect("sender");
    let sink = RecordingCheckpointSink::default();
    let ctx = DeliveryContext {
        task: &fx.task,
        entry: &fx.entry,
        feed: &fx.feed,
        route: &fx.route,
        route_options: RouteOptions::default(),
        channel: &fx.channel,
        channel_config: &fx.channel_config,
        checkpoint: None,
        checkpoint_sink: Some(&sink),
    };

    let outcome = sender.send(&ctx).await.expect("send");
    match outcome {
        DeliveryOutcome::Delivered {
            status_code,
            checkpoint,
            ..
        } => {
            assert_eq!(status_code, Some(200));
            assert_eq!(
                checkpoint,
                Some(DeliveryCheckpoint::Notion {
                    create_initiated: true,
                    async_task_id: None,
                    page_id: Some("page-created-001".to_string()),
                })
            );
        }
        other => panic!("expected Delivered, got {other:?}"),
    }

    // Verify CheckpointSink recorded pre-create `create_initiated: true` BEFORE completion!
    let history = sink.history();
    assert_eq!(history.len(), 2);
    assert_eq!(
        history[0],
        DeliveryCheckpoint::Notion {
            create_initiated: true,
            async_task_id: None,
            page_id: None,
        }
    );
    assert_eq!(
        history[1],
        DeliveryCheckpoint::Notion {
            create_initiated: true,
            async_task_id: None,
            page_id: Some("page-created-001".to_string()),
        }
    );
}

#[tokio::test]
async fn crash_recovery_reconciles_fc_id_with_zero_duplicate_page_creates() {
    let server = MockServer::start().await;
    let ds_id = "33333333-4444-5555-6666-777777777777";
    let fx = sample_fixtures(ds_id);
    let expected_fp = fx.entry.fingerprint_sha256.clone();

    // `POST /v1/pages` MUST NOT be called (`.expect(0)`) because `FC-ID` reconciliation finds
    // the page that was committed right before the simulated process crash!
    Mock::given(method("POST"))
        .and(path("/v1/pages"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path(format!("/v1/data_sources/{ds_id}/query")))
        .and(header("Notion-Version", NOTION_API_VERSION))
        .respond_with(move |req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).expect("query json");
            assert_eq!(body["filter"]["property"], "FC-ID");
            assert_eq!(body["filter"]["rich_text"]["equals"], expected_fp);
            assert_eq!(body["page_size"], 1);

            ResponseTemplate::new(200).set_body_json(json!({
                "object": "list",
                "results": [
                    {
                        "object": "page",
                        "id": "reconciled-page-999",
                        "in_trash": false
                    }
                ]
            }))
        })
        .expect(1)
        .mount(&server)
        .await;

    let sender = NotionSender::with_base_url(server.uri(), None).expect("sender");
    let sink = RecordingCheckpointSink::default();
    let crashed_checkpoint = DeliveryCheckpoint::Notion {
        create_initiated: true,
        async_task_id: None,
        page_id: None,
    };

    let ctx = DeliveryContext {
        task: &fx.task,
        entry: &fx.entry,
        feed: &fx.feed,
        route: &fx.route,
        route_options: RouteOptions::default(),
        channel: &fx.channel,
        channel_config: &fx.channel_config,
        checkpoint: Some(crashed_checkpoint),
        checkpoint_sink: Some(&sink),
    };

    let outcome = sender.send(&ctx).await.expect("reconcile send");
    match outcome {
        DeliveryOutcome::Delivered { checkpoint, .. } => {
            assert_eq!(
                checkpoint,
                Some(DeliveryCheckpoint::Notion {
                    create_initiated: true,
                    async_task_id: None,
                    page_id: Some("reconciled-page-999".to_string()),
                })
            );
        }
        other => panic!("expected Delivered via FC-ID reconciliation, got {other:?}"),
    }
}

#[tokio::test]
async fn async_202_accepted_persists_task_handle_to_sqlite_and_resolves_on_poll() {
    let server = MockServer::start().await;
    let ds_id = "44444444-5555-6666-7777-888888888888";

    // Set up a real SQLite WAL database + `DbCheckpointSink` to verify mid-flight persistence!
    let tmp = TempDir::new().expect("tempdir");
    let app_cfg = AppConfig::with_data_dir(tmp.path());
    let key = MasterKey::from_config(&app_cfg).expect("master key");
    let db = Database::from_config(&app_cfg).await.expect("sqlite db");

    let feed = db
        .create_feed(&NewFeed {
            url: "https://async.example.com/rss".to_string(),
            title: "Async Longform".to_string(),
            site_url: None,
            favicon_url: None,
            tags: vec!["deep-dive".to_string()],
            poll_interval_secs: 1800,
            fetch_full_content: true,
            use_proxy: false,
            silent_init: false,
            next_poll_at: None,
        })
        .await
        .expect("create feed");

    // Generate > 250 paragraph blocks so `allow_async: true` is automatically enabled.
    let long_html: String = (0..280)
        .map(|i| format!("<p>Section {i} of the longform technical essay.</p>"))
        .collect();
    let entry = match db
        .insert_entry_if_new(&NewEntry {
            feed_id: feed.id,
            fingerprint_sha256: sha256_hex(b"entry-async-202"),
            published_at: Some(1_762_992_000),
            discovered_at: 1_762_992_010,
            title: Some("Massive Architecture Article".to_string()),
            canonical_url: Some("https://async.example.com/essay".to_string()),
            author: None,
            content_html: Some(long_html),
            first_sentence: None,
            summary_text: None,
            media: Vec::new(),
        })
        .await
        .expect("insert entry")
    {
        EntryInsertOutcome::Inserted(e) => *e,
        EntryInsertOutcome::Duplicate { .. } => unreachable!(),
    };

    let ch_cfg = ChannelConfig::Notion(NotionChannelConfig {
        integration_secret: "ntn_async_secret".to_string(),
        data_source_id: ds_id.to_string(),
        database_id: None,
        parent_page_id: None,
        column_mapping: NotionColumnMapping::standard(),
    });
    let encrypted = key
        .encrypt(&serde_json::to_vec(&ch_cfg).unwrap())
        .expect("encrypt");
    let channel = db
        .create_channel(&NewChannel {
            name: "Notion Async".to_string(),
            kind: ChannelKind::Notion,
            encrypted_config: encrypted,
            credential_group_key: ch_cfg.credential_group_key(),
            capabilities_json: "{}".to_string(),
            min_interval_secs: 1,
            jitter_max_secs: 0,
            hourly_quota: None,
            daily_quota: None,
            use_proxy: false,
            enabled: true,
        })
        .await
        .expect("create channel");

    let route = db
        .create_route(&NewRoute {
            name: "Longform Route".to_string(),
            feed_ids: vec![feed.id],
            match_tags: Vec::new(),
            channel_id: channel.id,
            include_keywords: Vec::new(),
            exclude_keywords: Vec::new(),
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions::default(),
            enabled: true,
        })
        .await
        .expect("create route");

    let task = db
        .enqueue_delivery_task(route.id, channel.id, entry.id, 1_762_992_010)
        .await
        .expect("enqueue")
        .expect("new task");

    Mock::given(method("POST"))
        .and(path("/v1/pages"))
        .respond_with(|req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).expect("json");
            assert_eq!(
                body["allow_async"], true,
                ">250 blocks must enable allow_async: true"
            );
            ResponseTemplate::new(202).set_body_json(json!({
                "object": "async_task",
                "async_task_id": "ntask_2026_handle_77"
            }))
        })
        .expect(1)
        .mount(&server)
        .await;

    // First 2 inline polls return `in_progress`, 3rd poll (on scheduler retry) returns `succeeded`!
    let poll_counter = Arc::new(Mutex::new(0usize));
    let counter_clone = Arc::clone(&poll_counter);
    Mock::given(method("GET"))
        .and(path("/v1/async_tasks/ntask_2026_handle_77"))
        .and(header("Notion-Version", NOTION_API_VERSION))
        .respond_with(move |_req: &Request| {
            let mut n = counter_clone.lock().unwrap();
            *n += 1;
            if *n <= 2 {
                ResponseTemplate::new(200).set_body_json(json!({
                    "object": "async_task",
                    "id": "ntask_2026_handle_77",
                    "status": "in_progress"
                }))
            } else {
                ResponseTemplate::new(200).set_body_json(json!({
                    "object": "async_task",
                    "id": "ntask_2026_handle_77",
                    "status": "succeeded",
                    "result": { "page_id": "async-page-final-42" }
                }))
            }
        })
        .expect(3)
        .mount(&server)
        .await;

    let sender = NotionSender::with_base_url(server.uri(), None)
        .expect("sender")
        .with_async_poll_policy(Duration::from_millis(5), 2, 2);

    let db_sink = db.checkpoint_sink(task.id);

    // Attempt 1: receives 202 Accepted, persists `async_task_id` to SQLite, polls twice (`in_progress`),
    // and yields `DeliveryOutcome::Retry` without cooling down the credential group.
    let ctx1 = DeliveryContext {
        task: &task,
        entry: &entry,
        feed: &feed,
        route: &route,
        route_options: RouteOptions::default(),
        channel: &channel,
        channel_config: &ch_cfg,
        checkpoint: None,
        checkpoint_sink: Some(&db_sink),
    };

    let outcome1 = sender.send(&ctx1).await.expect("attempt 1");
    let saved_in_db = db
        .get_delivery_task(task.id)
        .await
        .expect("read task")
        .expect("task exists")
        .checkpoint()
        .expect("parse checkpoint");

    let expected_mid_cp = DeliveryCheckpoint::Notion {
        create_initiated: true,
        async_task_id: Some("ntask_2026_handle_77".to_string()),
        page_id: None,
    };
    assert_eq!(
        saved_in_db,
        Some(expected_mid_cp.clone()),
        "SQLite must hold async_task_id mid-flight"
    );

    match outcome1 {
        DeliveryOutcome::Retry {
            status_code,
            cooldown_credential_group,
            checkpoint,
            ..
        } => {
            assert_eq!(status_code, Some(202));
            assert!(!cooldown_credential_group);
            assert_eq!(checkpoint, Some(expected_mid_cp.clone()));
        }
        other => panic!("expected Retry while async task is running, got {other:?}"),
    }

    // Attempt 2: resumes from persisted `async_task_id` checkpoint, polls `/v1/async_tasks/...`
    // (0 new `POST /v1/pages`!), observes `succeeded`, and returns `Delivered`!
    let refreshed_task = db
        .get_delivery_task(task.id)
        .await
        .expect("read task")
        .expect("task exists");
    let ctx2 = DeliveryContext {
        task: &refreshed_task,
        entry: &entry,
        feed: &feed,
        route: &route,
        route_options: RouteOptions::default(),
        channel: &channel,
        channel_config: &ch_cfg,
        checkpoint: Some(expected_mid_cp),
        checkpoint_sink: Some(&db_sink),
    };

    let outcome2 = sender.send(&ctx2).await.expect("attempt 2");
    match outcome2 {
        DeliveryOutcome::Delivered { checkpoint, .. } => {
            assert_eq!(
                checkpoint,
                Some(DeliveryCheckpoint::Notion {
                    create_initiated: true,
                    async_task_id: None,
                    page_id: Some("async-page-final-42".to_string()),
                })
            );
        }
        other => panic!("expected Delivered once async task succeeded, got {other:?}"),
    }

    db.close().await;
}

#[tokio::test]
async fn rate_limit_429_triggers_credential_group_cooldown_and_send_test_verifies_schema() {
    let server = MockServer::start().await;
    let ds_id = "55555555-6666-7777-8888-999999999999";
    let fx = sample_fixtures(ds_id);

    Mock::given(method("POST"))
        .and(path("/v1/pages"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "17")
                .set_body_json(json!({
                    "object": "error",
                    "status": 429,
                    "code": "rate_limited",
                    "message": "Rate limit exceeded."
                })),
        )
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!("/v1/data_sources/{ds_id}")))
        .and(header("Notion-Version", NOTION_API_VERSION))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "data_source",
            "id": ds_id,
            "title": [{ "plain_text": "FeedCourier Inbox" }],
            "properties": {
                "Title": { "type": "title" },
                "FC-ID": { "type": "rich_text" },
                "Link": { "type": "url" },
                "Source": { "type": "select" },
                "Published": { "type": "date" },
                "State": { "type": "select" },
                "Tag": { "type": "multi_select" }
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let sender = NotionSender::with_base_url(server.uri(), None).expect("sender");
    let ctx = DeliveryContext {
        task: &fx.task,
        entry: &fx.entry,
        feed: &fx.feed,
        route: &fx.route,
        route_options: RouteOptions::default(),
        channel: &fx.channel,
        channel_config: &fx.channel_config,
        checkpoint: None,
        checkpoint_sink: None,
    };

    let outcome = sender.send(&ctx).await.expect("send 429");
    match outcome {
        DeliveryOutcome::Retry {
            retry_after_secs,
            status_code,
            cooldown_credential_group,
            checkpoint,
            ..
        } => {
            assert_eq!(retry_after_secs, Some(17));
            assert_eq!(status_code, Some(429));
            assert!(
                cooldown_credential_group,
                "429 Retry-After must activate shared credential_group_key cooldown"
            );
            assert_eq!(
                checkpoint,
                Some(DeliveryCheckpoint::Notion {
                    create_initiated: true,
                    async_task_id: None,
                    page_id: None,
                })
            );
        }
        other => panic!("expected Retry on 429, got {other:?}"),
    }

    // Verify non-destructive `send_test` against the same Notion data source.
    let test_msg = sender
        .send_test(&fx.channel_config, false)
        .await
        .expect("send_test");
    assert!(test_msg.contains("FeedCourier Inbox"));
    assert!(test_msg.contains(NOTION_API_VERSION));
    assert!(test_msg.contains("FC-ID"));
}

#[tokio::test]
async fn in_trash_page_is_ignored_during_fc_id_reconcile_and_confirmed_page_id_short_circuits() {
    let server = MockServer::start().await;
    let ds_id = "66666666-7777-8888-9999-000000000000";
    let fx = sample_fixtures(ds_id);

    // 1. If checkpoint ALREADY holds `page_id: Some(...)`, `send` must short-circuit with 0 HTTP calls.
    let sender = NotionSender::with_base_url(server.uri(), None).expect("sender");
    let already_done_ctx = DeliveryContext {
        task: &fx.task,
        entry: &fx.entry,
        feed: &fx.feed,
        route: &fx.route,
        route_options: RouteOptions::default(),
        channel: &fx.channel,
        channel_config: &fx.channel_config,
        checkpoint: Some(DeliveryCheckpoint::Notion {
            create_initiated: true,
            async_task_id: None,
            page_id: Some("already-confirmed-page".to_string()),
        }),
        checkpoint_sink: None,
    };
    match sender.send(&already_done_ctx).await.expect("short-circuit") {
        DeliveryOutcome::Delivered { checkpoint, .. } => {
            assert_eq!(
                checkpoint,
                Some(DeliveryCheckpoint::Notion {
                    create_initiated: true,
                    async_task_id: None,
                    page_id: Some("already-confirmed-page".to_string()),
                })
            );
        }
        other => panic!("expected Delivered short-circuit, got {other:?}"),
    }

    // 2. If `FC-ID` query returns ONLY an `in_trash: true` page, `NotionSender` must ignore it
    //    and recreate a fresh live page via `POST /v1/pages`.
    Mock::given(method("POST"))
        .and(path(format!("/v1/data_sources/{ds_id}/query")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "results": [
                {
                    "object": "page",
                    "id": "trashed-page-old",
                    "in_trash": true
                }
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/pages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "page",
            "id": "fresh-live-page-new",
            "in_trash": false
        })))
        .expect(1)
        .mount(&server)
        .await;

    let trashed_ctx = DeliveryContext {
        task: &fx.task,
        entry: &fx.entry,
        feed: &fx.feed,
        route: &fx.route,
        route_options: RouteOptions::default(),
        channel: &fx.channel,
        channel_config: &fx.channel_config,
        checkpoint: Some(DeliveryCheckpoint::Notion {
            create_initiated: true,
            async_task_id: None,
            page_id: None,
        }),
        checkpoint_sink: None,
    };
    match sender
        .send(&trashed_ctx)
        .await
        .expect("recreate after trash")
    {
        DeliveryOutcome::Delivered { checkpoint, .. } => {
            assert_eq!(
                checkpoint,
                Some(DeliveryCheckpoint::Notion {
                    create_initiated: true,
                    async_task_id: None,
                    page_id: Some("fresh-live-page-new".to_string()),
                })
            );
        }
        other => panic!("expected Delivered fresh live page, got {other:?}"),
    }
}

#[tokio::test]
async fn send_classifies_5xx_as_retry_preserving_create_initiated_and_4xx_as_permanent_failure() {
    let server_503 = MockServer::start().await;
    let ds_id = "77777777-8888-9999-aaaa-bbbbbbbbbbbb";
    let fx = sample_fixtures(ds_id);

    Mock::given(method("POST"))
        .and(path("/v1/pages"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "object": "error",
            "status": 503,
            "code": "service_unavailable",
            "message": "Notion is temporarily unavailable."
        })))
        .expect(1)
        .mount(&server_503)
        .await;

    let sender_503 = NotionSender::with_base_url(server_503.uri(), None).expect("sender");
    let ctx_503 = DeliveryContext {
        task: &fx.task,
        entry: &fx.entry,
        feed: &fx.feed,
        route: &fx.route,
        route_options: RouteOptions::default(),
        channel: &fx.channel,
        channel_config: &fx.channel_config,
        checkpoint: None,
        checkpoint_sink: None,
    };
    match sender_503.send(&ctx_503).await.expect("send 503") {
        DeliveryOutcome::Retry {
            status_code,
            cooldown_credential_group,
            checkpoint,
            ..
        } => {
            assert_eq!(status_code, Some(503));
            assert!(!cooldown_credential_group);
            assert_eq!(
                checkpoint,
                Some(DeliveryCheckpoint::Notion {
                    create_initiated: true,
                    async_task_id: None,
                    page_id: None,
                }),
                "5xx must preserve create_initiated: true so next attempt reconciles FC-ID first"
            );
        }
        other => panic!("expected Retry on 503, got {other:?}"),
    }

    let server_401 = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/pages"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "object": "error",
            "status": 401,
            "code": "unauthorized",
            "message": "API token is invalid."
        })))
        .expect(1)
        .mount(&server_401)
        .await;

    let sender_401 = NotionSender::with_base_url(server_401.uri(), None).expect("sender");
    let ctx_401 = DeliveryContext {
        task: &fx.task,
        entry: &fx.entry,
        feed: &fx.feed,
        route: &fx.route,
        route_options: RouteOptions::default(),
        channel: &fx.channel,
        channel_config: &fx.channel_config,
        checkpoint: None,
        checkpoint_sink: None,
    };
    match sender_401.send(&ctx_401).await.expect("send 401") {
        DeliveryOutcome::PermanentFailure {
            status_code,
            diagnostic,
            ..
        } => {
            assert_eq!(status_code, Some(401));
            assert!(diagnostic.contains("unauthorized"));
        }
        other => panic!("expected PermanentFailure on 401, got {other:?}"),
    }
}
