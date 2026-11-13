//! End-to-end integration tests for Wave 2b:
//! - Background Scheduler (`DeliveryDispatcher`, `ProtectedMediaFetcher`, lease recovery, quotas,
//!   shared `credential_group_key` cooldowns, pacing + jitter, Notion `202` async deferral, GC)
//! - Full `/api/v1/*` Axum REST server, `fc_session` HttpOnly cookie auth, and `rust-embed` SPA.

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE, COOKIE, HOST, SET_COOKIE};
use axum::http::{Method, Request, StatusCode};
use bytes::Bytes;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use feedcourier::api::{AppState, build_api_router};
use feedcourier::channels::notion::NotionSender;
use feedcourier::channels::{
    ChannelConfig, ChannelKind, DeliveryCheckpoint, NotionChannelConfig, NotionColumnMapping,
    RouteOptions, TelegramChannelConfig, TelegramDeliveryMode,
};
use feedcourier::config::AppConfig;
use feedcourier::crypto::{MasterKey, sha256_hex};
use feedcourier::db::Database;
use feedcourier::db::models::{
    DeliveryTaskStatus, MediaAttachment, MediaAttachmentKind, NewChannel, NewEntry, NewFeed,
    NewRoute, UpdateChannel, now_unix,
};
use feedcourier::net::ProtectedHttpClient;
use feedcourier::scheduler::{DeliveryDispatcher, ProtectedMediaFetcher};

struct TestHarness {
    _temp_dir: TempDir,
    config: AppConfig,
    db: Database,
    master_key: MasterKey,
    dispatcher: DeliveryDispatcher,
    router: Router,
}

impl TestHarness {
    async fn new() -> Self {
        Self::with_builder(|d| d).await
    }

    async fn with_builder<F>(customize: F) -> Self
    where
        F: FnOnce(DeliveryDispatcher) -> DeliveryDispatcher,
    {
        let temp_dir = TempDir::new().expect("tempdir must create");
        let mut config = AppConfig::with_data_dir(temp_dir.path());
        config.retention_hours = 24;

        let db = Database::from_config(&config)
            .await
            .expect("SQLite database must initialize");
        let master_key = MasterKey::from_config(&config).expect("master key must initialize");
        let http = ProtectedHttpClient::with_defaults();

        let dispatcher = customize(
            DeliveryDispatcher::new(config.clone(), db.clone(), master_key.clone(), http)
                .expect("DeliveryDispatcher must build"),
        );
        let state = AppState::new(dispatcher.clone());
        let router = build_api_router(state);

        Self {
            _temp_dir: temp_dir,
            config,
            db,
            master_key,
            dispatcher,
            router,
        }
    }

    async fn send(
        &self,
        method: Method,
        uri: &str,
        cookie: Option<&str>,
        json_body: Option<Value>,
    ) -> (StatusCode, axum::http::HeaderMap, Bytes) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(HOST, "localhost:8080");

        if let Some(c) = cookie {
            builder = builder.header(COOKIE, c);
        }

        let body = if let Some(payload) = json_body {
            builder = builder.header(CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&payload).expect("JSON request body must serialize"))
        } else {
            Body::empty()
        };

        let req = builder.body(body).expect("HTTP request must build");
        let resp = self
            .router
            .clone()
            .oneshot(req)
            .await
            .expect("Axum oneshot must succeed");

        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .expect("response body must collect")
            .to_bytes();

        (status, headers, bytes)
    }

    async fn setup_admin_and_cookie(&self) -> String {
        let (status, headers, body) = self
            .send(
                Method::POST,
                "/api/v1/auth/setup",
                None,
                Some(json!({ "password": "correct-horse-battery" })),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "setup failed: {}",
            String::from_utf8_lossy(&body)
        );
        extract_session_cookie_pair(&headers)
    }
}

fn extract_session_cookie_pair(headers: &axum::http::HeaderMap) -> String {
    let raw = headers
        .get(SET_COOKIE)
        .expect("Set-Cookie header must be present")
        .to_str()
        .expect("Set-Cookie must be ASCII");
    raw.split(';')
        .next()
        .expect("cookie pair must exist")
        .trim()
        .to_string()
}

fn parse_json(bytes: &Bytes) -> Value {
    serde_json::from_slice(bytes).unwrap_or_else(|err| {
        panic!(
            "expected valid JSON response ({err}), got: {}",
            String::from_utf8_lossy(bytes)
        )
    })
}

