//! End-to-end integration tests for FeedCourier v0.3.1 Phase 3:
//! - HTTP Bridge Plugin Protocol v1 (HMAC-SHA256 signing & ±300s window)
//! - Bridge registration (`dry_run` preview, register, refresh-manifest, incompatible `min_core`)
//! - Strict host:port isolation (bridge allowed without `SSRF_ALLOWLIST`, private feeds still blocked, redirects rejected)
//! - Channel plugins (`ChannelKind::Plugin`, schema + remote `/validate`, secret-preserving PATCH, `/test`, `/deliver` outcomes & 16 KiB checkpoint cap)
//! - Processor bridge plugins (`/hooks/on_render`, `ammonia` re-sanitization, non-strict isolation, `RouteOptions.plugin_order`)
//! - Persistent at-least-once `on_delivered` via `plugin_events` + retry backoff + 5-attempt drop
//! - Referential delete guard (`409 Conflict` + `?cascade=true`).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request as MockRequest, ResponseTemplate};

use feedcourier::api::{AppState, build_api_router};
use feedcourier::channels::{ChannelKind, RouteOptions};
use feedcourier::config::AppConfig;
use feedcourier::crypto::{MasterKey, sha256_hex};
use feedcourier::db::Database;
use feedcourier::db::models::{
    DeliveryTaskStatus, EntryInsertOutcome, NewEntry, NewFeed, NewRoute, now_unix,
};
use feedcourier::plugins::bridge::{
    HEADER_IDEMPOTENCY_KEY, HEADER_SIGNATURE, HEADER_TIMESTAMP, sign_bridge_request,
    verify_bridge_signature,
};
use feedcourier::plugins::check_plugin_compatibility;
use feedcourier::scheduler::DeliveryDispatcher;

struct BridgeHarness {
    _temp_dir: TempDir,
    db: Database,
    dispatcher: DeliveryDispatcher,
    router: Router,
    session_cookie: String,
}

impl BridgeHarness {
    async fn new() -> Self {
        let temp_dir = TempDir::new().expect("tempdir");
        // Strictly EMPTY ssrf_allowlist to verify bridge isolation doesn't weaken SSRF.
        let config = AppConfig::with_data_dir(temp_dir.path());
        let db = Database::from_config(&config).await.expect("db");
        let master_key = MasterKey::from_config(&config).expect("master key");
        let dispatcher =
            DeliveryDispatcher::from_db_settings(config, db.clone(), master_key.clone())
                .await
                .expect("dispatcher");
        let router = build_api_router(AppState::new(dispatcher.clone()));

        let setup_req = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/auth/setup")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "username": "Courier", "password": "BridgeTestPassword123!" }).to_string(),
            ))
            .unwrap();
        let setup_resp = router.clone().oneshot(setup_req).await.unwrap();
        assert_eq!(setup_resp.status(), StatusCode::OK);
        let raw_cookie = setup_resp
            .headers()
            .get(SET_COOKIE)
            .expect("fc_session cookie")
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();

        Self {
            _temp_dir: temp_dir,
            db,
            dispatcher,
            router,
            session_cookie: raw_cookie,
        }
    }

    async fn req(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(COOKIE, &self.session_cookie);
        let payload = if let Some(b) = body {
            builder = builder.header(CONTENT_TYPE, "application/json");
            Body::from(b.to_string())
        } else {
            Body::empty()
        };
        let resp = self
            .router
            .clone()
            .oneshot(builder.body(payload).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }

    async fn seed_feed_and_entry(&self, html: &str, title: &str) -> (i64, i64) {
        let feed = self
            .db
            .create_feed(&NewFeed {
                url: format!("https://example.com/feed-{}.xml", rand::random::<u32>()),
                title: "Bridge Feed".to_string(),
                site_url: None,
                favicon_url: None,
                tags: vec!["tech".to_string()],
                poll_interval_secs: 1800,
                fetch_full_content: false,
                use_proxy: false,
                next_poll_at: None,
                silent_init: false,
            })
            .await
            .unwrap();
        let entry = match self
            .db
            .insert_entry_if_new(&NewEntry {
                feed_id: feed.id,
                fingerprint_sha256: sha256_hex(
                    format!("seed-{}-{}", title, rand::random::<u64>()).as_bytes(),
                ),
                canonical_url: Some("https://example.com/articles/1".to_string()),
                title: Some(title.to_string()),
                author: Some("Alice".to_string()),
                published_at: Some(now_unix()),
                discovered_at: now_unix(),
                first_sentence: Some("First sentence.".to_string()),
                summary_text: Some("Summary text.".to_string()),
                content_html: Some(html.to_string()),
                media: Vec::new(),
            })
            .await
            .unwrap()
        {
            EntryInsertOutcome::Inserted(e) => e,
            EntryInsertOutcome::Duplicate { .. } => panic!("expected fresh entry"),
        };
        (feed.id, entry.id)
    }
}