#[tokio::test]
async fn auth_lifecycle_and_session_guard() {
    let h = TestHarness::new().await;

    // 1. Public health check works without authentication.
    let (status, _, body) = h.send(Method::GET, "/api/v1/health", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let health = parse_json(&body);
    assert_eq!(health["status"], "ok");
    assert_eq!(health["license"], "AGPL-3.0-only");

    // 2. Fresh instance reports `setup_required: true, authenticated: false`.
    let (status, _, body) = h.send(Method::GET, "/api/v1/auth/status", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let auth_status = parse_json(&body);
    assert_eq!(auth_status["setup_required"], true);
    assert_eq!(auth_status["authenticated"], false);

    // 3. Protected endpoint rejects unauthenticated requests with 401 JSON.
    let (status, _, body) = h.send(Method::GET, "/api/v1/overview", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let err_json = parse_json(&body);
    assert_eq!(err_json["error"], "Authentication required");

    // 4. Short password (< 8 chars) is rejected with 400.
    let (status, _, _) = h
        .send(
            Method::POST,
            "/api/v1/auth/setup",
            None,
            Some(json!({ "password": "short" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 5. Initial setup hashes password via Argon2id and issues HttpOnly `fc_session` cookie.
    let (status, headers, body) = h
        .send(
            Method::POST,
            "/api/v1/auth/setup",
            None,
            Some(json!({ "password": "super-secret-admin-pw" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse_json(&body)["ok"], true);

    let set_cookie_raw = headers
        .get(SET_COOKIE)
        .expect("Set-Cookie missing")
        .to_str()
        .unwrap();
    assert!(set_cookie_raw.contains("fc_session="));
    assert!(set_cookie_raw.contains("HttpOnly"));
    assert!(set_cookie_raw.contains("SameSite=Lax"));
    assert!(set_cookie_raw.contains("Max-Age=2592000"));
    let session_cookie = extract_session_cookie_pair(&headers);

    // 6. Calling `/auth/setup` a second time is rejected with 409 Conflict.
    let (status, _, _) = h
        .send(
            Method::POST,
            "/api/v1/auth/setup",
            None,
            Some(json!({ "password": "another-password-123" })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // 7. Authenticated `/overview` and `/auth/status` now succeed.
    let (status, _, body) = h
        .send(
            Method::GET,
            "/api/v1/auth/status",
            Some(&session_cookie),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let auth_status = parse_json(&body);
    assert_eq!(auth_status["setup_required"], false);
    assert_eq!(auth_status["authenticated"], true);

    let (status, _, body) = h
        .send(Method::GET, "/api/v1/overview", Some(&session_cookie), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let overview = parse_json(&body);
    assert_eq!(overview["feeds"]["active"], 0);
    assert_eq!(overview["tasks"]["queued"], 0);

    // 8. Logout invalidates the DB session and sets `Max-Age=0`.
    let (status, logout_headers, _) = h
        .send(
            Method::POST,
            "/api/v1/auth/logout",
            Some(&session_cookie),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let logout_cookie = logout_headers.get(SET_COOKIE).unwrap().to_str().unwrap();
    assert!(logout_cookie.contains("Max-Age=0"));

    // Reusing the logged-out session token returns 401.
    let (status, _, _) = h
        .send(Method::GET, "/api/v1/overview", Some(&session_cookie), None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 9. Login rejects bad password with 401, then issues fresh session on valid password.
    let (status, _, _) = h
        .send(
            Method::POST,
            "/api/v1/auth/login",
            None,
            Some(json!({ "password": "wrong-password-999" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, login_headers, _) = h
        .send(
            Method::POST,
            "/api/v1/auth/login",
            None,
            Some(json!({ "password": "super-secret-admin-pw" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let new_cookie = extract_session_cookie_pair(&login_headers);

    let (status, _, _) = h
        .send(Method::GET, "/api/v1/overview", Some(&new_cookie), None)
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn static_spa_router_and_api_404_separation() {
    let h = TestHarness::new().await;

    // 1. Root `/` returns embedded `web/dist/index.html`.
    let (status, headers, body) = h.send(Method::GET, "/", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let ct = headers.get(CONTENT_TYPE).unwrap().to_str().unwrap();
    assert!(ct.starts_with("text/html"));
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("<div id=\"root\">"));

    // 2. SPA client-side deep link `/routes` falls back to `index.html`.
    let (status, headers, body) = h.send(Method::GET, "/routes", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let ct = headers.get(CONTENT_TYPE).unwrap().to_str().unwrap();
    assert!(ct.starts_with("text/html"));
    assert_eq!(String::from_utf8_lossy(&body), html);

    // 3. Exact static asset `/favicon.svg` is served with SVG MIME type.
    let (status, headers, _) = h.send(Method::GET, "/favicon.svg", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let svg_ct = headers.get(CONTENT_TYPE).unwrap().to_str().unwrap();
    assert!(svg_ct.contains("image/svg+xml"));

    // 4. Missing hashed asset under `/assets/*` returns 404 (not HTML).
    let (status, _, _) = h
        .send(Method::GET, "/assets/does-not-exist-999.js", None, None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 5. Unknown `/api/v1/unknown` returns JSON 404 (even when authenticated).
    let cookie = h.setup_admin_and_cookie().await;
    let (status, headers, body) = h
        .send(Method::GET, "/api/v1/unknown", Some(&cookie), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        headers
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("application/json")
    );
    assert_eq!(parse_json(&body)["code"], "not_found");
}

#[tokio::test]
async fn end_to_end_settings_hot_reload_telegram_pipeline_and_secret_masking() {
    let mock_server = MockServer::start().await;
    let h = TestHarness::new().await;
    let cookie = h.setup_admin_and_cookie().await;

    let rss_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
        <rss version="2.0">
          <channel>
            <title>Rust Release Blog</title>
            <link>{base}</link>
            <description>Official Rust announcements</description>
            <item>
              <guid>rust-1-90</guid>
              <title>Announcing Rust 1.90</title>
              <link>{base}/posts/rust-1-90</link>
              <pubDate>Fri, 13 Nov 2026 10:00:00 GMT</pubDate>
              <description><![CDATA[<p>Rust 1.90 brings faster async traits.</p><img src="{base}/media/banner.png"/>]]></description>
            </item>
            <item>
              <guid>rust-1-89</guid>
              <title>Announcing Rust 1.89</title>
              <link>{base}/posts/rust-1-89</link>
              <pubDate>Thu, 12 Nov 2026 10:00:00 GMT</pubDate>
              <description><![CDATA[<p>Previous Rust release notes.</p>]]></description>
            </item>
          </channel>
        </rss>"#,
        base = mock_server.uri()
    );

    Mock::given(method("GET"))
        .and(path("/rss.xml"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/rss+xml")
                .set_body_string(rss_xml),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/media/banner.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4]),
        )
        .mount(&mock_server)
        .await;

    // Mock Telegram Bot API (`getMe` + `sendRichMessage`).
    Mock::given(method("GET"))
        .and(path("/bot123456789:ABCDEF_super_secret_token_WXYZ/getMe"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": {
                "id": 123456789_i64,
                "is_bot": true,
                "first_name": "FeedCourierBot",
                "username": "feedcourier_bot"
            }
        })))
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path(
            "/bot123456789:ABCDEF_super_secret_token_WXYZ/sendRichMessage",
        ))
        .respond_with(|req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            if body.get("chat_id").is_none() {
                // Capability probe (`{}`) -> 400 Bad Request ("chat_id is empty") proves method exists!
                ResponseTemplate::new(400).set_body_json(json!({
                    "ok": false,
                    "error_code": 400,
                    "description": "Bad Request: chat_id is empty"
                }))
            } else {
                assert_eq!(body["chat_id"], "-100999888");
                ResponseTemplate::new(200).set_body_json(json!({
                    "ok": true,
                    "result": { "message_id": 777 }
                }))
            }
        })
        .mount(&mock_server)
        .await;

    // 1. Before SSRF allowlisting `127.0.0.1/32`, feed preview to loopback is blocked (`403`).
    let feed_url = format!("{}/rss.xml", mock_server.uri());
    let (status, _, body) = h
        .send(
            Method::POST,
            "/api/v1/feeds/preview",
            Some(&cookie),
            Some(json!({
                "url": feed_url,
                "fetch_full_content": false,
                "use_proxy": false
            })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(parse_json(&body)["code"], "ssrf_blocked");

    // 2. Invalid CIDR in `PUT /api/v1/settings` returns 400 without mutating runtime settings.
    let (status, _, _) = h
        .send(
            Method::PUT,
            "/api/v1/settings",
            Some(&cookie),
            Some(json!({
                "outbound_proxy_url": null,
                "ssrf_allowlist": ["999.999.999.999/99"]
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 3. Valid `PUT /api/v1/settings` allowlisting loopback hot-reloads `ProtectedHttpClient` & `ProtectedMediaFetcher`.
    let (status, _, body) = h
        .send(
            Method::PUT,
            "/api/v1/settings",
            Some(&cookie),
            Some(json!({
                "outbound_proxy_url": null,
                "ssrf_allowlist": ["127.0.0.1/32", "::1/128"]
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        parse_json(&body)["ssrf_allowlist"],
        json!(["127.0.0.1/32", "::1/128"])
    );

    // Verify `ProtectedMediaFetcher` also shares the hot-reloaded SSRF allowlist immediately.
    use feedcourier::channels::telegram::MediaStreamFetcher as _;
    let media_fetcher: std::sync::Arc<ProtectedMediaFetcher> = h.dispatcher.media_fetcher();
    let fetched_media = media_fetcher
        .fetch_media(
            &format!("{}/media/banner.png", mock_server.uri()),
            1024 * 1024,
            false,
        )
        .await
        .expect("ProtectedMediaFetcher must honor hot-reloaded SSRF allowlist");
    assert_eq!(fetched_media.content_type.as_deref(), Some("image/png"));
    assert_eq!(fetched_media.filename, "banner.png");

    // Live feed preview now succeeds!
    let (status, _, body) = h
        .send(
            Method::POST,
            "/api/v1/feeds/preview",
            Some(&cookie),
            Some(json!({
                "url": feed_url,
                "fetch_full_content": false,
                "use_proxy": false
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let preview = parse_json(&body);
    assert_eq!(preview["feed_title"], "Rust Release Blog");
    assert_eq!(preview["items"].as_array().unwrap().len(), 2);

    // 4. Create Telegram channel, verify secret masking & capability probing, then PATCH `chat_id` omitting `bot_token`.
    let (status, _, body) = h
        .send(
            Method::POST,
            "/api/v1/channels",
            Some(&cookie),
            Some(json!({
                "name": "TG Releases",
                "kind": "telegram",
                "config": {
                    "kind": "telegram",
                    "bot_token": "123456789:ABCDEF_super_secret_token_WXYZ",
                    "chat_id": "-100111222",
                    "api_base_url": mock_server.uri()
                },
                "min_interval_secs": 0,
                "jitter_max_secs": 0,
                "hourly_quota": null,
                "daily_quota": null,
                "use_proxy": false,
                "enabled": true
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let created_ch = parse_json(&body);
    let channel_id = created_ch["id"].as_i64().unwrap();
    assert_eq!(created_ch["capabilities"]["supports_rich_message"], true);

    let (status, _, list_body) = h
        .send(Method::GET, "/api/v1/channels", Some(&cookie), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let raw_channels_str = String::from_utf8_lossy(&list_body);
    assert!(
        !raw_channels_str.contains("ABCDEF_super_secret_token"),
        "GET /api/v1/channels must NEVER leak raw bot_token"
    );
    let channels_json = parse_json(&list_body);
    assert_eq!(channels_json[0]["config"]["bot_token_hint"], "1234••••WXYZ");
    assert_eq!(channels_json[0]["config"]["chat_id"], "-100111222");

    // PATCH `chat_id` without supplying `bot_token` -> preserves encrypted `bot_token`!
    let (status, _, patch_body) = h
        .send(
            Method::PATCH,
            &format!("/api/v1/channels/{channel_id}"),
            Some(&cookie),
            Some(json!({
                "config": {
                    "kind": "telegram",
                    "chat_id": "-100999888"
                }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let patched_ch = parse_json(&patch_body);
    assert_eq!(patched_ch["config"]["chat_id"], "-100999888");
    assert_eq!(patched_ch["config"]["bot_token_hint"], "1234••••WXYZ");

    // 5. Invalid regex on `POST /api/v1/routes` returns 400; valid route returns 201.
    let (status, _, _) = h
        .send(
            Method::POST,
            "/api/v1/routes",
            Some(&cookie),
            Some(json!({
                "name": "Bad Regex Route",
                "feed_ids": [],
                "match_tags": ["rust"],
                "channel_id": channel_id,
                "include_keywords": [],
                "exclude_keywords": [],
                "include_regex": "[unclosed-group",
                "exclude_regex": null,
                "options": { "telegram_mode": "rich_message", "link_preview": true },
                "enabled": true
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, route_body) = h
        .send(
            Method::POST,
            "/api/v1/routes",
            Some(&cookie),
            Some(json!({
                "name": "Rust -> Telegram",
                "feed_ids": [],
                "match_tags": ["rust"],
                "channel_id": channel_id,
                "include_keywords": ["Rust"],
                "exclude_keywords": [],
                "include_regex": null,
                "exclude_regex": null,
                "options": { "telegram_mode": "rich_message", "link_preview": true },
                "enabled": true
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let route_id = parse_json(&route_body)["id"].as_i64().unwrap();

    // 6. `POST /api/v1/feeds` with `silent_init: true, push_latest_one: true` fingerprints both
    // items and enqueues exactly 1 task for the newest entry.
    let (status, _, feed_body) = h
        .send(
            Method::POST,
            "/api/v1/feeds",
            Some(&cookie),
            Some(json!({
                "url": feed_url,
                "tags": ["rust"],
                "poll_interval_secs": 900,
                "fetch_full_content": false,
                "use_proxy": false,
                "silent_init": true,
                "push_latest_one": true
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let created_feed = parse_json(&feed_body);
    let feed_id = created_feed["id"].as_i64().unwrap();
    assert_eq!(created_feed["title"], "Rust Release Blog");
    // `silent_init` automatically clears to `false` once the initial poll succeeds.
    assert_eq!(created_feed["silent_init"], false);

    let snap_before = h.db.get_overview_snapshot().await.unwrap();
    assert_eq!(snap_before.entries_total_fingerprints, 2);
    assert_eq!(snap_before.entries_active_payloads, 1);
    assert_eq!(snap_before.tasks_queued, 1);

    // 7. Run outbox worker tick -> delivers to mock Telegram Bot API (`-100999888`) using preserved secret!
    let processed = h.dispatcher.process_next_outbox_task().await.unwrap();
    assert!(processed);
    assert!(!h.dispatcher.process_next_outbox_task().await.unwrap());

    let (status, _, tasks_body) = h
        .send(
            Method::GET,
            "/api/v1/tasks?status=delivered&limit=10",
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let tasks = parse_json(&tasks_body);
    let tasks_arr = tasks.as_array().unwrap();
    assert_eq!(tasks_arr.len(), 1);
    assert_eq!(tasks_arr[0]["status"], "delivered");
    assert_eq!(tasks_arr[0]["route_id"], route_id);
    assert_eq!(tasks_arr[0]["feed_id"], feed_id);
    assert_eq!(tasks_arr[0]["feed_title"], "Rust Release Blog");
    assert_eq!(tasks_arr[0]["entry_title"], "Announcing Rust 1.90");
}

#[tokio::test]
async fn outbox_expired_lease_recovery_and_notion_async_deferral() {
    let mock_notion = MockServer::start().await;
    let base_uri = mock_notion.uri();

    let h = TestHarness::with_builder(|d| {
        let notion = NotionSender::with_base_url(&base_uri, None)
            .expect("custom NotionSender must build")
            .with_async_poll_policy(Duration::from_millis(5), 0, 1);
        d.with_notion_sender(notion)
    })
    .await;

    let ds_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let cfg = ChannelConfig::Notion(NotionChannelConfig {
        integration_secret: "ntn_secret_reconcile_123456".to_string(),
        data_source_id: ds_id.to_string(),
        database_id: None,
        parent_page_id: None,
        column_mapping: NotionColumnMapping::standard(),
    });
    let channel =
        h.db.create_channel(&NewChannel {
            name: "Notion Inbox".to_string(),
            kind: ChannelKind::Notion,
            encrypted_config: h.master_key.encrypt_json(&cfg).unwrap(),
            credential_group_key: cfg.credential_group_key(),
            capabilities_json: "{}".to_string(),
            min_interval_secs: 0,
            jitter_max_secs: 0,
            hourly_quota: None,
            daily_quota: None,
            use_proxy: false,
            enabled: true,
        })
        .await
        .unwrap();

    let feed =
        h.db.create_feed(&NewFeed::new("https://example.com/feed.xml"))
            .await
            .unwrap();
    let route =
        h.db.create_route(&NewRoute {
            name: "All -> Notion".to_string(),
            feed_ids: vec![feed.id],
            match_tags: vec![],
            channel_id: channel.id,
            include_keywords: vec![],
            exclude_keywords: vec![],
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions::default(),
            enabled: true,
        })
        .await
        .unwrap();

    let fp1 = sha256_hex(b"orphaned-crash-entry");
    let entry1 = match h
        .db
        .insert_entry_if_new(&NewEntry {
            feed_id: feed.id,
            fingerprint_sha256: fp1.clone(),
            published_at: Some(now_unix()),
            discovered_at: now_unix(),
            title: Some("Crash Recovery Article".to_string()),
            canonical_url: Some("https://example.com/crash-article".to_string()),
            author: Some("Alice".to_string()),
            content_html: Some("<p>Recovered body</p>".to_string()),
            first_sentence: Some("Recovered body".to_string()),
            summary_text: Some("Recovered body".to_string()),
            media: vec![],
        })
        .await
        .unwrap()
    {
        feedcourier::db::models::EntryInsertOutcome::Inserted(e) => *e,
        _ => panic!("expected fresh entry"),
    };

    let task1 =
        h.db.enqueue_delivery_task(route.id, channel.id, entry1.id, now_unix() - 120)
            .await
            .unwrap()
            .unwrap();

    // Simulate a worker that crashed mid-flight after persisting `create_initiated: true`
    // and whose `delivering` lease expired 60 seconds ago (`lease_until < now`).
    let cp = DeliveryCheckpoint::Notion {
        create_initiated: true,
        async_task_id: None,
        page_id: None,
    };
    sqlx::query(
        "UPDATE delivery_tasks SET status = 'delivering', lease_until = ?1, attempt_count = 1, checkpoint_json = ?2 WHERE id = ?3",
    )
    .bind(now_unix() - 60)
    .bind(serde_json::to_string(&cp).unwrap())
    .bind(task1.id)
    .execute(h.db.write_pool())
    .await
    .unwrap();

    // Mock `POST /v1/data_sources/{ds_id}/query` finding the page by `FC-ID`; `POST /v1/pages` must NOT be called!
    Mock::given(method("POST"))
        .and(path(format!("/v1/data_sources/{ds_id}/query")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "results": [
                { "id": "page-reconciled-999", "in_trash": false }
            ]
        })))
        .expect(1)
        .mount(&mock_notion)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/pages"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&mock_notion)
        .await;

    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());

    let updated_task1 = h.db.get_delivery_task(task1.id).await.unwrap().unwrap();
    assert_eq!(updated_task1.status, DeliveryTaskStatus::Delivered);
    assert_eq!(updated_task1.lease_until, None);
    assert_eq!(
        updated_task1.checkpoint().unwrap(),
        Some(DeliveryCheckpoint::Notion {
            create_initiated: true,
            async_task_id: None,
            page_id: Some("page-reconciled-999".to_string()),
        })
    );

    // Part B: Verify Notion `202 Accepted` async polling defers (`status = deferred`) without
    // burning `attempt_count`!
    mock_notion.reset().await;

    let fp2 = sha256_hex(b"async-202-entry");
    let entry2 = match h
        .db
        .insert_entry_if_new(&NewEntry {
            feed_id: feed.id,
            fingerprint_sha256: fp2,
            published_at: Some(now_unix()),
            discovered_at: now_unix(),
            title: Some("Long Async Article".to_string()),
            canonical_url: Some("https://example.com/async-article".to_string()),
            author: None,
            content_html: Some(format!("<p>{}</p>", "word ".repeat(6000))),
            first_sentence: None,
            summary_text: None,
            media: vec![],
        })
        .await
        .unwrap()
    {
        feedcourier::db::models::EntryInsertOutcome::Inserted(e) => *e,
        _ => panic!("expected fresh entry"),
    };

    let task2 =
        h.db.enqueue_delivery_task(route.id, channel.id, entry2.id, now_unix())
            .await
            .unwrap()
            .unwrap();

    Mock::given(method("POST"))
        .and(path("/v1/pages"))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({
            "async_task_id": "async-job-42"
        })))
        .expect(1)
        .mount(&mock_notion)
        .await;

    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    let deferred_task2 = h.db.get_delivery_task(task2.id).await.unwrap().unwrap();
    assert_eq!(deferred_task2.status, DeliveryTaskStatus::Deferred);
    assert_eq!(
        deferred_task2.attempt_count, 0,
        "202 Accepted async poll must not consume retry budget"
    );
    assert_eq!(
        deferred_task2.checkpoint().unwrap(),
        Some(DeliveryCheckpoint::Notion {
            create_initiated: true,
            async_task_id: Some("async-job-42".to_string()),
            page_id: None,
        })
    );
}

#[tokio::test]
async fn hourly_quota_and_pacing_defer_without_burning_attempt_count() {
    let mock_tg = MockServer::start().await;
    let h = TestHarness::new().await;

    Mock::given(method("POST"))
        .and(path_regex(r"^/bot.*/sendMessage$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": { "message_id": 101 }
        })))
        .mount(&mock_tg)
        .await;

    let tg_cfg = ChannelConfig::Telegram(TelegramChannelConfig {
        bot_token: "999888777:quota_test_token_123456".to_string(),
        chat_id: "-100555".to_string(),
        message_thread_id: None,
        api_base_url: Some(mock_tg.uri()),
    });

    // `hourly_quota = 1`, zero pacing so the 2nd task specifically hits `hourly_quota`.
    let channel =
        h.db.create_channel(&NewChannel {
            name: "Quota TG".to_string(),
            kind: ChannelKind::Telegram,
            encrypted_config: h.master_key.encrypt_json(&tg_cfg).unwrap(),
            credential_group_key: tg_cfg.credential_group_key(),
            capabilities_json: "{}".to_string(),
            min_interval_secs: 0,
            jitter_max_secs: 0,
            hourly_quota: Some(1),
            daily_quota: None,
            use_proxy: false,
            enabled: true,
        })
        .await
        .unwrap();

    let feed =
        h.db.create_feed(&NewFeed::new("https://example.com/quota.xml"))
            .await
            .unwrap();
    let route =
        h.db.create_route(&NewRoute {
            name: "Compact Quota Route".to_string(),
            feed_ids: vec![feed.id],
            match_tags: vec![],
            channel_id: channel.id,
            include_keywords: vec![],
            exclude_keywords: vec![],
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions {
                telegram_mode: TelegramDeliveryMode::CompactCard,
                link_preview: true,
            },
            enabled: true,
        })
        .await
        .unwrap();

    let mut task_ids = Vec::new();
    for idx in 1..=2 {
        let fp = sha256_hex(format!("quota-entry-{idx}").as_bytes());
        let entry = match h
            .db
            .insert_entry_if_new(&NewEntry {
                feed_id: feed.id,
                fingerprint_sha256: fp,
                published_at: Some(now_unix()),
                discovered_at: now_unix(),
                title: Some(format!("Quota Item #{idx}")),
                canonical_url: Some(format!("https://example.com/item-{idx}")),
                author: None,
                content_html: Some("<p>Hello quota</p>".to_string()),
                first_sentence: Some("Hello quota".to_string()),
                summary_text: Some("Hello quota".to_string()),
                media: vec![MediaAttachment {
                    url: "https://example.com/a.jpg".to_string(),
                    mime_type: Some("image/jpeg".to_string()),
                    kind: MediaAttachmentKind::Image,
                    length_bytes: None,
                    caption: None,
                }],
            })
            .await
            .unwrap()
        {
            feedcourier::db::models::EntryInsertOutcome::Inserted(e) => *e,
            _ => unreachable!(),
        };
        let t =
            h.db.enqueue_delivery_task(route.id, channel.id, entry.id, now_unix())
                .await
                .unwrap()
                .unwrap();
        task_ids.push(t.id);
    }

    // 1st task delivers cleanly.
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    let first = h.db.get_delivery_task(task_ids[0]).await.unwrap().unwrap();
    assert_eq!(first.status, DeliveryTaskStatus::Delivered);
    assert_eq!(first.attempt_count, 1);

    // 2nd task hits `hourly_quota = 1` -> transitions to `deferred` with `attempt_count == 0`!
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    let second = h.db.get_delivery_task(task_ids[1]).await.unwrap().unwrap();
    assert_eq!(second.status, DeliveryTaskStatus::Deferred);
    assert_eq!(
        second.attempt_count, 0,
        "hourly quota deferral must NOT increment attempt_count"
    );
    assert!(
        second
            .diagnostic_log
            .as_deref()
            .unwrap_or("")
            .contains("hourly quota"),
        "unexpected diagnostic: {:?}",
        second.diagnostic_log
    );

    // Raise hourly_quota to 10 and enable `min_interval_secs = 60` + `jitter_max_secs = 10` to verify pacing deferral!
    h.db.update_channel(
        channel.id,
        &UpdateChannel {
            hourly_quota: Some(Some(10)),
            min_interval_secs: Some(60),
            jitter_max_secs: Some(10),
            ..UpdateChannel::default()
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE delivery_tasks SET next_attempt_at = ?1 WHERE id = ?2")
        .bind(now_unix() - 1)
        .bind(task_ids[1])
        .execute(h.db.write_pool())
        .await
        .unwrap();

    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    let paced = h.db.get_delivery_task(task_ids[1]).await.unwrap().unwrap();
    assert_eq!(paced.status, DeliveryTaskStatus::Deferred);
    assert_eq!(paced.attempt_count, 0);
    assert!(
        paced
            .diagnostic_log
            .as_deref()
            .unwrap_or("")
            .contains("pacing"),
        "expected pacing diagnostic, got: {:?}",
        paced.diagnostic_log
    );
}

#[tokio::test]
async fn downstream_429_shared_credential_group_cooldown_dead_letter_and_retry() {
    let mock_tg = MockServer::start().await;
    // Set `max_delivery_attempts = 2` so we can verify `retrying` -> `dead_letter` -> `POST /api/v1/tasks/retry`.
    let h = TestHarness::with_builder(|d| d.with_max_delivery_attempts(2)).await;
    let cookie = h.setup_admin_and_cookie().await;

    Mock::given(method("POST"))
        .and(path_regex(r"^/bot.*/sendMessage$"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "ok": false,
            "error_code": 429,
            "description": "Too Many Requests: retry after 45",
            "parameters": { "retry_after": 45 }
        })))
        .mount(&mock_tg)
        .await;

    // Two distinct channels sharing the exact same Telegram Bot token (`credential_group_key`).
    let cfg_a = ChannelConfig::Telegram(TelegramChannelConfig {
        bot_token: "555666777:shared_bot_token_for_group_lock".to_string(),
        chat_id: "-100111".to_string(),
        message_thread_id: None,
        api_base_url: Some(mock_tg.uri()),
    });
    let cfg_b = ChannelConfig::Telegram(TelegramChannelConfig {
        bot_token: "555666777:shared_bot_token_for_group_lock".to_string(),
        chat_id: "-100222".to_string(),
        message_thread_id: None,
        api_base_url: Some(mock_tg.uri()),
    });
    assert_eq!(cfg_a.credential_group_key(), cfg_b.credential_group_key());

    let ch_a =
        h.db.create_channel(&NewChannel {
            name: "TG Group A".to_string(),
            kind: ChannelKind::Telegram,
            encrypted_config: h.master_key.encrypt_json(&cfg_a).unwrap(),
            credential_group_key: cfg_a.credential_group_key(),
            capabilities_json: "{}".to_string(),
            min_interval_secs: 0,
            jitter_max_secs: 0,
            hourly_quota: None,
            daily_quota: None,
            use_proxy: false,
            enabled: true,
        })
        .await
        .unwrap();

    let ch_b =
        h.db.create_channel(&NewChannel {
            name: "TG Group B".to_string(),
            kind: ChannelKind::Telegram,
            encrypted_config: h.master_key.encrypt_json(&cfg_b).unwrap(),
            credential_group_key: cfg_b.credential_group_key(),
            capabilities_json: "{}".to_string(),
            min_interval_secs: 0,
            jitter_max_secs: 0,
            hourly_quota: None,
            daily_quota: None,
            use_proxy: false,
            enabled: true,
        })
        .await
        .unwrap();

    let feed =
        h.db.create_feed(&NewFeed::new("https://example.com/rate-limit.xml"))
            .await
            .unwrap();
    let route_a =
        h.db.create_route(&NewRoute {
            name: "Route A".to_string(),
            feed_ids: vec![feed.id],
            match_tags: vec![],
            channel_id: ch_a.id,
            include_keywords: vec![],
            exclude_keywords: vec![],
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions {
                telegram_mode: TelegramDeliveryMode::CompactCard,
                link_preview: false,
            },
            enabled: true,
        })
        .await
        .unwrap();
    let route_b =
        h.db.create_route(&NewRoute {
            name: "Route B".to_string(),
            feed_ids: vec![feed.id],
            match_tags: vec![],
            channel_id: ch_b.id,
            include_keywords: vec![],
            exclude_keywords: vec![],
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions {
                telegram_mode: TelegramDeliveryMode::CompactCard,
                link_preview: false,
            },
            enabled: true,
        })
        .await
        .unwrap();

    let entry = match h
        .db
        .insert_entry_if_new(&NewEntry {
            feed_id: feed.id,
            fingerprint_sha256: sha256_hex(b"429-entry"),
            published_at: Some(now_unix()),
            discovered_at: now_unix(),
            title: Some("Rate Limited Post".to_string()),
            canonical_url: Some("https://example.com/429".to_string()),
            author: None,
            content_html: Some("<p>429 test</p>".to_string()),
            first_sentence: Some("429 test".to_string()),
            summary_text: Some("429 test".to_string()),
            media: vec![],
        })
        .await
        .unwrap()
    {
        feedcourier::db::models::EntryInsertOutcome::Inserted(e) => *e,
        _ => unreachable!(),
    };

    let task_a =
        h.db.enqueue_delivery_task(route_a.id, ch_a.id, entry.id, now_unix())
            .await
            .unwrap()
            .unwrap();
    let _task_b =
        h.db.enqueue_delivery_task(route_b.id, ch_b.id, entry.id, now_unix())
            .await
            .unwrap()
            .unwrap();

    // Attempt 1 on `ch_a`: receives 429 `retry_after: 45` -> transitions `task_a` to `retrying` AND
    // places BOTH `ch_a` and sibling `ch_b` into shared `credential_group_key` cooldown!
    let before = now_unix();
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());

    let updated_a = h.db.get_delivery_task(task_a.id).await.unwrap().unwrap();
    assert_eq!(updated_a.status, DeliveryTaskStatus::Retrying);
    assert_eq!(updated_a.attempt_count, 1);
    assert_eq!(updated_a.last_status_code, Some(429));

    let refreshed_ch_a = h.db.get_channel(ch_a.id).await.unwrap().unwrap();
    let refreshed_ch_b = h.db.get_channel(ch_b.id).await.unwrap().unwrap();
    assert!(refreshed_ch_a.cooldown_until.unwrap() >= before + 45);
    assert_eq!(refreshed_ch_a.cooldown_until, refreshed_ch_b.cooldown_until);

    // Sibling `task_b` is NOT claimed while shared cooldown is active!
    assert!(!h.dispatcher.process_next_outbox_task().await.unwrap());

    // `GET /api/v1/overview` exposes both cooled-down channels in `active_cooldowns`.
    let (status, _, ov_body) = h
        .send(Method::GET, "/api/v1/overview", Some(&cookie), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let ov = parse_json(&ov_body);
    assert_eq!(ov["active_cooldowns"].as_array().unwrap().len(), 2);

    // Fast-forward cooldown & `task_a.next_attempt_at` to simulate 45s elapsing -> Attempt 2 hits
    // `max_delivery_attempts = 2` and transitions `task_a` to `dead_letter`!
    sqlx::query("UPDATE channels SET cooldown_until = NULL")
        .execute(h.db.write_pool())
        .await
        .unwrap();
    sqlx::query("UPDATE delivery_tasks SET next_attempt_at = ?1 WHERE id = ?2")
        .bind(now_unix() - 5)
        .bind(task_a.id)
        .execute(h.db.write_pool())
        .await
        .unwrap();

    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    let dead_a = h.db.get_delivery_task(task_a.id).await.unwrap().unwrap();
    assert_eq!(dead_a.status, DeliveryTaskStatus::DeadLetter);
    assert_eq!(dead_a.attempt_count, 2);

    // `POST /api/v1/tasks/retry` with `all_dead_letter: true` requeues `task_a` cleanly!
    let (status, _, retry_body) = h
        .send(
            Method::POST,
            "/api/v1/tasks/retry",
            Some(&cookie),
            Some(json!({ "all_dead_letter": true })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse_json(&retry_body)["requeued_count"], 1);

    let requeued_a = h.db.get_delivery_task(task_a.id).await.unwrap().unwrap();
    assert_eq!(requeued_a.status, DeliveryTaskStatus::Queued);
    assert_eq!(requeued_a.attempt_count, 0);
}

#[tokio::test]
async fn maintenance_backup_gc_opml_mcp_tokens_and_scheduler_shutdown() {
    let mock_notion = MockServer::start().await;
    let base_uri = mock_notion.uri();
    let h = TestHarness::with_builder(|d| {
        let notion = NotionSender::with_base_url(&base_uri, None).unwrap();
        d.with_notion_sender(notion).with_loop_intervals(
            Duration::from_secs(60),
            Duration::from_secs(60),
            Duration::from_secs(60),
        )
    })
    .await;
    let cookie = h.setup_admin_and_cookie().await;

    // 1. Spawn background scheduler loops and verify clean `CancellationToken` shutdown.
    let scheduler_handle = h.dispatcher.spawn();
    h.dispatcher.notify_poller();
    h.dispatcher.notify_outbox();
    h.dispatcher.notify_maintenance();
    tokio::time::sleep(Duration::from_millis(25)).await;
    scheduler_handle.shutdown().await;

    // 2. OPML import + export round-trip.
    let opml_input = r#"<?xml version="1.0" encoding="UTF-8"?>
    <opml version="2.0">
      <head><title>Subscriptions</title></head>
      <body>
        <outline text="Tech" title="Tech">
          <outline type="rss" text="Example Blog" title="Example Blog" xmlUrl="https://example.com/rss.xml" htmlUrl="https://example.com"/>
        </outline>
      </body>
    </opml>"#;

    let (status, _, import_body) = h
        .send(
            Method::POST,
            "/api/v1/feeds/opml/import",
            Some(&cookie),
            Some(json!({ "opml_xml": opml_input, "silent_init": true })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let imported = parse_json(&import_body);
    assert_eq!(imported["imported_count"], 1);
    assert_eq!(imported["feeds"][0]["tags"], json!(["Tech"]));
    let feed_id = imported["feeds"][0]["id"].as_i64().unwrap();

    let (status, headers, export_bytes) = h
        .send(
            Method::GET,
            "/api/v1/feeds/opml/export",
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("application/xml")
    );
    let exported_xml = String::from_utf8_lossy(&export_bytes);
    assert!(exported_xml.contains("https://example.com/rss.xml"));

    // 3. Notion `inspect-schema` reusing stored `channel_id` secret when `secret: ""`.
    let ds_id = "11111111-2222-3333-4444-555555555555";
    Mock::given(method("GET"))
        .and(path(format!("/v1/data_sources/{ds_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "data_source",
            "id": ds_id,
            "title": [{ "plain_text": "Reading List" }],
            "properties": {
                "Title": { "id": "title", "type": "title", "title": {} },
                "FC-ID": { "id": "fcid", "type": "rich_text", "rich_text": {} }
            }
        })))
        .expect(1)
        .mount(&mock_notion)
        .await;

    let (status, _, ch_body) = h
        .send(
            Method::POST,
            "/api/v1/channels",
            Some(&cookie),
            Some(json!({
                "name": "Notion Vault",
                "kind": "notion",
                "config": {
                    "kind": "notion",
                    "integration_secret": "ntn_stored_secret_abcdef123456",
                    "data_source_id": ds_id,
                    "column_mapping": NotionColumnMapping::standard()
                },
                "min_interval_secs": 5,
                "jitter_max_secs": 2,
                "hourly_quota": null,
                "daily_quota": null,
                "use_proxy": false,
                "enabled": true
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let notion_ch_id = parse_json(&ch_body)["id"].as_i64().unwrap();

    let (status, _, inspect_body) = h
        .send(
            Method::POST,
            "/api/v1/channels/notion/inspect-schema",
            Some(&cookie),
            Some(json!({
                "secret": "",
                "channel_id": notion_ch_id,
                "data_source_or_db_id": ds_id,
                "patch_missing": false,
                "use_proxy": false
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse_json(&inspect_body)["ready"], true);

    // 4. MCP Token creation (`fc_mcp_<48 hex>`), listing (never exposing plaintext), and deletion.
    let (status, _, mcp_body) = h
        .send(
            Method::POST,
            "/api/v1/mcp-tokens",
            Some(&cookie),
            Some(json!({ "name": "Claude Desktop", "scope": "admin" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let mcp_created = parse_json(&mcp_body);
    let pt = mcp_created["plaintext_token"].as_str().unwrap();
    assert!(pt.starts_with("fc_mcp_"));
    assert_eq!(pt.len(), "fc_mcp_".len() + 48);
    assert!(
        mcp_created["client_config_json"]
            .as_str()
            .unwrap()
            .contains("http://localhost:8080/mcp")
    );
    let token_id = mcp_created["token"]["id"].as_i64().unwrap();

    let (status, _, list_mcp) = h
        .send(Method::GET, "/api/v1/mcp-tokens", Some(&cookie), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!String::from_utf8_lossy(&list_mcp).contains(pt));

    let (status, _, _) = h
        .send(
            Method::DELETE,
            &format!("/api/v1/mcp-tokens/{token_id}"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // 5. `NOT EXISTS` Payload GC: purges expired unrouted/delivered entries while protecting in-flight tasks!
    let old_ts = now_unix() - (48 * 3600);
    let purgeable_entry = match h
        .db
        .insert_entry_if_new(&NewEntry {
            feed_id,
            fingerprint_sha256: sha256_hex(b"old-delivered-entry"),
            published_at: Some(old_ts),
            discovered_at: old_ts,
            title: Some("Old Delivered Entry".to_string()),
            canonical_url: Some("https://example.com/old-1".to_string()),
            author: None,
            content_html: Some("<p>Transient HTML to purge</p>".to_string()),
            first_sentence: None,
            summary_text: None,
            media: vec![],
        })
        .await
        .unwrap()
    {
        feedcourier::db::models::EntryInsertOutcome::Inserted(e) => *e,
        _ => unreachable!(),
    };

    let protected_entry = match h
        .db
        .insert_entry_if_new(&NewEntry {
            feed_id,
            fingerprint_sha256: sha256_hex(b"old-retrying-entry"),
            published_at: Some(old_ts),
            discovered_at: old_ts,
            title: Some("Old Retrying Entry".to_string()),
            canonical_url: Some("https://example.com/old-2".to_string()),
            author: None,
            content_html: Some("<p>Must stay intact until delivered</p>".to_string()),
            first_sentence: None,
            summary_text: None,
            media: vec![],
        })
        .await
        .unwrap()
    {
        feedcourier::db::models::EntryInsertOutcome::Inserted(e) => *e,
        _ => unreachable!(),
    };

    let route =
        h.db.create_route(&NewRoute {
            name: "Notion Route".to_string(),
            feed_ids: vec![feed_id],
            match_tags: vec![],
            channel_id: notion_ch_id,
            include_keywords: vec![],
            exclude_keywords: vec![],
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions::default(),
            enabled: true,
        })
        .await
        .unwrap();

    let in_flight =
        h.db.enqueue_delivery_task(route.id, notion_ch_id, protected_entry.id, now_unix() + 600)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(in_flight.status, DeliveryTaskStatus::Queued);

    let (status, _, gc_body) = h
        .send(Method::POST, "/api/v1/maintenance/gc", Some(&cookie), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse_json(&gc_body)["purged_entries"], 1);

    let after_purged = h.db.get_entry(purgeable_entry.id).await.unwrap().unwrap();
    assert!(after_purged.is_purged());
    assert!(after_purged.content_html.is_none());
    assert_eq!(after_purged.fingerprint_sha256.len(), 64);

    let after_protected = h.db.get_entry(protected_entry.id).await.unwrap().unwrap();
    assert!(!after_protected.is_purged());
    assert!(after_protected.content_html.is_some());

    // 6. `GET /api/v1/maintenance/backup` streams a valid `VACUUM INTO` SQLite 3 database snapshot
    // and cleans up its temporary file in `data_dir`.
    let (status, backup_headers, backup_bytes) = h
        .send(
            Method::GET,
            "/api/v1/maintenance/backup",
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        backup_headers.get(CONTENT_TYPE).unwrap().to_str().unwrap(),
        "application/octet-stream"
    );
    assert!(
        backup_headers
            .get(CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("feedcourier-snapshot-")
    );
    assert!(backup_bytes.len() > 4096);
    assert_eq!(&backup_bytes[..16], b"SQLite format 3\0");

    let lingering_temps: Vec<_> = std::fs::read_dir(&h.config.data_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".feedcourier-backup-")
        })
        .collect();
    assert!(
        lingering_temps.is_empty(),
        "temporary backup snapshot must be removed after streaming"
    );
}

#[tokio::test]
async fn poller_batch_channel_test_notion_init_db_and_crud_mutations() {
    let mock_srv = MockServer::start().await;
    let base_uri = mock_srv.uri();
    let h = TestHarness::with_builder(|d| {
        let notion = NotionSender::with_base_url(&base_uri, None).unwrap();
        d.with_notion_sender(notion)
    })
    .await;
    let cookie = h.setup_admin_and_cookie().await;

    // Allowlist loopback for local mock feed polling.
    let (status, _, _) = h
        .send(
            Method::PUT,
            "/api/v1/settings",
            Some(&cookie),
            Some(json!({
                "outbound_proxy_url": null,
                "ssrf_allowlist": ["127.0.0.1/32", "::1/128"]
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // 1. `POST /api/v1/channels/notion/init-db` provisions 7-column Notion DB (`2026-03-11`).
    let parent_page_id = "22222222-3333-4444-5555-666666666666";
    Mock::given(method("POST"))
        .and(path("/v1/databases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "database",
            "id": "db-created-123",
            "url": "https://www.notion.so/db-created-123",
            "data_sources": [
                { "id": "ds-created-456", "name": "FeedCourier Inbox" }
            ]
        })))
        .expect(1)
        .mount(&mock_srv)
        .await;

    let (status, _, init_body) = h
        .send(
            Method::POST,
            "/api/v1/channels/notion/init-db",
            Some(&cookie),
            Some(json!({
                "secret": "ntn_init_db_secret_987654",
                "parent_page_id": parent_page_id,
                "title": "My Inbox",
                "use_proxy": false
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let init_json = parse_json(&init_body);
    assert_eq!(init_json["database_id"], "db-created-123");
    assert_eq!(init_json["data_source_id"], "ds-created-456");

    // 2. `POST /api/v1/channels/{id}/test` on Telegram probes & persists capabilities + sends test msg.
    Mock::given(method("GET"))
        .and(path("/bot777:test_channel_secret_token/getMe"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": {
                "id": 777,
                "is_bot": true,
                "first_name": "TestBot",
                "username": "fc_test_bot"
            }
        })))
        .mount(&mock_srv)
        .await;

    Mock::given(method("POST"))
        .and(path("/bot777:test_channel_secret_token/sendRichMessage"))
        .respond_with(|req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            if body.get("chat_id").is_none() {
                ResponseTemplate::new(400).set_body_json(json!({
                    "ok": false,
                    "error_code": 400,
                    "description": "Bad Request: chat_id is empty"
                }))
            } else {
                ResponseTemplate::new(200).set_body_json(json!({
                    "ok": true,
                    "result": { "message_id": 888 }
                }))
            }
        })
        .mount(&mock_srv)
        .await;

    let (status, _, tg_body) = h
        .send(
            Method::POST,
            "/api/v1/channels",
            Some(&cookie),
            Some(json!({
                "name": "Test TG Channel",
                "kind": "telegram",
                "config": {
                    "kind": "telegram",
                    "bot_token": "777:test_channel_secret_token",
                    "chat_id": "-100333",
                    "api_base_url": base_uri
                },
                "min_interval_secs": 0,
                "jitter_max_secs": 0,
                "hourly_quota": null,
                "daily_quota": null,
                "use_proxy": false,
                "enabled": true
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let ch_id = parse_json(&tg_body)["id"].as_i64().unwrap();

    let (status, _, test_resp_body) = h
        .send(
            Method::POST,
            &format!("/api/v1/channels/{ch_id}/test"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let test_resp = parse_json(&test_resp_body);
    assert_eq!(test_resp["ok"], true);
    assert_eq!(test_resp["capabilities"]["supports_rich_message"], true);

    // 3. `poll_due_feeds_once` + `POST /api/v1/feeds/{id}/poll` + PATCH/DELETE lifecycle.
    Mock::given(method("GET"))
        .and(path("/due.xml"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/atom+xml")
                .set_body_string(format!(
                    r#"<?xml version="1.0" encoding="utf-8"?>
                    <feed xmlns="http://www.w3.org/2005/Atom">
                      <title>Atom Stream</title>
                      <id>urn:uuid:atom-stream-1</id>
                      <updated>2026-11-13T10:00:00Z</updated>
                      <entry>
                        <id>urn:uuid:item-1</id>
                        <title>Scheduled Poll Entry</title>
                        <link href="{base_uri}/item-1"/>
                        <updated>2026-11-13T10:00:00Z</updated>
                        <summary>Delivered via poll_due_feeds_once</summary>
                      </entry>
                    </feed>"#
                )),
        )
        .mount(&mock_srv)
        .await;

    let (status, _, r_body) = h
        .send(
            Method::POST,
            "/api/v1/routes",
            Some(&cookie),
            Some(json!({
                "name": "Atom Route",
                "feed_ids": [],
                "match_tags": [],
                "channel_id": ch_id,
                "include_keywords": [],
                "exclude_keywords": [],
                "include_regex": null,
                "exclude_regex": null,
                "options": { "telegram_mode": "rich_message", "link_preview": true },
                "enabled": true
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let route_id = parse_json(&r_body)["id"].as_i64().unwrap();

    // Insert feed via DB with `silent_init: false` and `next_poll_at <= now` so `poll_due_feeds_once` claims it.
    let mut due_feed = NewFeed::new(format!("{base_uri}/due.xml"));
    due_feed.silent_init = false;
    due_feed.next_poll_at = Some(now_unix() - 30);
    let feed = h.db.create_feed(&due_feed).await.unwrap();

    let polled_count = h.dispatcher.poll_due_feeds_once(10).await.unwrap();
    assert_eq!(polled_count, 1);
    assert_eq!(h.dispatcher.drain_due_outbox_tasks(10).await.unwrap(), 1);

    // Manual `POST /api/v1/feeds/{id}/poll` succeeds and deduplicates existing entry (`new_entries == 0`).
    let (status, _, poll_body) = h
        .send(
            Method::POST,
            &format!("/api/v1/feeds/{}/poll", feed.id),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse_json(&poll_body)["new_entries"], 0);

    // PATCH route, PATCH feed (`paused`), then DELETE route, feed, and channel.
    let (status, _, _) = h
        .send(
            Method::PATCH,
            &format!("/api/v1/routes/{route_id}"),
            Some(&cookie),
            Some(json!({ "name": "Renamed Atom Route", "enabled": false })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, paused_body) = h
        .send(
            Method::PATCH,
            &format!("/api/v1/feeds/{}", feed.id),
            Some(&cookie),
            Some(json!({ "health_status": "paused", "title": "Custom Atom Title" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse_json(&paused_body)["health_status"], "paused");
    assert_eq!(parse_json(&paused_body)["title"], "Custom Atom Title");

    let (status, _, _) = h
        .send(
            Method::DELETE,
            &format!("/api/v1/routes/{route_id}"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, _) = h
        .send(
            Method::DELETE,
            &format!("/api/v1/feeds/{}", feed.id),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, _) = h
        .send(
            Method::DELETE,
            &format!("/api/v1/channels/{ch_id}"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}