fn assert_valid_bridge_hmac(req: &MockRequest, secret: &str) {
    let ts: i64 = req
        .headers
        .get(HEADER_TIMESTAMP)
        .expect("X-FeedCourier-Timestamp header")
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    let sig = req
        .headers
        .get(HEADER_SIGNATURE)
        .expect("X-FeedCourier-Signature header")
        .to_str()
        .unwrap();
    assert!(
        verify_bridge_signature(secret, ts, &req.body, sig, now_unix()),
        "invalid bridge HMAC signature: {sig}"
    );
}

#[test]
fn bridge_hmac_signing_and_window_verification() {
    let secret = "super-secret-bridge-key";
    let body = br#"{"hello":"world"}"#;
    let ts = 1_700_000_000_i64;
    let sig = sign_bridge_request(secret, ts, body);

    assert!(sig.starts_with("v1="));
    assert!(verify_bridge_signature(secret, ts, body, &sig, ts));
    assert!(verify_bridge_signature(secret, ts, body, &sig, ts + 300));
    assert!(verify_bridge_signature(secret, ts, body, &sig, ts - 300));
    assert!(!verify_bridge_signature(secret, ts, body, &sig, ts + 301));
    assert!(!verify_bridge_signature(secret, ts, body, &sig, ts - 301));
    assert!(!verify_bridge_signature("wrong-secret", ts, body, &sig, ts));
    assert!(!verify_bridge_signature(secret, ts, b"tampered", &sig, ts));

    let (ok, reason) = check_plugin_compatibility("1", Some("0.3.0"));
    assert!(ok && reason.is_none());
    let (ok, reason) = check_plugin_compatibility("1", Some("99.0.0"));
    assert!(!ok && reason.unwrap().contains("99.0.0"));
    let (ok, reason) = check_plugin_compatibility("2", None);
    assert!(!ok && reason.unwrap().contains("v2"));
}

#[tokio::test]
async fn bridge_registration_network_isolation_and_redirect_rejection() {
    let h = BridgeHarness::new().await;
    let bridge = MockServer::start().await;
    let other_private_server = MockServer::start().await;
    let secret = "bridge-secret-isolation-01";

    let manifest_v1 = json!({
        "id": "demo-chan",
        "name": "Demo Channel",
        "version": "1.0.0",
        "api_version": "1",
        "kind": "channel",
        "category": "notification",
        "icon": "bell",
        "description": "Demo bridge channel",
        "hooks": [],
        "permissions": ["network"],
        "account_schema": {
            "type": "object",
            "required": ["server_url"],
            "properties": {
                "server_url": { "type": "string", "format": "uri" },
                "access_token": { "type": "string", "format": "password" }
            }
        },
        "destination_schema": {
            "type": "object",
            "required": ["topic"],
            "properties": {
                "topic": { "type": "string" }
            }
        }
    });

    Mock::given(method("GET"))
        .and(path("/manifest"))
        .respond_with({
            let m = manifest_v1.clone();
            move |req: &MockRequest| {
                assert_valid_bridge_hmac(req, secret);
                ResponseTemplate::new(200).set_body_json(m.clone())
            }
        })
        .mount(&bridge)
        .await;

    // 1. Dry-run preview works on 127.0.0.1 WITHOUT SSRF_ALLOWLIST!
    let (st, preview) = h
        .req(
            Method::POST,
            "/api/v1/plugins/bridges",
            Some(json!({
                "bridge_url": bridge.uri(),
                "bridge_secret": secret,
                "dry_run": true
            })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "preview failed: {preview}");
    assert_eq!(preview["manifest"]["id"], "demo-chan");
    assert_eq!(preview["compatible"], true);

    // 2. Real registration persists & loads into memory cache
    let (st, created) = h
        .req(
            Method::POST,
            "/api/v1/plugins/bridges",
            Some(json!({
                "bridge_url": bridge.uri(),
                "bridge_secret": secret,
                "enabled": true,
                "dry_run": false
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "register failed: {created}");
    assert_eq!(created["id"], "demo-chan");
    assert_eq!(created["runtime"], "bridge");
    assert_eq!(created["kind"], "channel");

    // 3. Unregistered 127.0.0.1:<other_port> feed preview MUST still be blocked by SSRF!
    Mock::given(method("GET"))
        .and(path("/rss.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<rss/>"))
        .mount(&other_private_server)
        .await;
    let (st, body) = h
        .req(
            Method::POST,
            "/api/v1/feeds/preview",
            Some(json!({
                "url": format!("{}/rss.xml", other_private_server.uri()),
                "fetch_full_content": false,
                "use_proxy": false
            })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "private feed MUST remain SSRF-blocked: {body}"
    );

    // 4. Refresh manifest updates version in DB & cache
    bridge.reset().await;
    let mut manifest_v2 = manifest_v1.clone();
    manifest_v2["version"] = json!("1.1.0");
    Mock::given(method("GET"))
        .and(path("/manifest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(manifest_v2))
        .mount(&bridge)
        .await;
    let (st, refreshed) = h
        .req(
            Method::POST,
            "/api/v1/plugins/demo-chan/refresh-manifest",
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "refresh failed: {refreshed}");
    assert_eq!(refreshed["installed_version"], "1.1.0");

    // 5. Bridge returning 302 redirect MUST be rejected (redirect::Policy::none)
    let redirect_bridge = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/manifest"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", "http://169.254.169.254/latest/meta-data"),
        )
        .mount(&redirect_bridge)
        .await;
    let (st, err_body) = h
        .req(
            Method::POST,
            "/api/v1/plugins/bridges",
            Some(json!({
                "bridge_url": redirect_bridge.uri(),
                "bridge_secret": secret,
                "dry_run": true
            })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "bridge redirect should fail with 403 SsrfBlocked: {err_body}"
    );

    // 6. Incompatible min_core rejected on registration
    let incompat_bridge = MockServer::start().await;
    let mut future_manifest = manifest_v1;
    future_manifest["id"] = json!("future-chan");
    future_manifest["min_core"] = json!("99.0.0");
    Mock::given(method("GET"))
        .and(path("/manifest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(future_manifest))
        .mount(&incompat_bridge)
        .await;
    let (st, err_body) = h
        .req(
            Method::POST,
            "/api/v1/plugins/bridges",
            Some(json!({
                "bridge_url": incompat_bridge.uri(),
                "bridge_secret": secret,
                "dry_run": false
            })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "incompatible min_core must be rejected: {err_body}"
    );
}

#[tokio::test]
async fn channel_bridge_deliver_outcomes_and_checkpoint_cap() {
    let h = BridgeHarness::new().await;
    let bridge = MockServer::start().await;
    let secret = "channel-bridge-secret-42";

    let manifest = json!({
        "id": "mock-ntfy",
        "name": "Mock ntfy",
        "version": "1.0.0",
        "api_version": "1",
        "kind": "channel",
        "category": "notification",
        "icon": "bell",
        "description": "Mock ntfy bridge",
        "hooks": [],
        "permissions": ["network"],
        "account_schema": {
            "type": "object",
            "required": ["server_url"],
            "properties": {
                "server_url": { "type": "string", "format": "uri" },
                "access_token": { "type": "string", "format": "password" }
            }
        },
        "destination_schema": {
            "type": "object",
            "required": ["topic"],
            "properties": {
                "topic": { "type": "string" }
            }
        },
        "capabilities": { "supports_html": false, "max_text_len": 4096 }
    });

    Mock::given(method("GET"))
        .and(path("/manifest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(manifest))
        .mount(&bridge)
        .await;
    Mock::given(method("POST"))
        .and(path("/validate"))
        .respond_with(move |req: &MockRequest| {
            assert_valid_bridge_hmac(req, secret);
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            ResponseTemplate::new(200)
                .set_body_json(json!({ "ok": true, "normalized_config": body["config"] }))
        })
        .mount(&bridge)
        .await;
    Mock::given(method("POST"))
        .and(path("/test"))
        .respond_with(move |req: &MockRequest| {
            assert_valid_bridge_hmac(req, secret);
            ResponseTemplate::new(200).set_body_json(json!({ "ok": true, "message": "ntfy OK" }))
        })
        .mount(&bridge)
        .await;

    let (st, _) = h
        .req(
            Method::POST,
            "/api/v1/plugins/bridges",
            Some(json!({
                "bridge_url": bridge.uri(),
                "bridge_secret": secret,
                "enabled": true
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);

    // Reject use_proxy=true on ChannelKind::Plugin
    let (st, _) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "kind": "plugin",
                "config": {
                    "kind": "plugin",
                    "plugin_id": "mock-ntfy",
                    "config": { "server_url": "https://ntfy.sh", "access_token": "tk_secret_9999" }
                },
                "use_proxy": true
            })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Create channel + initial destination
    let (st, ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "kind": "plugin",
                "config": {
                    "kind": "plugin",
                    "plugin_id": "mock-ntfy",
                    "config": { "server_url": "https://ntfy.sh", "access_token": "tk_secret_9999" }
                },
                "initial_destination": {
                    "config": { "kind": "plugin", "config": { "topic": "my-alerts" } }
                },
                "min_interval_secs": 0,
                "jitter_max_secs": 0
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "create channel failed: {ch}");
    assert_eq!(ch["name"], "Mock ntfy");
    assert_eq!(ch["config"]["config"]["has_access_token"], true);
    assert!(ch["config"]["config"].get("access_token").is_none());
    let channel_id = ch["id"].as_i64().unwrap();
    let dest_id = ch["destinations"][0]["id"].as_i64().unwrap();
    assert_eq!(ch["destinations"][0]["name"], "my-alerts");

    // PATCH preserves stored access_token when omitted/blank
    let (st, patched) = h
        .req(
            Method::PATCH,
            &format!("/api/v1/channels/{channel_id}"),
            Some(json!({
                "config": {
                    "kind": "plugin",
                    "config": { "server_url": "https://ntfy.example.org", "access_token": "" }
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "patch failed: {patched}");
    assert_eq!(patched["config"]["config"]["has_access_token"], true);
    assert_eq!(
        patched["config"]["config"]["server_url"],
        "https://ntfy.example.org"
    );

    // Test account & destination endpoints
    let (st, t1) = h
        .req(
            Method::POST,
            &format!("/api/v1/channels/{channel_id}/test"),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(t1["message"], "ntfy OK");
    let (st, t2) = h
        .req(
            Method::POST,
            &format!("/api/v1/destinations/{dest_id}/test"),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(t2["message"], "ntfy OK");

    let (feed_id, _) = h.seed_feed_and_entry("<p>Hello bridge</p>", "Hello").await;
    let route =
        h.db.create_route(&NewRoute {
            name: "To ntfy".to_string(),
            feed_ids: vec![feed_id],
            match_tags: vec![],
            channel_id,
            destination_id: dest_id,
            include_keywords: vec![],
            exclude_keywords: vec![],
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions::default(),
            enabled: true,
        })
        .await
        .unwrap();

    // Helper to enqueue one fresh entry + task and process it with a specific `/deliver` response
    let run_case = |resp: ResponseTemplate| {
        let bridge = &bridge;
        let h = &h;
        let route_id = route.id;
        async move {
            bridge.reset().await;
            Mock::given(method("POST"))
                .and(path("/deliver"))
                .respond_with(resp)
                .mount(bridge)
                .await;
            let entry = match h
                .db
                .insert_entry_if_new(&NewEntry {
                    feed_id,
                    fingerprint_sha256: sha256_hex(
                        format!("case-{}", rand::random::<u64>()).as_bytes(),
                    ),
                    canonical_url: Some("https://example.com/p".to_string()),
                    title: Some("Title".to_string()),
                    author: None,
                    published_at: Some(now_unix()),
                    discovered_at: now_unix(),
                    first_sentence: None,
                    summary_text: Some("Summary".to_string()),
                    content_html: Some("<p>Body</p>".to_string()),
                    media: vec![],
                })
                .await
                .unwrap()
            {
                EntryInsertOutcome::Inserted(e) => e,
                EntryInsertOutcome::Duplicate { .. } => panic!("expected fresh entry"),
            };
            let task =
                h.db.enqueue_delivery_task(route_id, channel_id, dest_id, entry.id, now_unix())
                    .await
                    .unwrap()
                    .unwrap();
            assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
            h.db.get_delivery_task(task.id).await.unwrap().unwrap()
        }
    };

    // Case 1: delivered + checkpoint + external_id
    let t = run_case(ResponseTemplate::new(200).set_body_json(json!({
        "outcome": "delivered",
        "external_id": "ntfy-msg-1",
        "checkpoint": { "cursor": "abc" }
    })))
    .await;
    assert_eq!(t.status, DeliveryTaskStatus::Delivered);
    assert!(
        t.diagnostic_log
            .as_deref()
            .unwrap_or("")
            .contains("external_id=ntfy-msg-1")
    );
    assert!(t.checkpoint_json.as_deref().unwrap_or("").contains("abc"));

    // Case 2: retry with retry_after_secs
    let before = now_unix();
    let t = run_case(ResponseTemplate::new(200).set_body_json(json!({
        "outcome": "retry",
        "retry_after_secs": 42,
        "diagnostic": "upstream busy"
    })))
    .await;
    assert_eq!(t.status, DeliveryTaskStatus::Retrying);
    assert!(t.next_attempt_at >= before + 42);

    // Verify cooldown was set, then clear it directly so subsequent cases can claim immediately
    assert!(
        h.db.get_channel(channel_id)
            .await
            .unwrap()
            .unwrap()
            .cooldown_until
            .is_some()
    );
    sqlx::query("UPDATE channels SET cooldown_until = NULL WHERE id = ?1")
        .bind(channel_id)
        .execute(h.db.write_pool())
        .await
        .unwrap();

    // Case 3: HTTP 429 + Retry-After header -> Retrying + cooldown
    let t = run_case(
        ResponseTemplate::new(429)
            .insert_header("Retry-After", "30")
            .set_body_string("slow down"),
    )
    .await;
    assert_eq!(t.status, DeliveryTaskStatus::Retrying);
    assert_eq!(t.last_status_code, Some(429));
    assert!(
        h.db.get_channel(channel_id)
            .await
            .unwrap()
            .unwrap()
            .cooldown_until
            .is_some()
    );
    sqlx::query("UPDATE channels SET cooldown_until = NULL WHERE id = ?1")
        .bind(channel_id)
        .execute(h.db.write_pool())
        .await
        .unwrap();

    // Case 4: HTTP 503 -> Retrying
    let t = run_case(ResponseTemplate::new(503).set_body_string("unavailable")).await;
    assert_eq!(t.status, DeliveryTaskStatus::Retrying);
    assert_eq!(t.last_status_code, Some(503));

    // Case 5: HTTP 400 -> PermanentFailure (DeadLetter)
    let t = run_case(ResponseTemplate::new(400).set_body_string("bad topic")).await;
    assert_eq!(t.status, DeliveryTaskStatus::DeadLetter);
    assert_eq!(t.last_status_code, Some(400));

    // Case 6: JSON permanent_failure -> DeadLetter
    let t = run_case(ResponseTemplate::new(200).set_body_json(json!({
        "outcome": "permanent_failure",
        "diagnostic": "topic banned"
    })))
    .await;
    assert_eq!(t.status, DeliveryTaskStatus::DeadLetter);

    // Case 7: JSON deferred -> Deferred (attempt_count restored to 0)
    let t = run_case(ResponseTemplate::new(200).set_body_json(json!({
        "outcome": "deferred",
        "retry_after_secs": 120,
        "diagnostic": "quiet window"
    })))
    .await;
    assert_eq!(t.status, DeliveryTaskStatus::Deferred);
    assert_eq!(t.attempt_count, 0);

    // Case 8: checkpoint > 16 KiB -> DeadLetter
    let huge = "x".repeat(20 * 1024);
    let t = run_case(ResponseTemplate::new(200).set_body_json(json!({
        "outcome": "delivered",
        "checkpoint": { "blob": huge }
    })))
    .await;
    assert_eq!(t.status, DeliveryTaskStatus::DeadLetter);
    assert!(t.diagnostic_log.as_deref().unwrap_or("").contains("16 KiB"));
}

#[tokio::test]
async fn processor_bridge_on_render_ammonia_sanitization_non_strict_and_plugin_order() {
    let h = BridgeHarness::new().await;
    let proc_a = MockServer::start().await;
    let proc_b = MockServer::start().await;
    let chan = MockServer::start().await;
    let secret = "proc-bridge-secret";

    // Register channel bridge capturing delivered HTML
    let captured_html = Arc::new(std::sync::Mutex::new(String::new()));
    Mock::given(method("GET"))
        .and(path("/manifest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "sink-chan",
            "name": "Sink",
            "version": "1.0.0",
            "api_version": "1",
            "kind": "channel",
            "hooks": [],
            "account_schema": { "type": "object", "properties": {} },
            "destination_schema": { "type": "object", "properties": {} }
        })))
        .mount(&chan)
        .await;
    Mock::given(method("POST"))
        .and(path("/validate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .mount(&chan)
        .await;
    Mock::given(method("POST"))
        .and(path("/deliver"))
        .respond_with({
            let cap = Arc::clone(&captured_html);
            move |req: &MockRequest| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                *cap.lock().unwrap() = body["entry"]["content_html"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                ResponseTemplate::new(200).set_body_json(json!({ "outcome": "delivered" }))
            }
        })
        .mount(&chan)
        .await;

    // Processor A appends <p>ALPHA</p><script>alert(1)</script>
    Mock::given(method("GET"))
        .and(path("/manifest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "proc-alpha",
            "name": "Alpha",
            "version": "1.0.0",
            "api_version": "1",
            "kind": "processor",
            "hooks": ["on_render"]
        })))
        .mount(&proc_a)
        .await;
    Mock::given(method("POST"))
        .and(path("/hooks/on_render"))
        .respond_with(move |req: &MockRequest| {
            assert_valid_bridge_hmac(req, secret);
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let cur = body["entry"]["content_html"].as_str().unwrap_or("");
            ResponseTemplate::new(200).set_body_json(json!({
                "content_html": format!("{cur}<p>ALPHA</p><script>alert(1)</script>")
            }))
        })
        .mount(&proc_a)
        .await;

    // Processor B appends <p>BETA</p>
    Mock::given(method("GET"))
        .and(path("/manifest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "proc-beta",
            "name": "Beta",
            "version": "1.0.0",
            "api_version": "1",
            "kind": "processor",
            "hooks": ["on_render"]
        })))
        .mount(&proc_b)
        .await;
    Mock::given(method("POST"))
        .and(path("/hooks/on_render"))
        .respond_with(move |req: &MockRequest| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let cur = body["entry"]["content_html"].as_str().unwrap_or("");
            ResponseTemplate::new(200).set_body_json(json!({
                "content_html": format!("{cur}<p>BETA</p>")
            }))
        })
        .mount(&proc_b)
        .await;

    for url in [chan.uri(), proc_a.uri(), proc_b.uri()] {
        let (st, res) = h
            .req(
                Method::POST,
                "/api/v1/plugins/bridges",
                Some(json!({ "bridge_url": url, "bridge_secret": secret, "enabled": true })),
            )
            .await;
        assert_eq!(st, StatusCode::CREATED, "register failed: {res}");
    }

    let (st, ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "kind": "plugin",
                "config": { "kind": "plugin", "plugin_id": "sink-chan", "config": {} },
                "initial_destination": { "config": { "kind": "plugin", "config": {} } },
                "min_interval_secs": 0,
                "jitter_max_secs": 0
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let ch_id = ch["id"].as_i64().unwrap();
    let dest_id = ch["destinations"][0]["id"].as_i64().unwrap();

    let (feed_id, entry1_id) = h.seed_feed_and_entry("<p>ORIG</p>", "Order Test").await;
    let mut route_plugins = BTreeMap::new();
    route_plugins.insert("proc-alpha".to_string(), json!({}));
    route_plugins.insert("proc-beta".to_string(), json!({}));
    let route =
        h.db.create_route(&NewRoute {
            name: "Ordered Route".to_string(),
            feed_ids: vec![feed_id],
            match_tags: vec![],
            channel_id: ch_id,
            destination_id: dest_id,
            include_keywords: vec![],
            exclude_keywords: vec![],
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions {
                plugins: route_plugins,
                plugin_order: vec!["proc-beta".to_string(), "proc-alpha".to_string()],
                ..RouteOptions::default()
            },
            enabled: true,
        })
        .await
        .unwrap();

    let task1 =
        h.db.enqueue_delivery_task(route.id, ch_id, dest_id, entry1_id, now_unix())
            .await
            .unwrap()
            .unwrap();
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    let done1 = h.db.get_delivery_task(task1.id).await.unwrap().unwrap();
    assert_eq!(done1.status, DeliveryTaskStatus::Delivered);

    let html = captured_html.lock().unwrap().clone();
    let beta_pos = html.find("BETA").expect("BETA present");
    let alpha_pos = html.find("ALPHA").expect("ALPHA present");
    assert!(
        beta_pos < alpha_pos,
        "plugin_order must run proc-beta before proc-alpha: {html}"
    );
    assert!(
        !html.contains("<script") && !html.contains("alert(1)"),
        "ammonia MUST strip <script> from bridge HTML: {html}"
    );

    // Non-strict isolation: proc-beta returns HTTP 500 -> delivery still succeeds with warning!
    proc_b.reset().await;
    Mock::given(method("POST"))
        .and(path("/hooks/on_render"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&proc_b)
        .await;

    let entry2 = match h
        .db
        .insert_entry_if_new(&NewEntry {
            feed_id,
            fingerprint_sha256: sha256_hex(b"fp-nonstrict-test"),
            canonical_url: Some("https://example.com/2".to_string()),
            title: Some("Non-Strict".to_string()),
            author: None,
            published_at: Some(now_unix()),
            discovered_at: now_unix(),
            first_sentence: None,
            summary_text: None,
            content_html: Some("<p>SAFE</p>".to_string()),
            media: vec![],
        })
        .await
        .unwrap()
    {
        EntryInsertOutcome::Inserted(e) => e,
        EntryInsertOutcome::Duplicate { .. } => panic!("expected fresh entry"),
    };
    let task2 =
        h.db.enqueue_delivery_task(route.id, ch_id, dest_id, entry2.id, now_unix())
            .await
            .unwrap()
            .unwrap();
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    let done2 = h.db.get_delivery_task(task2.id).await.unwrap().unwrap();
    assert_eq!(done2.status, DeliveryTaskStatus::Delivered);
    assert!(
        done2
            .diagnostic_log
            .as_deref()
            .unwrap_or("")
            .contains("proc-beta"),
        "non-strict warning must be recorded in diagnostic_log: {:?}",
        done2.diagnostic_log
    );
    assert!(captured_html.lock().unwrap().contains("ALPHA"));
}

#[tokio::test]
async fn on_delivered_persistent_events_retry_backoff_and_409_cascade_delete() {
    let h = BridgeHarness::new().await;
    let bridge = MockServer::start().await;
    let secret = "ondelivered-secret-key";

    Mock::given(method("GET"))
        .and(path("/manifest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "audit-chan",
            "name": "Audit Channel",
            "version": "1.0.0",
            "api_version": "1",
            "kind": "channel",
            "hooks": ["on_delivered"],
            "account_schema": { "type": "object", "properties": {} },
            "destination_schema": { "type": "object", "properties": {} }
        })))
        .mount(&bridge)
        .await;
    Mock::given(method("POST"))
        .and(path("/validate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .mount(&bridge)
        .await;
    Mock::given(method("POST"))
        .and(path("/deliver"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "outcome": "delivered" })))
        .mount(&bridge)
        .await;

    let attempts = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/hooks/on_delivered"))
        .respond_with({
            let attempts = Arc::clone(&attempts);
            move |req: &MockRequest| {
                assert_valid_bridge_hmac(req, secret);
                assert!(req.headers.get(HEADER_IDEMPOTENCY_KEY).is_some());
                let n = attempts.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    ResponseTemplate::new(500).set_body_string("temporary error")
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({ "ok": true }))
                }
            }
        })
        .mount(&bridge)
        .await;

    let (st, _) = h
        .req(
            Method::POST,
            "/api/v1/plugins/bridges",
            Some(json!({ "bridge_url": bridge.uri(), "bridge_secret": secret, "enabled": true })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);

    let (st, ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "kind": "plugin",
                "config": { "kind": "plugin", "plugin_id": "audit-chan", "config": {} },
                "initial_destination": { "config": { "kind": "plugin", "config": {} } },
                "min_interval_secs": 0,
                "jitter_max_secs": 0
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let ch_id = ch["id"].as_i64().unwrap();
    let dest_id = ch["destinations"][0]["id"].as_i64().unwrap();

    let (feed_id, entry_id) = h.seed_feed_and_entry("<p>Event test</p>", "Event").await;
    let route =
        h.db.create_route(&NewRoute {
            name: "Audit Route".to_string(),
            feed_ids: vec![feed_id],
            match_tags: vec![],
            channel_id: ch_id,
            destination_id: dest_id,
            include_keywords: vec![],
            exclude_keywords: vec![],
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions::default(),
            enabled: true,
        })
        .await
        .unwrap();

    let task =
        h.db.enqueue_delivery_task(route.id, ch_id, dest_id, entry_id, now_unix())
            .await
            .unwrap()
            .unwrap();
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    let done = h.db.get_delivery_task(task.id).await.unwrap().unwrap();
    assert_eq!(done.status, DeliveryTaskStatus::Delivered);

    // Event persisted atomically in `plugin_events`
    let due =
        h.db.list_due_plugin_events(now_unix() + 10, 10)
            .await
            .unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].plugin_id, "audit-chan");

    // 1st attempt fails (HTTP 500) -> rescheduled with attempts=1, backoff >= 15s
    assert_eq!(
        h.dispatcher
            .process_due_plugin_events_once(10)
            .await
            .unwrap(),
        1
    );
    let remaining =
        h.db.list_due_plugin_events(now_unix() + 60, 10)
            .await
            .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].attempts, 1);

    // Backdate next_attempt_at so 2nd attempt runs immediately -> succeeds (HTTP 200) -> deleted!
    h.db.reschedule_plugin_event(remaining[0].id, 1, now_unix() - 1)
        .await
        .unwrap();
    assert_eq!(
        h.dispatcher
            .process_due_plugin_events_once(10)
            .await
            .unwrap(),
        1
    );
    let empty =
        h.db.list_due_plugin_events(now_unix() + 3600, 10)
            .await
            .unwrap();
    assert!(empty.is_empty());
    assert_eq!(attempts.load(Ordering::SeqCst), 2);

    // Delete guard: 409 Conflict when referenced by channel & route
    let (st, conflict) = h
        .req(Method::DELETE, "/api/v1/plugins/audit-chan", None)
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "expected 409: {conflict}");
    assert_eq!(
        conflict["referencing_channels"].as_array().unwrap().len(),
        1
    );
    assert_eq!(conflict["referencing_routes"].as_array().unwrap().len(), 1);

    // Cascade delete succeeds and cleans up plugin, channel & route
    let (st, ack) = h
        .req(
            Method::DELETE,
            "/api/v1/plugins/audit-chan?cascade=true",
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "cascade delete failed: {ack}");
    assert!(h.db.get_plugin("audit-chan").await.unwrap().is_none());
    assert!(h.db.get_channel(ch_id).await.unwrap().is_none());
    assert!(h.db.get_route(route.id).await.unwrap().is_none());
    assert!(h.dispatcher.plugins().get_bridge("audit-chan").is_none());

    // Sanity: ChannelKind::Plugin parses & formats cleanly
    assert_eq!(ChannelKind::Plugin.as_str(), "plugin");
}
