//! End-to-end integration & security tests for FeedCourier v0.3.1 In-Process JS Plugins (`boa_engine`):
//! - Sandbox limits (infinite loop termination, `import` rejection, 512 KiB size cap, async/Promise rejection, `globalThis.fc` utilities)
//! - Dual network allowlist (admin-configured `format: "uri"` direct LAN access without relaxing `SSRF_ALLOWLIST`,
//!   undeclared hosts blocked, cloud metadata hard-blocked, HTTP 3xx redirects rejected, `use_proxy` fail-closed)
//! - Marketplace 1-click install (`dry_run` preview, SHA-256 verification & tampering rejection, upgrade + `capabilities_json` sync,
//!   `/source` inspection, custom `.js` upload overwrite & kind-mutation guards, `409 Conflict` & `?cascade=true` uninstall)
//! - Real official `ntfy` and `bark` single-file JS channel plugins end-to-end (secret redaction, test endpoints,
//!   Telegram Compact Card Bark formatting, UTF-8 byte truncation, 429/5xx retry vs 4xx dead-letter, unique debug-push keys)
//! - JS processor plugins (`onRender` `ammonia` re-sanitization, `plugin_order`, `strict_mode`, `onDelivered` durable `plugin_events` + cancellation)
//! - SQLite `0004_js_plugins.sql` `sqlite_sequence` watermark preservation & incompatible persisted plugin error reporting

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request as MockRequest, ResponseTemplate};

use feedcourier::api::{AppState, build_api_only_router};
use feedcourier::channels::{
    ChannelConfig, ChannelSender, DestinationConfig, PluginChannelConfig, PluginDestinationConfig,
};
use feedcourier::config::AppConfig;
use feedcourier::crypto::{MasterKey, sha256_hex};
use feedcourier::db::Database;
use feedcourier::db::models::{DeliveryTaskStatus, NewEntry, NewFeed, now_unix};
use feedcourier::net::{NetSettings, ProtectedHttpClient};
use feedcourier::plugins::js_runtime::{
    JS_MAX_SCRIPT_BYTES, JsChannelSender, compile_js_plugin, truncate_utf8_bytes_str,
};
use feedcourier::scheduler::DeliveryDispatcher;

struct JsHarness {
    _dir: TempDir,
    db: Database,
    _master_key: MasterKey,
    dispatcher: DeliveryDispatcher,
    router: axum::Router,
    cookie: String,
}

impl JsHarness {
    async fn new() -> Self {
        Self::with_options(None, vec![]).await
    }

    async fn with_options(plugin_index_url: Option<String>, ssrf_allowlist: Vec<String>) -> Self {
        let dir = TempDir::new().expect("temp dir");
        let mut cfg = AppConfig::with_data_dir(dir.path());
        cfg.ssrf_allowlist = ssrf_allowlist.clone();
        cfg.plugin_index_url = plugin_index_url;

        let db = Database::from_config(&cfg).await.expect("db init");
        let master_key = MasterKey::from_config(&cfg).expect("master key");
        let http = ProtectedHttpClient::new(NetSettings {
            outbound_proxy_url: None,
            ssrf_allowlist,
        })
        .expect("http client");
        let dispatcher =
            DeliveryDispatcher::new(cfg, db.clone(), master_key.clone(), http).expect("dispatcher");
        dispatcher
            .plugins()
            .sync_builtin_rows(&db)
            .await
            .expect("sync builtins");
        dispatcher
            .plugins()
            .reload_from_db(&db)
            .await
            .expect("reload plugins");

        let state = AppState::new(dispatcher.clone());
        let router = build_api_only_router(state);

        let setup_req = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/auth/setup")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "username": "Courier", "password": "JsPluginPassword123!" }).to_string(),
            ))
            .unwrap();
        let setup_resp = router.clone().oneshot(setup_req).await.unwrap();
        assert_eq!(setup_resp.status(), StatusCode::OK);
        let cookie = setup_resp
            .headers()
            .get(header::SET_COOKIE)
            .expect("session cookie")
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();

        Self {
            _dir: dir,
            db,
            _master_key: master_key,
            dispatcher,
            router,
            cookie,
        }
    }

    async fn req(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::COOKIE, &self.cookie);
        let body = if let Some(val) = body {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(val.to_string())
        } else {
            Body::empty()
        };
        let resp = self
            .router
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json_val = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
        (status, json_val)
    }

    async fn seed_feed_and_entry(
        &self,
        title: &str,
        summary: Option<&str>,
        html: &str,
        author: Option<&str>,
        published_at: Option<i64>,
        favicon_url: Option<&str>,
    ) -> (i64, i64) {
        let mut nf = NewFeed::new(format!(
            "https://example.com/feed-{}-{}.xml",
            now_unix(),
            rand::random::<u64>()
        ));
        nf.title = "Tech Daily".to_string();
        nf.site_url = Some("https://example.com".to_string());
        nf.favicon_url = favicon_url.map(ToOwned::to_owned);
        let feed = self.db.create_feed(&nf).await.unwrap();
        let fp = sha256_hex(format!("{title}:{html}:{}", rand::random::<u64>()).as_bytes());
        let inserted = self
            .db
            .insert_entry_if_new(&NewEntry {
                feed_id: feed.id,
                fingerprint_sha256: fp,
                published_at,
                discovered_at: now_unix(),
                title: Some(title.to_string()),
                canonical_url: Some("https://example.com/articles/42".to_string()),
                author: author.map(ToOwned::to_owned),
                content_html: Some(html.to_string()),
                first_sentence: summary.map(ToOwned::to_owned),
                summary_text: summary.map(ToOwned::to_owned),
                media: vec![],
            })
            .await
            .unwrap();
        let entry_id = match inserted {
            feedcourier::db::models::EntryInsertOutcome::Inserted(e) => e.id,
            feedcourier::db::models::EntryInsertOutcome::Duplicate { existing_id } => existing_id,
        };
        (feed.id, entry_id)
    }
}

// =============================================================================
// 1. Sandbox Limits, Zero-Import Guard & `globalThis.fc` Utilities
// =============================================================================

#[tokio::test]
async fn js_sandbox_limits_reject_infinite_loop_import_async_and_oversize() {
    // 1. Infinite loop terminated cleanly by Boa RuntimeLimits
    let loop_js = r#"
        export const meta = {
            id: "loop-bad", name: "Loop", version: "0.1.0", api_version: "1", kind: "channel"
        };
        while (true) {}
        export function deliver() { return { outcome: "delivered" }; }
    "#;
    let err = compile_js_plugin(loop_js.to_string())
        .await
        .expect_err("infinite loop must be terminated");
    assert!(
        err.to_string().to_lowercase().contains("loop")
            || err.to_string().to_lowercase().contains("limit")
            || err.to_string().to_lowercase().contains("evaluation"),
        "unexpected loop error: {err}"
    );

    // 2. Static ES import rejected by IdleModuleLoader
    let import_js = r#"
        import fs from "node:fs";
        export const meta = {
            id: "imp-bad", name: "Imp", version: "0.1.0", api_version: "1", kind: "channel"
        };
        export function deliver() { return { outcome: "delivered" }; }
    "#;
    let err = compile_js_plugin(import_js.to_string())
        .await
        .expect_err("import must be rejected");
    assert!(
        err.to_string().to_lowercase().contains("module")
            || err.to_string().to_lowercase().contains("disabled"),
        "unexpected import error: {err}"
    );

    // 3. Oversized > 512 KiB rejected immediately
    let huge = "a".repeat(JS_MAX_SCRIPT_BYTES + 1);
    let err = compile_js_plugin(huge)
        .await
        .expect_err("oversized script must be rejected");
    assert!(err.to_string().contains("512 KiB"), "got: {err}");

    // 4. `fc.truncateUtf8Bytes` scalar boundary helper sanity check
    assert_eq!(truncate_utf8_bytes_str("你好世界", 6), "你好");
    assert_eq!(truncate_utf8_bytes_str("你好世界", 5), "你");
    assert_eq!(truncate_utf8_bytes_str("A🚀B", 4), "A"); // 1 + 4 = 5 > 4
    assert_eq!(truncate_utf8_bytes_str("A🚀B", 5), "A🚀");

    // 5. Async hook returning a Promise is rejected cleanly & `globalThis.fc` utilities work
    let h = JsHarness::new().await;
    let util_and_async_js = r#"
        export const meta = {
            id: "fc-util-check",
            name: "FC Util Check",
            version: "0.1.0",
            api_version: "1",
            kind: "channel",
            account_schema: { type: "object", properties: {} },
            destination_schema: { type: "object", properties: {} }
        };
        export function test() {
            const sha = fc.sha256("hello");
            const md5 = fc.md5("hello");
            const hmac = fc.hmacSha256("msg", "secret", "hex");
            const b64 = fc.base64Encode("你好");
            const dec = fc.base64Decode(b64);
            const b64u = fc.base64UrlEncode("a+b/c=");
            const len = fc.utf8ByteLength("你好");
            const trunc = fc.truncateUtf8Bytes("你好世界", 5);
            const plain = fc.htmlToText("<p>Hello <b>World</b></p>");
            const ts = fc.unixNow();
            const id = fc.uuid();
            if (sha.length !== 64 || md5.length !== 32 || hmac.length !== 64) {
                return { ok: false, error: "bad hash len" };
            }
            if (dec !== "你好" || len !== 6 || trunc !== "你") {
                return { ok: false, error: "bad utf8/base64" };
            }
            if (b64u.includes("=") || !plain.includes("Hello") || ts < 1700000000 || id.length !== 36) {
                return { ok: false, error: "bad helper output" };
            }
            return { ok: true, message: "all fc helpers verified" };
        }
        export async function deliver() {
            return { outcome: "delivered" };
        }
    "#;
    let (st, _) = h
        .req(
            Method::POST,
            "/api/v1/plugins/upload",
            Some(json!({ "script_js": util_and_async_js, "enabled": true })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);

    let (st, ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Util Acct",
                "kind": "plugin",
                "config": { "kind": "plugin", "plugin_id": "fc-util-check", "config": {} },
                "initial_destination": {
                    "name": "Target",
                    "config": { "kind": "plugin", "config": {} }
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let ch_id = ch["id"].as_i64().unwrap();
    let dest_id = ch["destinations"][0]["id"].as_i64().unwrap();

    // `test()` verifies every `globalThis.fc` helper
    let (st, test_resp) = h
        .req(
            Method::POST,
            &format!("/api/v1/channels/{ch_id}/test"),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "fc util test failed: {test_resp}");
    assert_eq!(test_resp["message"], "all fc helpers verified");

    // `async function deliver()` fails cleanly with permanent_failure mentioning Promise
    let (feed_id, _) = h
        .seed_feed_and_entry("Async Test", None, "<p>Body</p>", None, None, None)
        .await;
    let (st, route) = h
        .req(
            Method::POST,
            "/api/v1/routes",
            Some(json!({
                "name": "Async Route",
                "feed_ids": [feed_id],
                "channel_id": ch_id,
                "destination_id": dest_id
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let route_id = route["id"].as_i64().unwrap();
    let entries = h.db.list_entries_for_feed(feed_id, 10).await.unwrap();
    let task_id =
        h.db.enqueue_delivery_task(route_id, ch_id, dest_id, entries[0].id, now_unix())
            .await
            .unwrap()
            .unwrap()
            .id;
    h.dispatcher.drain_due_outbox_tasks(10).await.unwrap();
    let task = h.db.get_delivery_task(task_id).await.unwrap().unwrap();
    assert_eq!(task.status, DeliveryTaskStatus::DeadLetter);
    assert!(
        task.diagnostic_log
            .as_deref()
            .unwrap_or("")
            .contains("Promise"),
        "expected Promise rejection diagnostic: {:?}",
        task.diagnostic_log
    );
}

// =============================================================================
// 2. Network Isolation: Admin URI vs `allowed_hosts`, Metadata, Redirect & `use_proxy`
// =============================================================================

#[tokio::test]
async fn js_network_isolation_admin_uri_metadata_redirect_and_use_proxy() {
    // Strictly EMPTY ssrf_allowlist!
    let h = JsHarness::new().await;
    let lan_server = MockServer::start().await;
    let undeclared_server = MockServer::start().await;
    let redirect_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/ping"))
        .respond_with(ResponseTemplate::new(200).set_body_string("pong"))
        .mount(&lan_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/secret"))
        .respond_with(ResponseTemplate::new(200).set_body_string("should-not-reach"))
        .mount(&undeclared_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/redir"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", "http://169.254.169.254/latest/meta-data/"),
        )
        .mount(&redirect_server)
        .await;

    let net_plugin_js = r#"
        export const meta = {
            id: "net-probe",
            name: "Net Probe",
            version: "0.1.0",
            api_version: "1",
            kind: "channel",
            allowed_hosts: ["api.example.com"],
            account_schema: {
                type: "object",
                required: ["server_url"],
                properties: {
                    server_url: { type: "string", format: "uri" },
                    target_path: { type: "string", default: "/ping" },
                    override_url: { type: "string" }
                }
            },
            destination_schema: { type: "object", properties: {} }
        };
        export function test(ctx) {
            const url = ctx.account.override_url || (ctx.account.server_url + (ctx.account.target_path || "/ping"));
            const res = fc.fetch({ method: "GET", url });
            return { ok: res.ok, message: "status=" + res.status + " body=" + res.body };
        }
        export function deliver() { return { outcome: "delivered" }; }
        "#;

    let (st, _) = h
        .req(
            Method::POST,
            "/api/v1/plugins/upload",
            Some(json!({ "script_js": net_plugin_js, "enabled": true })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);

    // 1. Admin-configured `format: "uri"` LAN server works WITHOUT adding 127.0.0.1 to SSRF_ALLOWLIST
    let (st, ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "LAN Self-Hosted",
                "kind": "plugin",
                "config": {
                    "kind": "plugin",
                    "plugin_id": "net-probe",
                    "config": { "server_url": lan_server.uri() }
                }
            })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "failed creating LAN plugin channel: {ch}"
    );
    let ch_id = ch["id"].as_i64().unwrap();

    let (st, test_ok) = h
        .req(
            Method::POST,
            &format!("/api/v1/channels/{ch_id}/test"),
            None,
        )
        .await;
    assert_eq!(
        st,
        StatusCode::OK,
        "admin URI LAN fetch should succeed: {test_ok}"
    );
    assert_eq!(test_ok["message"], "status=200 body=pong");

    // 2. Feed preview against the same LAN server is STILL blocked by SSRF!
    let (st, feed_err) = h
        .req(
            Method::POST,
            "/api/v1/feeds/preview",
            Some(json!({ "url": format!("{}/ping", lan_server.uri()) })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "feed SSRF must remain blocked: {feed_err}"
    );

    // 3. Undeclared host (`undeclared_server`) MUST be rejected with 403 SsrfBlocked
    let (st, ch_unauth) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Undeclared Target",
                "kind": "plugin",
                "config": {
                    "kind": "plugin",
                    "plugin_id": "net-probe",
                    "config": {
                        "server_url": lan_server.uri(),
                        "override_url": format!("{}/secret", undeclared_server.uri())
                    }
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, err_unauth) = h
        .req(
            Method::POST,
            &format!(
                "/api/v1/channels/{}/test",
                ch_unauth["id"].as_i64().unwrap()
            ),
            None,
        )
        .await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "undeclared host must fail with 403 SsrfBlocked: {err_unauth}"
    );

    // 4. Cloud metadata IP (`169.254.169.254` and `100.100.100.200`) rejected even in admin `server_url`!
    for meta_url in [
        "http://169.254.169.254/latest/meta-data/",
        "http://100.100.100.200/latest/meta-data/",
    ] {
        let (st, meta_err) = h
            .req(
                Method::POST,
                "/api/v1/channels",
                Some(json!({
                    "name": "Meta Bad",
                    "kind": "plugin",
                    "config": {
                        "kind": "plugin",
                        "plugin_id": "net-probe",
                        "config": { "server_url": meta_url }
                    }
                })),
            )
            .await;
        assert_eq!(
            st,
            StatusCode::FORBIDDEN,
            "cloud metadata {meta_url} must be rejected: {meta_err}"
        );
    }

    // 5. HTTP 302 redirect on `fc.fetch` MUST be rejected with 403 SsrfBlocked
    let (st, ch_redir) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Redirect Target",
                "kind": "plugin",
                "config": {
                    "kind": "plugin",
                    "plugin_id": "net-probe",
                    "config": {
                        "server_url": redirect_server.uri(),
                        "target_path": "/redir"
                    }
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, redir_err) = h
        .req(
            Method::POST,
            &format!("/api/v1/channels/{}/test", ch_redir["id"].as_i64().unwrap()),
            None,
        )
        .await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "HTTP 302 redirect must fail with 403 SsrfBlocked: {redir_err}"
    );

    // 6. `use_proxy: true` is accepted on plugin channels and fails closed when no OUTBOUND_PROXY is configured
    let (st, ch_proxy) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Proxied Plugin",
                "kind": "plugin",
                "use_proxy": true,
                "config": {
                    "kind": "plugin",
                    "plugin_id": "net-probe",
                    "config": { "server_url": lan_server.uri() }
                }
            })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "use_proxy=true should be accepted: {ch_proxy}"
    );
    assert_eq!(ch_proxy["use_proxy"], true);
    let (st, proxy_err) = h
        .req(
            Method::POST,
            &format!("/api/v1/channels/{}/test", ch_proxy["id"].as_i64().unwrap()),
            None,
        )
        .await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "use_proxy without OUTBOUND_PROXY must fail closed with 400: {proxy_err}"
    );
    assert!(
        proxy_err["error"]
            .as_str()
            .unwrap_or("")
            .contains("OUTBOUND_PROXY"),
        "expected OUTBOUND_PROXY message: {proxy_err}"
    );
}

// =============================================================================
// 3. Marketplace 1-Click Install, SHA-256 Verification, Upgrade, Custom Upload & Uninstall Guard
// =============================================================================

#[tokio::test]
async fn marketplace_install_sha256_upgrade_upload_source_and_cascade_delete() {
    let market_srv = MockServer::start().await;

    let plugin_v1 = r#"
        export const meta = {
            id: "demo-chan",
            name: "Demo Channel",
            version: "0.3.1",
            api_version: "1",
            kind: "channel",
            min_core: "0.3.1",
            category: "delivery",
            icon: "bell",
            description: { en: "Demo v1", zh: "演示 v1" },
            allowed_hosts: ["api.demo.test"],
            account_schema: { type: "object", properties: {} },
            destination_schema: { type: "object", properties: {} },
            capabilities: { supports_html: false, max_text_len: 1000 }
        };
        export function deliver() { return { outcome: "delivered" }; }
    "#;
    let plugin_v2 = r#"
        export const meta = {
            id: "demo-chan",
            name: "Demo Channel",
            version: "0.3.2",
            api_version: "1",
            kind: "channel",
            min_core: "0.3.1",
            category: "delivery",
            icon: "bell",
            description: { en: "Demo v2", zh: "演示 v2" },
            allowed_hosts: ["api.demo.test"],
            account_schema: { type: "object", properties: {} },
            destination_schema: { type: "object", properties: {} },
            capabilities: { supports_html: true, max_text_len: 4000 }
        };
        export function deliver() { return { outcome: "delivered" }; }
    "#;
    let sha_v1 = sha256_hex(plugin_v1.as_bytes());
    let sha_v2 = sha256_hex(plugin_v2.as_bytes());

    let index_json = json!({
        "schema_version": 1,
        "generated_at": "2026-11-25T00:00:00Z",
        "plugins": [
            {
                "id": "demo-chan",
                "name": "Demo Channel",
                "version": "0.3.2",
                "latest": "0.3.2",
                "api_version": "1",
                "kind": "channel",
                "runtime": "js",
                "min_core": "0.3.1",
                "category": "delivery",
                "icon": "bell",
                "description": "Demo v2",
                "description_i18n": { "en": "Demo v2", "zh": "演示 v2" },
                "allowed_hosts": ["api.demo.test"],
                "hooks": [],
                "permissions": ["network"],
                "versions": [
                    {
                        "version": "0.3.2",
                        "min_core": "0.3.1",
                        "path": "plugins/channels/demo-chan/0.3.2/plugin.js",
                        "sha256": sha_v2,
                        "changelog": "Added HTML support"
                    },
                    {
                        "version": "0.3.1",
                        "min_core": "0.3.1",
                        "path": "plugins/channels/demo-chan/0.3.1/plugin.js",
                        "sha256": sha_v1,
                        "changelog": "Initial release"
                    }
                ]
            },
            {
                "id": "tampered-chan",
                "name": "Tampered",
                "version": "0.3.1",
                "latest": "0.3.1",
                "api_version": "1",
                "kind": "channel",
                "runtime": "js",
                "min_core": "0.3.1",
                "versions": [
                    {
                        "version": "0.3.1",
                        "min_core": "0.3.1",
                        "path": "plugins/channels/demo-chan/0.3.1/plugin.js",
                        "sha256": "0".repeat(64)
                    }
                ]
            }
        ]
    });

    Mock::given(method("GET"))
        .and(path("/index.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&index_json))
        .mount(&market_srv)
        .await;
    Mock::given(method("GET"))
        .and(path("/plugins/channels/demo-chan/0.3.1/plugin.js"))
        .respond_with(ResponseTemplate::new(200).set_body_string(plugin_v1))
        .mount(&market_srv)
        .await;
    Mock::given(method("GET"))
        .and(path("/plugins/channels/demo-chan/0.3.2/plugin.js"))
        .respond_with(ResponseTemplate::new(200).set_body_string(plugin_v2))
        .mount(&market_srv)
        .await;

    let h = JsHarness::with_options(
        Some(format!("{}/index.json", market_srv.uri())),
        vec!["127.0.0.1/32".to_string()],
    )
    .await;

    // 1. Tampered SHA-256 MUST fail with 400
    let (st, bad_sha) = h
        .req(
            Method::POST,
            "/api/v1/plugins/install",
            Some(json!({ "plugin_id": "tampered-chan" })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "tampered sha256 must fail: {bad_sha}"
    );
    assert!(bad_sha["error"].as_str().unwrap_or("").contains("SHA-256"));

    // 2. Dry-run preview for v0.3.1 returns manifest + allowed_hosts + changelog without persisting
    let (st, preview) = h
        .req(
            Method::POST,
            "/api/v1/plugins/install",
            Some(json!({ "plugin_id": "demo-chan", "version": "0.3.1", "dry_run": true })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "dry_run failed: {preview}");
    assert_eq!(preview["source"], "market");
    assert_eq!(preview["script_sha256"], sha_v1);
    assert_eq!(preview["allowed_hosts"], json!(["api.demo.test"]));
    assert_eq!(preview["already_installed"], false);
    assert_eq!(preview["changelog"], "Initial release");
    assert!(h.db.get_plugin("demo-chan").await.unwrap().is_none());

    // 3. Install v0.3.1 -> 201 Created, source endpoint returns exact JS
    let (st, installed) = h
        .req(
            Method::POST,
            "/api/v1/plugins/install",
            Some(json!({ "plugin_id": "demo-chan", "version": "0.3.1", "enabled": true })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "install failed: {installed}");
    assert_eq!(installed["runtime"], "js");
    assert_eq!(installed["source"], "market");
    assert_eq!(installed["installed_version"], "0.3.1");

    let (st, src_resp) = h
        .req(Method::GET, "/api/v1/plugins/demo-chan/source", None)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(src_resp["script_sha256"], sha_v1);
    assert_eq!(src_resp["script_js"], plugin_v1);

    // 4. Market shows `update_available: true` (0.3.1 -> 0.3.2)
    let (st, market) = h.req(Method::GET, "/api/v1/plugins/market", None).await;
    assert_eq!(st, StatusCode::OK);
    let demo_item = market["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "demo-chan")
        .unwrap();
    assert_eq!(demo_item["installed"], true);
    assert_eq!(demo_item["installed_version"], "0.3.1");
    assert_eq!(demo_item["update_available"], true);

    // Create a channel + route referencing `demo-chan`
    let (st, ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Demo Account",
                "kind": "plugin",
                "config": { "kind": "plugin", "plugin_id": "demo-chan", "config": {} },
                "initial_destination": {
                    "name": "Demo Dest",
                    "config": { "kind": "plugin", "config": {} }
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(ch["capabilities"]["supports_html"], false);
    let ch_id = ch["id"].as_i64().unwrap();
    let dest_id = ch["destinations"][0]["id"].as_i64().unwrap();
    let (feed_id, _) = h
        .seed_feed_and_entry("Feed", None, "<p>Hi</p>", None, None, None)
        .await;
    let (st, _) = h
        .req(
            Method::POST,
            "/api/v1/routes",
            Some(json!({
                "name": "Demo Route",
                "feed_ids": [feed_id],
                "channel_id": ch_id,
                "destination_id": dest_id
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);

    // 5. Upgrade to v0.3.2 via `/install` -> 200 OK + syncs channel `capabilities_json`!
    let (st, upgraded) = h
        .req(
            Method::POST,
            "/api/v1/plugins/install",
            Some(json!({ "plugin_id": "demo-chan", "version": "0.3.2" })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "upgrade failed: {upgraded}");
    assert_eq!(upgraded["installed_version"], "0.3.2");
    let ch_row = h.db.get_channel(ch_id).await.unwrap().unwrap();
    assert_eq!(ch_row.capabilities_value()["supports_html"], true);
    assert_eq!(ch_row.capabilities_value()["max_text_len"], 4000);

    // 6. Custom upload guards: built-in overwrite (409), duplicate without allow_overwrite (409), cross-kind mutation (400)
    let builtin_clash_js = r#"
        export const meta = {
            id: "link-rewrite", name: "Fake", version: "1.0.0", api_version: "1", kind: "channel"
        };
        export function deliver() { return { outcome: "delivered" }; }
    "#;
    let (st, _) = h
        .req(
            Method::POST,
            "/api/v1/plugins/upload",
            Some(json!({ "script_js": builtin_clash_js, "allow_overwrite": true })),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT);

    let (st, _) = h
        .req(
            Method::POST,
            "/api/v1/plugins/upload",
            Some(json!({ "script_js": plugin_v2, "allow_overwrite": false })),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT);

    let cross_kind_js = r#"
        export const meta = {
            id: "demo-chan", name: "Now Proc", version: "0.4.0", api_version: "1",
            kind: "processor", hooks: ["on_render"]
        };
        export function onRender() { return { outcome: "continue" }; }
    "#;
    let (st, cross_err) = h
        .req(
            Method::POST,
            "/api/v1/plugins/upload",
            Some(json!({ "script_js": cross_kind_js, "allow_overwrite": true })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "cross-kind must fail: {cross_err}"
    );

    // 7. Uninstall guard: 409 Conflict with references, then ?cascade=true succeeds!
    let (st, conflict) = h
        .req(Method::DELETE, "/api/v1/plugins/demo-chan", None)
        .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(
        conflict["referencing_channels"].as_array().unwrap().len(),
        1
    );
    assert_eq!(conflict["referencing_routes"].as_array().unwrap().len(), 1);

    let (st, _) = h
        .req(
            Method::DELETE,
            "/api/v1/plugins/demo-chan?cascade=true",
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert!(h.dispatcher.plugins().get_js("demo-chan").is_none());
    assert!(h.db.get_channel(ch_id).await.unwrap().is_none());
}

// =============================================================================
// 4. Real Official `ntfy` & `bark` Single-File JS Plugins End-to-End
// =============================================================================

#[tokio::test]
async fn real_ntfy_and_bark_plugins_end_to_end_delivery_redaction_and_retry_mapping() {
    let h = JsHarness::new().await;

    let ntfy_js = include_str!("fixtures/plugins/ntfy_0_3_1.js");
    let bark_js = include_str!("fixtures/plugins/bark_0_3_1.js");

    // Verify embedded test fixtures match the exact SHA-256 digests recorded in market_snapshot.json.
    let snapshot: Value =
        serde_json::from_str(feedcourier::plugins::MARKET_SNAPSHOT_JSON).expect("snapshot json");
    for (plugin_id, script) in [("ntfy", ntfy_js), ("bark", bark_js)] {
        let expected_sha = snapshot["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == plugin_id)
            .and_then(|p| p["versions"].as_array())
            .and_then(|vs| vs.iter().find(|v| v["version"] == "0.3.1"))
            .and_then(|v| v["sha256"].as_str())
            .expect("snapshot version sha256");
        assert_eq!(
            sha256_hex(script.as_bytes()),
            expected_sha,
            "fixture for '{plugin_id}' drifted from market_snapshot.json"
        );
    }

    // Upload both real official plugins into FeedCourier
    for script in [ntfy_js, bark_js] {
        let (st, resp) = h
            .req(
                Method::POST,
                "/api/v1/plugins/upload",
                Some(json!({ "script_js": script, "enabled": true })),
            )
            .await;
        assert_eq!(st, StatusCode::CREATED, "upload failed: {resp}");
    }

    // ---- A. ntfy end-to-end ----
    type CapturedNtfy = (String, String, String, String);
    let ntfy_srv = MockServer::start().await;
    let ntfy_captured: Arc<Mutex<Vec<CapturedNtfy>>> = Arc::new(Mutex::new(Vec::new()));
    let ntfy_cap_clone = Arc::clone(&ntfy_captured);

    Mock::given(method("GET"))
        .and(path("/v1/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "healthy": true })))
        .mount(&ntfy_srv)
        .await;
    Mock::given(method("POST"))
        .and(path("/rss-alerts"))
        .respond_with(move |req: &MockRequest| {
            let auth = req
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let title = req
                .headers
                .get("title")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let click = req
                .headers
                .get("click")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let body = String::from_utf8_lossy(&req.body).to_string();
            ntfy_cap_clone
                .lock()
                .unwrap()
                .push((auth, title, click, body));
            ResponseTemplate::new(200).set_body_json(json!({ "id": "ntfy_msg_001" }))
        })
        .mount(&ntfy_srv)
        .await;

    let (st, ntfy_ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "My ntfy",
                "kind": "plugin",
                "min_interval_secs": 0,
                "jitter_max_secs": 0,
                "config": {
                    "kind": "plugin",
                    "plugin_id": "ntfy",
                    "config": {
                        "server_url": ntfy_srv.uri(),
                        "access_token": "tk_ntfy_secret_99"
                    }
                },
                "initial_destination": {
                    "name": "Alerts",
                    "config": {
                        "kind": "plugin",
                        "config": { "topic": "rss-alerts", "priority": "high", "tags": "newspaper" }
                    }
                }
            })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "create ntfy channel failed: {ntfy_ch}"
    );
    // Secret redaction check
    let ch_json_str = serde_json::to_string(&ntfy_ch).unwrap();
    assert!(
        !ch_json_str.contains("tk_ntfy_secret_99"),
        "ntfy access_token leaked!"
    );
    assert_eq!(ntfy_ch["config"]["config"]["has_access_token"], true);

    let ntfy_ch_id = ntfy_ch["id"].as_i64().unwrap();
    let ntfy_dest_id = ntfy_ch["destinations"][0]["id"].as_i64().unwrap();

    let (st, ntfy_test) = h
        .req(
            Method::POST,
            &format!("/api/v1/channels/{ntfy_ch_id}/test"),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "ntfy account test failed: {ntfy_test}");

    let long_body = "汉字测试摘要内容 ".repeat(600);
    let (feed_id, _) = h
        .seed_feed_and_entry(
            "深度解读：FeedCourier v0.3.1 发布",
            Some(&long_body),
            &format!("<p>{long_body}</p>"),
            Some("Ellery"),
            Some(1_764_000_000),
            Some("https://example.com/favicon.png"),
        )
        .await;
    let (st, ntfy_route) = h
        .req(
            Method::POST,
            "/api/v1/routes",
            Some(json!({
                "name": "ntfy route",
                "feed_ids": [feed_id],
                "channel_id": ntfy_ch_id,
                "destination_id": ntfy_dest_id
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let ntfy_route_id = ntfy_route["id"].as_i64().unwrap();
    let entries = h.db.list_entries_for_feed(feed_id, 10).await.unwrap();
    let ntfy_task =
        h.db.enqueue_delivery_task(
            ntfy_route_id,
            ntfy_ch_id,
            ntfy_dest_id,
            entries[0].id,
            now_unix(),
        )
        .await
        .unwrap()
        .unwrap()
        .id;
    h.dispatcher.drain_due_outbox_tasks(10).await.unwrap();
    let task_done = h.db.get_delivery_task(ntfy_task).await.unwrap().unwrap();
    assert_eq!(
        task_done.status,
        DeliveryTaskStatus::Delivered,
        "ntfy diag: {:?}",
        task_done.diagnostic_log
    );
    {
        let caps = ntfy_captured.lock().unwrap();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].0, "Bearer tk_ntfy_secret_99");
        assert!(
            caps[0].1.starts_with("=?UTF-8?B?"),
            "non-ASCII title must use RFC 2047: {}",
            caps[0].1
        );
        assert_eq!(caps[0].2, "https://example.com/articles/42");
        assert!(
            caps[0].3.len() <= 4096,
            "ntfy body exceeded 4096 bytes: {}",
            caps[0].3.len()
        );
    }

    // ntfy HTTP 429 -> Retrying + credential group cooldown; HTTP 403 -> DeadLetter
    ntfy_srv.reset().await;
    Mock::given(method("POST"))
        .and(path("/rss-alerts"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "45"))
        .mount(&ntfy_srv)
        .await;
    let (_, entry2) = h
        .seed_feed_and_entry("Rate Limit", Some("body"), "<p>body</p>", None, None, None)
        .await;
    let t_429 =
        h.db.enqueue_delivery_task(ntfy_route_id, ntfy_ch_id, ntfy_dest_id, entry2, now_unix())
            .await
            .unwrap()
            .unwrap()
            .id;
    h.dispatcher.drain_due_outbox_tasks(10).await.unwrap();
    let t_429_row = h.db.get_delivery_task(t_429).await.unwrap().unwrap();
    assert_eq!(t_429_row.status, DeliveryTaskStatus::Retrying);

    ntfy_srv.reset().await;
    Mock::given(method("POST"))
        .and(path("/rss-alerts"))
        .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
        .mount(&ntfy_srv)
        .await;
    // Clear channel cooldown so immediate dispatch runs
    sqlx::query("UPDATE channels SET cooldown_until = NULL WHERE id = ?1")
        .bind(ntfy_ch_id)
        .execute(h.db.write_pool())
        .await
        .unwrap();
    let (_, entry3) = h
        .seed_feed_and_entry("Forbidden", Some("body"), "<p>body</p>", None, None, None)
        .await;
    let t_403 =
        h.db.enqueue_delivery_task(ntfy_route_id, ntfy_ch_id, ntfy_dest_id, entry3, now_unix())
            .await
            .unwrap()
            .unwrap()
            .id;
    h.dispatcher.drain_due_outbox_tasks(10).await.unwrap();
    let t_403_row = h.db.get_delivery_task(t_403).await.unwrap().unwrap();
    assert_eq!(t_403_row.status, DeliveryTaskStatus::DeadLetter);

    // ---- B. Bark end-to-end (Telegram Compact Card / 摘要模式) ----
    let bark_srv = MockServer::start().await;
    let bark_payloads: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
    let bark_cap = Arc::clone(&bark_payloads);

    Mock::given(method("GET"))
        .and(path("/ping"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "code": 200, "message": "pong" })),
        )
        .mount(&bark_srv)
        .await;
    Mock::given(method("POST"))
        .and(path("/push"))
        .respond_with(move |req: &MockRequest| {
            let auth = req
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            bark_cap.lock().unwrap().push((auth, body));
            ResponseTemplate::new(200).set_body_json(json!({ "code": 200, "message": "success" }))
        })
        .mount(&bark_srv)
        .await;

    let (st, bark_ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "My Bark",
                "kind": "plugin",
                "min_interval_secs": 0,
                "jitter_max_secs": 0,
                "config": {
                    "kind": "plugin",
                    "plugin_id": "bark",
                    "config": {
                        "server_url": bark_srv.uri(),
                        "username": "alice",
                        "password": "bark_secret_password"
                    }
                },
                "initial_destination": {
                    "name": "iPhone 16",
                    "config": {
                        "kind": "plugin",
                        "config": {
                            "device_key": "bark_device_key_secret_88",
                            "level": "timeSensitive",
                            "send_icon": true
                        }
                    }
                }
            })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "create bark channel failed: {bark_ch}"
    );
    let bark_json_str = serde_json::to_string(&bark_ch).unwrap();
    assert!(
        !bark_json_str.contains("bark_secret_password"),
        "bark password leaked!"
    );
    assert!(
        !bark_json_str.contains("bark_device_key_secret_88"),
        "bark device_key leaked!"
    );
    assert_eq!(bark_ch["config"]["config"]["has_password"], true);
    assert_eq!(
        bark_ch["destinations"][0]["config"]["config"]["has_device_key"],
        true
    );

    let bark_ch_id = bark_ch["id"].as_i64().unwrap();
    let bark_dest_id = bark_ch["destinations"][0]["id"].as_i64().unwrap();

    // Account test (`GET /ping`) & Destination test (`POST /push`)
    let (st, t_acct) = h
        .req(
            Method::POST,
            &format!("/api/v1/channels/{bark_ch_id}/test"),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "bark account test failed: {t_acct}");
    let (st, t_dest) = h
        .req(
            Method::POST,
            &format!("/api/v1/destinations/{bark_dest_id}/test"),
            Some(json!({ "locale": "zh-CN" })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "bark dest test failed: {t_dest}");
    bark_payloads.lock().unwrap().clear();

    // Real article delivery verifying Telegram Compact Card layout
    let (bark_feed_id, bark_entry_id) = h
        .seed_feed_and_entry(
            "Rust 2026 Roadmap",
            Some("Rust announces faster incremental builds and async improvements."),
            "<p>Rust announces faster incremental builds and async improvements.</p>",
            Some("Jane Doe"),
            Some(1_764_000_000),
            Some("https://example.com/icon.png"),
        )
        .await;
    let (st, bark_route) = h
        .req(
            Method::POST,
            "/api/v1/routes",
            Some(json!({
                "name": "Bark Route",
                "feed_ids": [bark_feed_id],
                "channel_id": bark_ch_id,
                "destination_id": bark_dest_id,
                "options": { "timezone": "UTC" }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let bark_route_id = bark_route["id"].as_i64().unwrap();
    let bark_task =
        h.db.enqueue_delivery_task(
            bark_route_id,
            bark_ch_id,
            bark_dest_id,
            bark_entry_id,
            now_unix(),
        )
        .await
        .unwrap()
        .unwrap()
        .id;
    h.dispatcher.drain_due_outbox_tasks(10).await.unwrap();
    let bark_done = h.db.get_delivery_task(bark_task).await.unwrap().unwrap();
    assert_eq!(
        bark_done.status,
        DeliveryTaskStatus::Delivered,
        "bark diag: {:?}",
        bark_done.diagnostic_log
    );

    {
        let payloads = bark_payloads.lock().unwrap();
        assert_eq!(payloads.len(), 1);
        assert!(
            payloads[0].0.starts_with("Basic "),
            "expected Basic Auth header"
        );
        let body = &payloads[0].1;
        assert_eq!(body["device_key"], "bark_device_key_secret_88");
        assert_eq!(body["title"], "Rust 2026 Roadmap");
        assert_eq!(body["url"], "https://example.com/articles/42");
        assert_eq!(body["icon"], "https://example.com/icon.png");
        assert_eq!(body["level"], "timeSensitive");
        let text = body["body"].as_str().unwrap();
        assert!(
            text.contains("faster incremental builds"),
            "missing summary: {text}"
        );
        assert!(
            text.contains("— Tech Daily · Jane Doe · "),
            "missing Compact Card footer: {text}"
        );
    }
}

// =============================================================================
// 5. JS Processor (`onRender` ammonia + `plugin_order` + `onDelivered` Queue & Cancel)
// =============================================================================

#[tokio::test]
async fn js_processor_on_render_ammonia_plugin_order_and_on_delivered_queue() {
    let h = JsHarness::new().await;
    let sink_srv = MockServer::start().await;
    let delivered_html = Arc::new(Mutex::new(String::new()));
    let delivered_html_c = Arc::clone(&delivered_html);
    let on_delivered_hits = Arc::new(Mutex::new(0usize));
    let on_delivered_hits_c = Arc::clone(&on_delivered_hits);

    Mock::given(method("POST"))
        .and(path("/deliver"))
        .respond_with(move |req: &MockRequest| {
            let v: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            *delivered_html_c.lock().unwrap() = v["html"].as_str().unwrap_or("").to_string();
            ResponseTemplate::new(200).set_body_string("ok")
        })
        .mount(&sink_srv)
        .await;
    Mock::given(method("POST"))
        .and(path("/audit"))
        .respond_with(move |_: &MockRequest| {
            let mut n = on_delivered_hits_c.lock().unwrap();
            *n += 1;
            if *n == 1 {
                ResponseTemplate::new(500).set_body_string("transient")
            } else {
                ResponseTemplate::new(200).set_body_string("recorded")
            }
        })
        .mount(&sink_srv)
        .await;

    let chan_js = r#"
        export const meta = {
            id: "capture-chan",
            name: "Capture",
            version: "0.1.0",
            api_version: "1",
            kind: "channel",
            account_schema: {
                type: "object",
                required: ["sink_url"],
                properties: { sink_url: { type: "string", format: "uri" } }
            },
            destination_schema: { type: "object", properties: {} }
        };
        export function deliver(ctx) {
            const res = fc.fetch({
                method: "POST",
                url: ctx.account.sink_url + "/deliver",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({ html: ctx.entry.content_html })
            });
            return { outcome: res.ok ? "delivered" : "retry" };
        }
    "#;
    let proc_a_js = r#"
        export const meta = {
            id: "proc-a",
            name: "Proc A",
            version: "0.1.0",
            api_version: "1",
            kind: "processor",
            hooks: ["on_render", "on_delivered"],
            instance_schema: {
                type: "object",
                properties: { audit_url: { type: "string", format: "uri" } }
            },
            route_option_schema: { type: "object", properties: {} }
        };
        export function onRender(ctx) {
            return {
                outcome: "continue",
                content_html: (ctx.entry.content_html || "") + "<p>[A]</p><script>alert(1)</script>"
            };
        }
        export function onDelivered(ctx) {
            if (ctx.payload && ctx.payload.route && ctx.payload.route.id) {
                const res = fc.fetch({
                    method: "POST",
                    url: globalThis.__audit_url || ctx.payload.audit_url || "",
                    body: JSON.stringify(ctx)
                });
                if (!res.ok) throw new Error("audit failed " + res.status);
            }
        }
    "#;
    let proc_a_with_url = proc_a_js.replace(
        "globalThis.__audit_url || ctx.payload.audit_url || \"\"",
        &format!("\"{}/audit\"", sink_srv.uri()),
    );
    let proc_b_js = r#"
        export const meta = {
            id: "proc-b",
            name: "Proc B",
            version: "0.1.0",
            api_version: "1",
            kind: "processor",
            hooks: ["on_render"],
            route_option_schema: { type: "object", properties: {} }
        };
        export function onRender(ctx) {
            return {
                outcome: "continue",
                content_html: (ctx.entry.content_html || "") + "<p>[B]</p>"
            };
        }
    "#;

    for js in [chan_js, &proc_a_with_url, proc_b_js] {
        let (st, resp) = h
            .req(
                Method::POST,
                "/api/v1/plugins/upload",
                Some(json!({ "script_js": js, "enabled": true })),
            )
            .await;
        assert_eq!(st, StatusCode::CREATED, "upload failed: {resp}");
    }

    // Create an instance on `proc-a` carrying `audit_url` so `sink_srv` is an admin-configured URI origin
    let (st, inst) = h
        .req(
            Method::POST,
            "/api/v1/plugins/proc-a/instances",
            Some(json!({
                "name": "Audit Sink",
                "config": { "audit_url": sink_srv.uri() },
                "enabled": true
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "create instance failed: {inst}");
    let inst_id = inst["id"].as_i64().unwrap();

    let (st, ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Capture Ch",
                "kind": "plugin",
                "config": {
                    "kind": "plugin",
                    "plugin_id": "capture-chan",
                    "config": { "sink_url": sink_srv.uri() }
                },
                "initial_destination": {
                    "name": "Dest",
                    "config": { "kind": "plugin", "config": {} }
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let ch_id = ch["id"].as_i64().unwrap();
    let dest_id = ch["destinations"][0]["id"].as_i64().unwrap();

    let (feed_id, entry_id) = h
        .seed_feed_and_entry("Order Test", None, "<p>Base</p>", None, None, None)
        .await;
    let (st, route) = h
        .req(
            Method::POST,
            "/api/v1/routes",
            Some(json!({
                "name": "Ordered Route",
                "feed_ids": [feed_id],
                "channel_id": ch_id,
                "destination_id": dest_id,
                "options": {
                    "plugin_order": ["proc-b", "proc-a"],
                    "plugins": { "proc-a": { "provider_id": inst_id }, "proc-b": {} }
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "route create failed: {route}");
    let route_id = route["id"].as_i64().unwrap();

    let task_id =
        h.db.enqueue_delivery_task(route_id, ch_id, dest_id, entry_id, now_unix())
            .await
            .unwrap()
            .unwrap()
            .id;
    h.dispatcher.drain_due_outbox_tasks(10).await.unwrap();
    assert_eq!(
        h.db.get_delivery_task(task_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        DeliveryTaskStatus::Delivered
    );

    // Verify `plugin_order` ran proc-b BEFORE proc-a AND ammonia stripped `<script>`!
    let html = delivered_html.lock().unwrap().clone();
    assert!(
        !html.contains("<script>"),
        "ammonia MUST strip <script>: {html}"
    );
    let pos_b = html.find("[B]").expect("missing [B]");
    let pos_a = html.find("[A]").expect("missing [A]");
    assert!(pos_b < pos_a, "expected [B] before [A], got: {html}");

    // Verify durable `plugin_events`: 1st attempt returns 500 -> rescheduled; 2nd attempt succeeds & deletes row!
    assert_eq!(
        h.dispatcher
            .process_due_plugin_events_once(10)
            .await
            .unwrap(),
        1
    );
    assert_eq!(*on_delivered_hits.lock().unwrap(), 1);
    // Backdate `next_attempt_at` to now so retry executes immediately
    sqlx::query("UPDATE plugin_events SET next_attempt_at = 0")
        .execute(h.db.write_pool())
        .await
        .unwrap();
    assert_eq!(
        h.dispatcher
            .process_due_plugin_events_once(10)
            .await
            .unwrap(),
        1
    );
    assert_eq!(*on_delivered_hits.lock().unwrap(), 2);
    let remaining =
        h.db.list_due_plugin_events(now_unix() + 9999, 10)
            .await
            .unwrap();
    assert!(
        remaining.is_empty(),
        "event row should be deleted after success"
    );

    // Verify shutdown cancellation aborts immediately without processing
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let started = Instant::now();
    let count = h
        .dispatcher
        .process_due_plugin_events_cancellable(10, &cancelled)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(started.elapsed() < Duration::from_millis(200));
}

// =============================================================================
// 6. Migration `0004_js_plugins.sql` Watermark & Incompatible Persisted Plugin Error
// =============================================================================

#[tokio::test]
async fn migration_0004_preserves_channels_sequence_and_incompatible_plugin_error() {
    // Part A: `sqlite_sequence` preservation across `0004_js_plugins.sql`
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("mig.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&db_path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../migrations/0001_init.sql"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../migrations/0002_channel_destinations.sql"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/0003_plugins_and_smtp_compliance.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();

    for i in 1..=3 {
        sqlx::query(
            "INSERT INTO channels (name, kind, encrypted_config, credential_group_key) VALUES (?1, 'smtp', X'00', ?2)",
        )
        .bind(format!("Ch {i}"))
        .bind(format!("grp-{i}"))
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query("DELETE FROM channels WHERE id IN (2, 3)")
        .execute(&pool)
        .await
        .unwrap();

    sqlx::raw_sql(include_str!("../migrations/0004_js_plugins.sql"))
        .execute(&pool)
        .await
        .unwrap();

    let res = sqlx::query(
        "INSERT INTO channels (name, kind, encrypted_config, credential_group_key) VALUES ('Ch New', 'plugin', X'00', 'grp-new')",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        res.last_insert_rowid(),
        4,
        "0004_js_plugins.sql MUST preserve channels sqlite_sequence watermark"
    );

    // Part B: Incompatible persisted plugin reports incompatibility reason rather than "disabled"
    let h = JsHarness::new().await;
    let ok_js = r#"
        export const meta = {
            id: "future-chan",
            name: "Future Chan",
            version: "0.3.1",
            api_version: "1",
            kind: "channel",
            account_schema: { type: "object", properties: {} },
            destination_schema: { type: "object", properties: {} }
        };
        export function deliver() { return { outcome: "delivered" }; }
    "#;
    let (st, _) = h
        .req(
            Method::POST,
            "/api/v1/plugins/upload",
            Some(json!({ "script_js": ok_js, "enabled": true })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);

    // Simulate a persisted plugin requiring FeedCourier >= 99.0.0
    let mut persisted_manifest: Value = serde_json::from_str(
        h.db.get_plugin("future-chan")
            .await
            .unwrap()
            .unwrap()
            .manifest_json
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    persisted_manifest["min_core"] = json!("99.0.0");
    sqlx::query("UPDATE plugins SET manifest_json = ?1 WHERE id = 'future-chan'")
        .bind(persisted_manifest.to_string())
        .execute(h.db.write_pool())
        .await
        .unwrap();
    h.dispatcher.plugins().reload_from_db(&h.db).await.unwrap();

    let (st, err) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Future Acct",
                "kind": "plugin",
                "config": { "kind": "plugin", "plugin_id": "future-chan", "config": {} }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let msg = err["error"].as_str().unwrap_or("");
    assert!(
        msg.contains("incompatible") && msg.contains("99.0.0"),
        "expected incompatibility reason, got: {msg}"
    );

    // Also verify `JsChannelSender::send_test` reports incompatibility instead of disabled
    let sender = JsChannelSender::new(h.dispatcher.plugins().clone(), h.dispatcher.http().clone());
    let test_err = sender
        .send_test(
            &ChannelConfig::Plugin(PluginChannelConfig {
                plugin_id: "future-chan".to_string(),
                config: json!({}),
            }),
            &DestinationConfig::Plugin(PluginDestinationConfig { config: json!({}) }),
            false,
        )
        .await
        .expect_err("incompatible plugin send_test must fail");
    assert!(
        test_err.to_string().contains("incompatible") && test_err.to_string().contains("99.0.0"),
        "unexpected send_test error: {test_err}"
    );
}

// =============================================================================
// 7. Round-3 QA Regression Suite (P0-2, P0-2b, P0-3, P1-1, P1-2, P1-5, P2-1, P2-3, P2-5)
// =============================================================================

#[tokio::test]
async fn round3_schema_default_and_validate_cannot_escalate_uri_origins() {
    let probe_srv = MockServer::start().await;
    let probe_hits = Arc::new(Mutex::new(0usize));
    let hits_clone = Arc::clone(&probe_hits);
    Mock::given(method("GET"))
        .and(path("/secret-internal"))
        .respond_with(move |_req: &MockRequest| {
            *hits_clone.lock().unwrap() += 1;
            ResponseTemplate::new(200).set_body_string("LEAKED")
        })
        .mount(&probe_srv)
        .await;

    let h = JsHarness::new().await;

    // P0-2: Plugin tries to smuggle `127.0.0.1` via hidden `default` on a `format: "uri"` field
    // with `allowed_hosts: []`. Admin submits empty config `{}` — `fc.fetch` MUST be SSRF-blocked.
    let sneaky_default_js = format!(
        r#"
        export const meta = {{
            id: "sneaky-default",
            name: "Sneaky Default",
            version: "0.1.0",
            api_version: "1",
            kind: "channel",
            allowed_hosts: [],
            account_schema: {{
                type: "object",
                properties: {{
                    probe_url: {{ type: "string", format: "uri", default: "{uri}" }}
                }}
            }},
            destination_schema: {{ type: "object", properties: {{}} }}
        }};
        export function deliver() {{
            const resp = fc.fetch({{ method: "GET", url: "{uri}/secret-internal" }});
            return {{ outcome: "delivered", diagnostic: resp.body }};
        }}
        "#,
        uri = probe_srv.uri()
    );
    let (st, resp) = h
        .req(
            Method::POST,
            "/api/v1/plugins/upload",
            Some(json!({ "script_js": sneaky_default_js, "enabled": true })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "upload failed: {resp}");

    let (st, ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Sneaky Ch",
                "kind": "plugin",
                "config": { "kind": "plugin", "plugin_id": "sneaky-default", "config": {} },
                "initial_destination": {
                    "name": "Dest",
                    "config": { "kind": "plugin", "config": {} }
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "channel create failed: {ch}");
    // Saved account config must NOT contain the auto-filled `probe_url` default!
    assert!(
        ch["config"]["config"].get("probe_url").is_none(),
        "URI default must never be auto-persisted: {ch}"
    );
    let ch_id = ch["id"].as_i64().unwrap();
    let dest_id = ch["destinations"][0]["id"].as_i64().unwrap();
    let (feed_id, entry_id) = h
        .seed_feed_and_entry("SSRF Check", None, "<p>Body</p>", None, None, None)
        .await;
    let (st, route) = h
        .req(
            Method::POST,
            "/api/v1/routes",
            Some(json!({
                "name": "Sneaky Route",
                "feed_ids": [feed_id],
                "channel_id": ch_id,
                "destination_id": dest_id
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let task_id =
        h.db.enqueue_delivery_task(
            route["id"].as_i64().unwrap(),
            ch_id,
            dest_id,
            entry_id,
            now_unix(),
        )
        .await
        .unwrap()
        .unwrap()
        .id;
    h.dispatcher.drain_due_outbox_tasks(10).await.unwrap();
    let task = h.db.get_delivery_task(task_id).await.unwrap().unwrap();
    assert_eq!(
        task.status,
        DeliveryTaskStatus::DeadLetter,
        "hidden default URI SSRF must dead-letter: {task:?}"
    );
    assert_eq!(
        *probe_hits.lock().unwrap(),
        0,
        "hidden default URI MUST NOT reach internal server"
    );

    // P0-2b: Plugin `validate()` attempts to inject a new URI field or mutate an existing URI origin.
    let validate_escalate_js = format!(
        r#"
        export const meta = {{
            id: "validate-escalate",
            name: "Validate Escalate",
            version: "0.1.0",
            api_version: "1",
            kind: "channel",
            account_schema: {{
                type: "object",
                properties: {{
                    server_url: {{ type: "string", format: "uri" }},
                    extra_url: {{ type: "string", format: "uri" }}
                }}
            }},
            destination_schema: {{ type: "object", properties: {{}} }}
        }};
        export function validate(ctx) {{
            if (ctx.scope === "account") {{
                if (ctx.config.server_url === "https://ok.example.com/path/") {{
                    return {{ ok: true, normalized_config: {{ server_url: "https://ok.example.com/path" }} }};
                }}
                if (ctx.config.server_url === "https://swap.example.com") {{
                    return {{ ok: true, normalized_config: {{ server_url: "{uri}" }} }};
                }}
                return {{ ok: true, normalized_config: {{ ...ctx.config, extra_url: "{uri}" }} }};
            }}
            return {{ ok: true }};
        }}
        export function deliver() {{ return {{ outcome: "delivered" }}; }}
        "#,
        uri = probe_srv.uri()
    );
    let (st, _) = h
        .req(
            Method::POST,
            "/api/v1/plugins/upload",
            Some(json!({ "script_js": validate_escalate_js, "enabled": true })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);

    // 1) Introducing a brand-new URI field in `normalized_config` is rejected.
    let (st, err1) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Inject Test",
                "kind": "plugin",
                "config": { "kind": "plugin", "plugin_id": "validate-escalate", "config": {} }
            })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "should reject URI injection: {err1}"
    );

    // 2) Mutating an existing URI origin `(scheme, host, port)` is rejected.
    let (st, err2) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Swap Test",
                "kind": "plugin",
                "config": {
                    "kind": "plugin",
                    "plugin_id": "validate-escalate",
                    "config": { "server_url": "https://swap.example.com" }
                }
            })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "should reject URI origin swap: {err2}"
    );

    // 3) Trailing-slash normalization preserving exact origin succeeds!
    let (st, ok_ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Trim Slash OK",
                "kind": "plugin",
                "config": {
                    "kind": "plugin",
                    "plugin_id": "validate-escalate",
                    "config": { "server_url": "https://ok.example.com/path/" }
                }
            })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "same-origin normalization must pass: {ok_ch}"
    );
    assert_eq!(
        ok_ch["config"]["config"]["server_url"],
        "https://ok.example.com/path"
    );
}

#[tokio::test]
async fn round3_bark_default_destination_name_never_leaks_device_key_and_redacts_when_unloaded() {
    let h = JsHarness::new().await;
    let bark_js = include_str!("fixtures/plugins/bark_0_3_1.js");
    let (st, _) = h
        .req(
            Method::POST,
            "/api/v1/plugins/upload",
            Some(json!({ "script_js": bark_js, "enabled": true })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);

    let secret_device_key = "BarkSecretKey_9988776655";
    let (st, ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Bark Account",
                "kind": "plugin",
                "config": {
                    "kind": "plugin",
                    "plugin_id": "bark",
                    "config": { "server_url": "https://api.day.app" }
                },
                "initial_destination": {
                    "config": {
                        "kind": "plugin",
                        "config": { "device_key": secret_device_key }
                    }
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "bark create failed: {ch}");
    let dest = &ch["destinations"][0];
    let dest_name = dest["name"].as_str().unwrap_or("");
    assert!(
        !dest_name.contains(secret_device_key),
        "Bark device_key MUST NOT leak into default destination name: {dest_name}"
    );
    assert_eq!(
        dest_name, "Bark Push Target",
        "Bark destination without group should fall back to '<Plugin> Target'"
    );
    assert!(dest["config"]["config"].get("device_key").is_none());
    assert_eq!(dest["config"]["config"]["has_device_key"], true);

    // Corrupt persisted `script_js` & reload so runtime fails to load; `GET /api/v1/channels`
    // must STILL recover `destination_schema` from `manifest_json` and redact `device_key`!
    sqlx::query("UPDATE plugins SET script_js = 'export const broken = {{{' WHERE id = 'bark'")
        .execute(h.db.write_pool())
        .await
        .unwrap();
    h.dispatcher.plugins().reload_from_db(&h.db).await.unwrap();

    let (st, list) = h.req(Method::GET, "/api/v1/channels", None).await;
    assert_eq!(st, StatusCode::OK);
    let dumped = list.to_string();
    assert!(
        !dumped.contains(secret_device_key),
        "device_key leaked after plugin failed to load: {dumped}"
    );
}

#[tokio::test]
async fn round3_proxy_plugin_http_blocks_cloud_metadata_literal_and_local_dns() {
    // 1. Local-DNS SOCKS5 proxy + custom DNS resolving evil hostname to 169.254.169.254.
    let lookup: feedcourier::net::ssrf::CustomDnsLookup = Arc::new(|host| {
        Box::pin(async move {
            if host == "imds-rebind.internal" {
                Ok(vec![std::net::IpAddr::V4(std::net::Ipv4Addr::new(
                    169, 254, 169, 254,
                ))])
            } else {
                Ok(vec![std::net::IpAddr::V4(std::net::Ipv4Addr::new(
                    10, 0, 0, 5,
                ))])
            }
        })
    });
    let socks_client = ProtectedHttpClient::with_custom_dns(
        NetSettings {
            outbound_proxy_url: Some("socks5://127.0.0.1:1080".to_string()),
            ssrf_allowlist: vec![],
        },
        lookup,
    )
    .unwrap();

    for url in [
        "http://169.254.169.254/latest/meta-data/",
        "http://169.254.170.2/v2/credentials",
        "http://100.100.100.200/latest/meta-data/",
        "http://[::ffff:169.254.169.254]/latest/",
        "http://metadata.google.internal/computeMetadata/v1/",
        "http://imds-rebind.internal/latest/meta-data/",
    ] {
        let err = socks_client
            .send_plugin_http(feedcourier::net::PluginHttpRequest {
                method: reqwest::Method::GET,
                url: url.to_string(),
                headers: reqwest::header::HeaderMap::new(),
                body: None,
                timeout: Duration::from_secs(2),
                max_bytes: 1024,
                allow_private_origin: true,
                use_proxy: true,
            })
            .await
            .expect_err(&format!("proxied metadata URL '{url}' must be blocked"));
        assert!(
            matches!(err, feedcourier::error::AppError::SsrfBlocked(_)),
            "expected SsrfBlocked for '{url}', got: {err:?}"
        );
    }

    // 2. Remote-DNS HTTP proxy still unconditionally blocks literal metadata IPs & hostnames.
    let http_proxy_client = ProtectedHttpClient::new(NetSettings {
        outbound_proxy_url: Some("http://127.0.0.1:8888".to_string()),
        ssrf_allowlist: vec![],
    })
    .unwrap();
    let err = http_proxy_client
        .send_plugin_http(feedcourier::net::PluginHttpRequest {
            method: reqwest::Method::GET,
            url: "http://169.254.169.254/latest/meta-data/".to_string(),
            headers: reqwest::header::HeaderMap::new(),
            body: None,
            timeout: Duration::from_secs(2),
            max_bytes: 1024,
            allow_private_origin: true,
            use_proxy: true,
        })
        .await
        .expect_err("HTTP-proxied literal IMDS must be blocked");
    assert!(matches!(err, feedcourier::error::AppError::SsrfBlocked(_)));
}

#[tokio::test]
async fn round3_timeout_cancels_subsequent_fetches_and_trips_circuit_breaker() {
    let slow_srv = MockServer::start().await;
    let after_timeout_hits = Arc::new(Mutex::new(0usize));
    let after_clone = Arc::clone(&after_timeout_hits);

    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(800)))
        .mount(&slow_srv)
        .await;
    Mock::given(method("GET"))
        .and(path("/after-timeout"))
        .respond_with(move |_req: &MockRequest| {
            *after_clone.lock().unwrap() += 1;
            ResponseTemplate::new(200).set_body_string("GHOST")
        })
        .mount(&slow_srv)
        .await;

    let h = JsHarness::new().await;
    h.dispatcher
        .plugins()
        .set_hook_timeout_override(Some(Duration::from_millis(250)));

    let slow_chan_js = r#"
        export const meta = {
            id: "slow-chan",
            name: "Slow Chan",
            version: "0.1.0",
            api_version: "1",
            kind: "channel",
            account_schema: {
                type: "object",
                properties: { base_url: { type: "string", format: "uri" } }
            },
            destination_schema: { type: "object", properties: {} }
        };
        export function deliver(ctx) {
            try {
                fc.fetch({ method: "GET", url: ctx.account.base_url + "/slow", timeoutMs: 5000 });
            } catch (_e) {
                // Attempt a ghost fetch after the deadline expired; host must reject it immediately!
                fc.fetch({ method: "GET", url: ctx.account.base_url + "/after-timeout", timeoutMs: 5000 });
            }
            return { outcome: "delivered" };
        }
    "#;
    let (st, _) = h
        .req(
            Method::POST,
            "/api/v1/plugins/upload",
            Some(json!({ "script_js": slow_chan_js, "enabled": true })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);

    let (st, ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Slow Ch",
                "kind": "plugin",
                "config": {
                    "kind": "plugin",
                    "plugin_id": "slow-chan",
                    "config": { "base_url": slow_srv.uri() }
                },
                "min_interval_secs": 0,
                "jitter_max_secs": 0,
                "initial_destination": {
                    "name": "Slow Dest",
                    "config": { "kind": "plugin", "config": {} }
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let ch_id = ch["id"].as_i64().unwrap();
    let dest_id = ch["destinations"][0]["id"].as_i64().unwrap();
    let (feed_id, entry_id) = h
        .seed_feed_and_entry("Slow Item", None, "<p>Body</p>", None, None, None)
        .await;
    let (st, route) = h
        .req(
            Method::POST,
            "/api/v1/routes",
            Some(json!({
                "name": "Slow Route",
                "feed_ids": [feed_id],
                "channel_id": ch_id,
                "destination_id": dest_id
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let route_id = route["id"].as_i64().unwrap();
    let task_id =
        h.db.enqueue_delivery_task(route_id, ch_id, dest_id, entry_id, now_unix())
            .await
            .unwrap()
            .unwrap()
            .id;

    // Run 3 consecutive timed-out attempts -> circuit breaker trips on the 3rd timeout!
    for attempt in 1..=3 {
        sqlx::query(
            "UPDATE delivery_tasks SET status = 'queued', next_attempt_at = 0 WHERE id = ?1",
        )
        .bind(task_id)
        .execute(h.db.write_pool())
        .await
        .unwrap();
        h.dispatcher.drain_due_outbox_tasks(1).await.unwrap();
        let row = h.db.get_plugin("slow-chan").await.unwrap().unwrap();
        if attempt < 3 {
            assert!(row.enabled, "plugin must remain enabled before 3rd timeout");
        } else {
            assert!(
                !row.enabled,
                "circuit breaker must auto-disable plugin on 3rd timeout"
            );
            assert!(
                row.disabled_reason
                    .as_deref()
                    .is_some_and(|r| r.contains("circuit breaker") && r.contains('3')),
                "expected circuit breaker disabled_reason, got: {:?}",
                row.disabled_reason
            );
        }
    }

    // Give any stray background thread a brief moment and confirm ZERO ghost requests fired.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        *after_timeout_hits.lock().unwrap(),
        0,
        "post-timeout ghost fc.fetch must be cancelled immediately"
    );

    // `GET /api/v1/plugins` exposes `disabled_reason`, and re-enabling clears it.
    let (st, plugins) = h.req(Method::GET, "/api/v1/plugins", None).await;
    assert_eq!(st, StatusCode::OK);
    let slow_dto = plugins
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "slow-chan")
        .unwrap();
    assert_eq!(slow_dto["enabled"], false);
    assert!(slow_dto["disabled_reason"].as_str().is_some());

    let (st, reenabled) = h
        .req(
            Method::PATCH,
            "/api/v1/plugins/slow-chan",
            Some(json!({ "enabled": true })),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(reenabled["enabled"], true);
    assert!(reenabled.get("disabled_reason").is_none());
}

#[tokio::test]
async fn round3_uncompilable_plugin_stays_visible_and_defers_channel_tasks_and_respects_warnings() {
    let h = JsHarness::new().await;
    let chan_js = r#"
        export const meta = {
            id: "fragile-chan",
            name: "Fragile Chan",
            version: "0.1.0",
            api_version: "1",
            kind: "channel",
            account_schema: { type: "object", properties: {} },
            destination_schema: { type: "object", properties: {} }
        };
        export function deliver() { return { outcome: "delivered" }; }
    "#;
    let warn_proc_js = r#"
        export const meta = {
            id: "warn-proc",
            name: "Warn Proc",
            version: "0.1.0",
            api_version: "1",
            kind: "processor",
            hooks: ["on_render"],
            route_option_schema: { type: "object", properties: {} }
        };
        export function onRender() {
            return { outcome: "continue", warnings: ["custom notice from plugin"] };
        }
    "#;
    for js in [chan_js, warn_proc_js] {
        let (st, _) = h
            .req(
                Method::POST,
                "/api/v1/plugins/upload",
                Some(json!({ "script_js": js, "enabled": true })),
            )
            .await;
        assert_eq!(st, StatusCode::CREATED);
    }

    let (st, ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Fragile Acct",
                "kind": "plugin",
                "config": { "kind": "plugin", "plugin_id": "fragile-chan", "config": {} },
                "initial_destination": {
                    "name": "Dest",
                    "config": { "kind": "plugin", "config": {} }
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let ch_id = ch["id"].as_i64().unwrap();
    let dest_id = ch["destinations"][0]["id"].as_i64().unwrap();
    let (feed_id, entry1_id) = h
        .seed_feed_and_entry("Warn Entry", None, "<p>Hi</p>", None, None, None)
        .await;
    let (st, route) = h
        .req(
            Method::POST,
            "/api/v1/routes",
            Some(json!({
                "name": "Fragile Route",
                "feed_ids": [feed_id],
                "channel_id": ch_id,
                "destination_id": dest_id,
                "options": { "plugins": { "warn-proc": {} } }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let route_id = route["id"].as_i64().unwrap();

    // P2-5: Delivered task with custom JS plugin warning has `[warn] ` prefix and `has_warnings: true`!
    let task1_id =
        h.db.enqueue_delivery_task(route_id, ch_id, dest_id, entry1_id, now_unix())
            .await
            .unwrap()
            .unwrap()
            .id;
    h.dispatcher.drain_due_outbox_tasks(10).await.unwrap();
    let (st, tasks) = h.req(Method::GET, "/api/v1/tasks", None).await;
    assert_eq!(st, StatusCode::OK);
    let t1 = tasks
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == task1_id)
        .unwrap();
    assert_eq!(t1["status"], "delivered");
    assert_eq!(t1["has_warnings"], true, "expected has_warnings=true: {t1}");
    assert!(
        t1["diagnostic_log"]
            .as_str()
            .is_some_and(|d| d.contains("[warn] custom notice from plugin")),
        "expected [warn] prefix in diagnostic_log: {t1}"
    );

    // P1-5: Corrupt `fragile-chan` script_js in DB -> reload -> remains visible as `compatible: false`
    // and new tasks transition to `Deferred` instead of `DeadLetter`!
    sqlx::query(
        "UPDATE plugins SET script_js = 'export const syntax error @@@' WHERE id = 'fragile-chan'",
    )
    .execute(h.db.write_pool())
    .await
    .unwrap();
    h.dispatcher.plugins().reload_from_db(&h.db).await.unwrap();

    let (st, plugins) = h.req(Method::GET, "/api/v1/plugins", None).await;
    assert_eq!(st, StatusCode::OK);
    let fragile_dto = plugins
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "fragile-chan")
        .expect("uncompilable plugin must stay visible in GET /api/v1/plugins");
    assert_eq!(fragile_dto["compatible"], false);
    assert_eq!(fragile_dto["enabled"], false);
    assert!(
        fragile_dto["compatibility_reason"]
            .as_str()
            .is_some_and(|r| r.contains("Failed to compile")),
        "unexpected compatibility_reason: {fragile_dto}"
    );

    let (_, entry2_id) = h
        .seed_feed_and_entry("Defer Entry", None, "<p>Body 2</p>", None, None, None)
        .await;
    let task2_id =
        h.db.enqueue_delivery_task(route_id, ch_id, dest_id, entry2_id, now_unix())
            .await
            .unwrap()
            .unwrap()
            .id;
    h.dispatcher.drain_due_outbox_tasks(10).await.unwrap();
    let t2 = h.db.get_delivery_task(task2_id).await.unwrap().unwrap();
    assert_eq!(
        t2.status,
        DeliveryTaskStatus::Deferred,
        "incompatible/uncompilable channel plugin must defer task instead of dead-lettering: {t2:?}"
    );
}

#[tokio::test]
async fn round3_on_delivered_idempotency_key_and_instance_isolation_and_strict_disabled_provider() {
    let srv_a = MockServer::start().await;
    let srv_b = MockServer::start().await;
    let hits_a = Arc::new(Mutex::new(0usize));
    let hits_b = Arc::new(Mutex::new(0usize));
    let a_clone = Arc::clone(&hits_a);
    let b_clone = Arc::clone(&hits_b);

    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(move |_req: &MockRequest| {
            *a_clone.lock().unwrap() += 1;
            ResponseTemplate::new(200)
        })
        .mount(&srv_a)
        .await;
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(move |_req: &MockRequest| {
            *b_clone.lock().unwrap() += 1;
            ResponseTemplate::new(200)
        })
        .mount(&srv_b)
        .await;

    let h = JsHarness::new().await;
    let chan_js = r#"
        export const meta = {
            id: "noop-chan",
            name: "Noop Chan",
            version: "0.1.0",
            api_version: "1",
            kind: "channel",
            account_schema: { type: "object", properties: {} },
            destination_schema: { type: "object", properties: {} }
        };
        export function deliver() { return { outcome: "delivered" }; }
    "#;
    let iso_proc_js = format!(
        r#"
        export const meta = {{
            id: "iso-proc",
            name: "Iso Proc",
            version: "0.1.0",
            api_version: "1",
            kind: "processor",
            hooks: ["on_render", "on_delivered"],
            instance_schema: {{
                type: "object",
                properties: {{ endpoint: {{ type: "string", format: "uri" }} }}
            }},
            route_option_schema: {{
                type: "object",
                properties: {{
                    provider_id: {{ type: "integer" }}
                }}
            }}
        }};
        export function onRender(ctx) {{
            return {{ outcome: "continue" }};
        }}
        export function onDelivered(ctx) {{
            // Try reaching srv_b (belongs to another instance) -> must be SSRF-blocked!
            let bBlocked = false;
            try {{
                fc.fetch({{ method: "POST", url: "{uri_b}/hook", body: "x" }});
            }} catch (e) {{
                bBlocked = String(e.message || e).indexOf("SSRF_BLOCKED") !== -1;
            }}
            if (!bBlocked) {{
                throw new Error("cross-instance origin leaked!");
            }}
            // Reaching srv_a (bound provider_id) must succeed!
            const resp = fc.fetch({{ method: "POST", url: "{uri_a}/hook", body: JSON.stringify(ctx.payload) }});
            if (!resp.ok) throw new Error("srv_a failed");
        }}
        "#,
        uri_a = srv_a.uri(),
        uri_b = srv_b.uri()
    );
    for js in [chan_js, &iso_proc_js] {
        let (st, resp) = h
            .req(
                Method::POST,
                "/api/v1/plugins/upload",
                Some(json!({ "script_js": js, "enabled": true })),
            )
            .await;
        assert_eq!(st, StatusCode::CREATED, "upload failed: {resp}");
    }

    let (st, inst_a) = h
        .req(
            Method::POST,
            "/api/v1/plugins/iso-proc/instances",
            Some(
                json!({ "name": "Inst A", "config": { "endpoint": srv_a.uri() }, "enabled": true }),
            ),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let inst_a_id = inst_a["id"].as_i64().unwrap();

    let (st, _inst_b) = h
        .req(
            Method::POST,
            "/api/v1/plugins/iso-proc/instances",
            Some(
                json!({ "name": "Inst B", "config": { "endpoint": srv_b.uri() }, "enabled": true }),
            ),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);

    let (st, ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Noop Ch",
                "kind": "plugin",
                "min_interval_secs": 0,
                "jitter_max_secs": 0,
                "config": { "kind": "plugin", "plugin_id": "noop-chan", "config": {} },
                "initial_destination": { "name": "Dest", "config": { "kind": "plugin", "config": {} } }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let ch_id = ch["id"].as_i64().unwrap();
    let dest_id = ch["destinations"][0]["id"].as_i64().unwrap();
    let (feed_id, entry1_id) = h
        .seed_feed_and_entry("Iso Entry", None, "<p>Body</p>", None, None, None)
        .await;
    let (st, route) = h
        .req(
            Method::POST,
            "/api/v1/routes",
            Some(json!({
                "name": "Iso Route",
                "feed_ids": [feed_id],
                "channel_id": ch_id,
                "destination_id": dest_id,
                "options": {
                    "plugins": {
                        "iso-proc": { "provider_id": inst_a_id, "strict_mode": true }
                    }
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "route failed: {route}");
    let route_id = route["id"].as_i64().unwrap();

    let task1_id =
        h.db.enqueue_delivery_task(route_id, ch_id, dest_id, entry1_id, now_unix())
            .await
            .unwrap()
            .unwrap()
            .id;
    h.dispatcher.drain_due_outbox_tasks(10).await.unwrap();

    // Verify deterministic idempotency key (`sha256("on_delivered:iso-proc:{task1_id}")`, no timestamp!)
    let events =
        h.db.list_due_plugin_events(now_unix() + 60, 10)
            .await
            .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].idempotency_key,
        sha256_hex(format!("on_delivered:iso-proc:{task1_id}").as_bytes())
    );
    let ev_payload: Value = serde_json::from_str(&events[0].payload_json).unwrap();
    assert_eq!(ev_payload["provider_id"], inst_a_id);

    // Dispatch event: srv_a succeeds, srv_b blocked!
    assert_eq!(
        h.dispatcher
            .process_due_plugin_events_once(10)
            .await
            .unwrap(),
        1
    );
    assert_eq!(*hits_a.lock().unwrap(), 1);
    assert_eq!(*hits_b.lock().unwrap(), 0);
    assert!(
        h.db.list_due_plugin_events(now_unix() + 9999, 10)
            .await
            .unwrap()
            .is_empty()
    );

    // Disable `inst_a`: because route has `strict_mode: true`, new delivery MUST enter `Retrying`!
    let (st, _) = h
        .req(
            Method::PATCH,
            &format!("/api/v1/plugins/iso-proc/instances/{inst_a_id}"),
            Some(json!({ "enabled": false })),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let (_, entry2_id) = h
        .seed_feed_and_entry("Strict Disabled", None, "<p>Body 2</p>", None, None, None)
        .await;
    let task2_id =
        h.db.enqueue_delivery_task(route_id, ch_id, dest_id, entry2_id, now_unix())
            .await
            .unwrap()
            .unwrap()
            .id;
    h.dispatcher.drain_due_outbox_tasks(10).await.unwrap();
    let t2 = h.db.get_delivery_task(task2_id).await.unwrap().unwrap();
    assert_eq!(
        t2.status,
        DeliveryTaskStatus::Retrying,
        "strict_mode must retry when bound provider instance is disabled: {t2:?}"
    );
}

#[tokio::test]
async fn round3_market_install_returns_409_over_custom_plugin_and_disabled_js_processor_rejected() {
    let market_srv = MockServer::start().await;
    let ntfy_js = include_str!("fixtures/plugins/ntfy_0_3_1.js");
    let ntfy_sha = sha256_hex(ntfy_js.as_bytes());
    let index_json = json!({
        "schema_version": 1,
        "generated_at": "2026-11-25T00:00:00Z",
        "plugins": [{
            "id": "ntfy",
            "name": "ntfy Push",
            "version": "0.3.1",
            "latest": "0.3.1",
            "api_version": "1",
            "kind": "channel",
            "runtime": "js",
            "min_core": "0.3.1",
            "category": "delivery",
            "icon": "bell",
            "description": "ntfy",
            "allowed_hosts": ["ntfy.sh"],
            "hooks": [],
            "permissions": ["network"],
            "versions": [{
                "version": "0.3.1",
                "min_core": "0.3.1",
                "path": "plugins/channels/ntfy/plugin.js",
                "sha256": ntfy_sha
            }]
        }]
    });
    Mock::given(method("GET"))
        .and(path("/index.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(index_json))
        .mount(&market_srv)
        .await;
    Mock::given(method("GET"))
        .and(path("/plugins/channels/ntfy/plugin.js"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ntfy_js))
        .mount(&market_srv)
        .await;

    let h = JsHarness::with_options(
        Some(format!("{}/index.json", market_srv.uri())),
        vec!["127.0.0.1/32".to_string()],
    )
    .await;

    // 1. Install `ntfy` as a custom upload first.
    let (st, _) = h
        .req(
            Method::POST,
            "/api/v1/plugins/upload",
            Some(json!({ "script_js": ntfy_js, "enabled": true })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);

    // 2. Market dry_run preview exposes `installed_source: "custom"`.
    let (st, preview) = h
        .req(
            Method::POST,
            "/api/v1/plugins/install",
            Some(json!({ "plugin_id": "ntfy", "dry_run": true })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "preview failed: {preview}");
    assert_eq!(preview["already_installed"], true);
    assert_eq!(preview["installed_source"], "custom");

    // 3. Market install WITHOUT `replace_custom: true` MUST return 409 Conflict!
    let (st, conflict) = h
        .req(
            Method::POST,
            "/api/v1/plugins/install",
            Some(json!({ "plugin_id": "ntfy" })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::CONFLICT,
        "expected 409 Conflict: {conflict}"
    );

    // 4. Market install WITH `replace_custom: true` succeeds and switches source to `"market"`.
    let (st, installed) = h
        .req(
            Method::POST,
            "/api/v1/plugins/install",
            Some(json!({ "plugin_id": "ntfy", "replace_custom": true })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "replace_custom failed: {installed}");
    assert_eq!(installed["source"], "market");

    // 5. Disabled JS processor plugin rejects instance creation and route attachment!
    let proc_js = r#"
        export const meta = {
            id: "gated-proc",
            name: "Gated Proc",
            version: "0.1.0",
            api_version: "1",
            kind: "processor",
            hooks: ["on_render"],
            instance_schema: { type: "object", properties: { label: { type: "string" } } },
            route_option_schema: { type: "object", properties: {} }
        };
        export function onRender() { return { outcome: "continue" }; }
    "#;
    let (st, _) = h
        .req(
            Method::POST,
            "/api/v1/plugins/upload",
            Some(json!({ "script_js": proc_js, "enabled": false })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);

    let (st, err) = h
        .req(
            Method::POST,
            "/api/v1/plugins/gated-proc/instances",
            Some(json!({ "name": "Blocked Inst", "config": { "label": "x" } })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "disabled JS processor must reject instance creation: {err}"
    );
}

#[tokio::test]
async fn round4_circuit_breaker_ignores_short_fetch_timeout_and_slot_starvation_but_trips_on_swallowed_deadline()
 {
    let slow_srv = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/slow-short"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(350)))
        .mount(&slow_srv)
        .await;
    Mock::given(method("GET"))
        .and(path("/hog"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(700)))
        .mount(&slow_srv)
        .await;

    let h = JsHarness::new().await;
    let short_fetch_js = r#"
        export const meta = {
            id: "short-fetch-chan",
            name: "Short Fetch Chan",
            version: "0.1.0",
            api_version: "1",
            kind: "channel",
            account_schema: {
                type: "object",
                properties: { base_url: { type: "string", format: "uri" } }
            },
            destination_schema: { type: "object", properties: {} }
        };
        export function deliver(ctx) {
            // Uncaught short per-request timeout inside a much longer hook budget must NOT trip
            // the plugin circuit breaker.
            fc.fetch({ method: "GET", url: ctx.account.base_url + "/slow-short", timeoutMs: 120 });
            return { outcome: "delivered" };
        }
    "#;
    let hog_js = r#"
        export const meta = {
            id: "hog-chan",
            name: "Hog Chan",
            version: "0.1.0",
            api_version: "1",
            kind: "channel",
            account_schema: {
                type: "object",
                properties: { base_url: { type: "string", format: "uri" } }
            },
            destination_schema: { type: "object", properties: {} }
        };
        export function deliver(ctx) {
            fc.fetch({ method: "GET", url: ctx.account.base_url + "/hog", timeoutMs: 5000 });
            return { outcome: "delivered" };
        }
    "#;
    let healthy_js = r#"
        export const meta = {
            id: "healthy-wait-chan",
            name: "Healthy Wait Chan",
            version: "0.1.0",
            api_version: "1",
            kind: "channel",
            account_schema: { type: "object", properties: {} },
            destination_schema: { type: "object", properties: {} }
        };
        export function deliver() { return { outcome: "delivered" }; }
    "#;
    let swallow_js = r#"
        export const meta = {
            id: "swallow-timeout-chan",
            name: "Swallow Timeout Chan",
            version: "0.1.0",
            api_version: "1",
            kind: "channel",
            account_schema: {
                type: "object",
                properties: { base_url: { type: "string", format: "uri" } }
            },
            destination_schema: { type: "object", properties: {} }
        };
        export function deliver(ctx) {
            try {
                fc.fetch({ method: "GET", url: ctx.account.base_url + "/slow-short", timeoutMs: 5000 });
            } catch (_e) {
                // Intentionally swallow deadline error and pretend delivery succeeded.
                return { outcome: "delivered" };
            }
            return { outcome: "delivered" };
        }
    "#;
    for js in [short_fetch_js, hog_js, healthy_js, swallow_js] {
        let (st, resp) = h
            .req(
                Method::POST,
                "/api/v1/plugins/upload",
                Some(json!({ "script_js": js, "enabled": true })),
            )
            .await;
        assert_eq!(st, StatusCode::CREATED, "upload failed: {resp}");
    }

    // 1. Short per-request `timeoutMs` (120ms) inside a 2000ms hook budget: 4 consecutive failures
    //    must retry normally and NEVER trip the circuit breaker.
    h.dispatcher
        .plugins()
        .set_hook_timeout_override(Some(Duration::from_millis(2000)));
    let (st, ch1) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Short Fetch Ch",
                "kind": "plugin",
                "min_interval_secs": 0,
                "jitter_max_secs": 0,
                "config": {
                    "kind": "plugin",
                    "plugin_id": "short-fetch-chan",
                    "config": { "base_url": slow_srv.uri() }
                },
                "initial_destination": {
                    "name": "Dest",
                    "config": { "kind": "plugin", "config": {} }
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let ch1_id = ch1["id"].as_i64().unwrap();
    let dest1_id = ch1["destinations"][0]["id"].as_i64().unwrap();
    let (feed1_id, entry1_id) = h
        .seed_feed_and_entry("Short Fetch Item", None, "<p>Body</p>", None, None, None)
        .await;
    let (st, route1) = h
        .req(
            Method::POST,
            "/api/v1/routes",
            Some(json!({
                "name": "Short Fetch Route",
                "feed_ids": [feed1_id],
                "channel_id": ch1_id,
                "destination_id": dest1_id
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let task1_id =
        h.db.enqueue_delivery_task(
            route1["id"].as_i64().unwrap(),
            ch1_id,
            dest1_id,
            entry1_id,
            now_unix(),
        )
        .await
        .unwrap()
        .unwrap()
        .id;

    for _ in 0..4 {
        sqlx::query(
            "UPDATE delivery_tasks SET status = 'queued', next_attempt_at = 0 WHERE id = ?1",
        )
        .bind(task1_id)
        .execute(h.db.write_pool())
        .await
        .unwrap();
        h.dispatcher.drain_due_outbox_tasks(1).await.unwrap();
    }
    let row = h.db.get_plugin("short-fetch-chan").await.unwrap().unwrap();
    assert!(
        row.enabled && row.disabled_reason.is_none(),
        "short fc.fetch timeoutMs must NEVER trip circuit breaker: {row:?}"
    );
    let t1 = h.db.get_delivery_task(task1_id).await.unwrap().unwrap();
    assert!(
        t1.diagnostic_log
            .as_deref()
            .is_some_and(|d| d.contains("timed out")
                && !d.contains("JS plugin execution timed out / cancelled")),
        "expected per-request HTTP timeout diagnostic, got: {:?}",
        t1.diagnostic_log
    );

    // 2. Sandbox slot starvation: occupy all 4 global JS semaphore permits with `hog-chan` while
    //    `healthy-wait-chan` times out waiting for a slot 3 times -> healthy plugin stays enabled!
    let (st, hog_ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Hog Ch",
                "kind": "plugin",
                "min_interval_secs": 0,
                "jitter_max_secs": 0,
                "config": {
                    "kind": "plugin",
                    "plugin_id": "hog-chan",
                    "config": { "base_url": slow_srv.uri() }
                },
                "initial_destination": {
                    "name": "Hog Dest",
                    "config": { "kind": "plugin", "config": {} }
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let hog_ch_id = hog_ch["id"].as_i64().unwrap();
    let hog_dest_id = hog_ch["destinations"][0]["id"].as_i64().unwrap();
    let (st, healthy_ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Healthy Ch",
                "kind": "plugin",
                "min_interval_secs": 0,
                "jitter_max_secs": 0,
                "config": {
                    "kind": "plugin",
                    "plugin_id": "healthy-wait-chan",
                    "config": {}
                },
                "initial_destination": {
                    "name": "Healthy Dest",
                    "config": { "kind": "plugin", "config": {} }
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let healthy_ch_id = healthy_ch["id"].as_i64().unwrap();
    let healthy_dest_id = healthy_ch["destinations"][0]["id"].as_i64().unwrap();
    let (feed2_id, entry2_id) = h
        .seed_feed_and_entry("Wait Item", None, "<p>Body</p>", None, None, None)
        .await;
    let (st, healthy_route) = h
        .req(
            Method::POST,
            "/api/v1/routes",
            Some(json!({
                "name": "Healthy Route",
                "feed_ids": [feed2_id],
                "channel_id": healthy_ch_id,
                "destination_id": healthy_dest_id
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let healthy_task_id =
        h.db.enqueue_delivery_task(
            healthy_route["id"].as_i64().unwrap(),
            healthy_ch_id,
            healthy_dest_id,
            entry2_id,
            now_unix(),
        )
        .await
        .unwrap()
        .unwrap()
        .id;

    let hog_runtime = h
        .dispatcher
        .plugins()
        .get_js("hog-chan")
        .unwrap()
        .runtime
        .unwrap();
    let hog_http = ProtectedHttpClient::new(NetSettings {
        outbound_proxy_url: None,
        ssrf_allowlist: vec![],
    })
    .unwrap();
    let hog_acct = json!({ "base_url": slow_srv.uri() });
    let hog_dest = json!({});
    let _ = (hog_ch_id, hog_dest_id);

    // Spawn 4 concurrent hog invocations via `test_config`? `hog-chan` only exports `deliver`, so
    // invoke `JsChannelSender` directly or spawn 4 concurrent `onDelivered`-less hog calls by
    // launching 4 tasks against `hog-chan` through `JsChannelSender`.
    let hog_ch_row = h.db.get_channel(hog_ch_id).await.unwrap().unwrap();
    let hog_dest_row = h.db.get_destination(hog_dest_id).await.unwrap().unwrap();
    let hog_feed_row = h.db.get_feed(feed2_id).await.unwrap().unwrap();
    let hog_entry_row = h.db.get_entry(entry2_id).await.unwrap().unwrap();
    let hog_route_row =
        h.db.get_route(healthy_route["id"].as_i64().unwrap())
            .await
            .unwrap()
            .unwrap();
    let _ = (hog_runtime, hog_acct, hog_dest);

    for _attempt in 0..3 {
        // Launch 4 concurrent `hog-chan` deliveries with a 2s hook timeout so all 4 semaphore
        // slots stay occupied for ~700ms.
        h.dispatcher
            .plugins()
            .set_hook_timeout_override(Some(Duration::from_millis(2000)));
        let mut hog_handles = Vec::new();
        for _ in 0..4 {
            let sender = JsChannelSender::new(h.dispatcher.plugins().clone(), hog_http.clone());
            let ch = hog_ch_row.clone();
            let dest = hog_dest_row.clone();
            let feed = hog_feed_row.clone();
            let entry = hog_entry_row.clone();
            let route = hog_route_row.clone();
            let acct = ChannelConfig::Plugin(PluginChannelConfig {
                plugin_id: "hog-chan".to_string(),
                config: json!({ "base_url": slow_srv.uri() }),
            });
            let dcfg = DestinationConfig::Plugin(PluginDestinationConfig { config: json!({}) });
            hog_handles.push(tokio::spawn(async move {
                let task = feedcourier::db::models::DeliveryTask {
                    id: 0,
                    route_id: route.id,
                    channel_id: ch.id,
                    destination_id: dest.id,
                    entry_id: entry.id,
                    status: DeliveryTaskStatus::Delivering,
                    lease_until: None,
                    checkpoint_json: None,
                    attempt_count: 0,
                    next_attempt_at: 0,
                    last_status_code: None,
                    diagnostic_log: None,
                    completed_at: None,
                    created_at: 0,
                };
                let ctx = feedcourier::channels::DeliveryContext {
                    task: &task,
                    route: &route,
                    route_options: feedcourier::channels::RouteOptions::default(),
                    timezone: chrono_tz::UTC,
                    channel: &ch,
                    destination: &dest,
                    channel_config: &acct,
                    destination_config: &dcfg,
                    feed: &feed,
                    entry: &entry,
                    checkpoint: None,
                    checkpoint_sink: None,
                    overrides: feedcourier::plugins::RenderOverrides::default(),
                    smtp_delivery: None,
                };
                let _ = sender.send(&ctx).await;
            }));
        }
        // Give the 4 hog tasks a moment to acquire all 4 sandbox slots, then attempt to deliver
        // `healthy-wait-chan` with a short 120ms timeout (guaranteed to time out waiting for a slot).
        tokio::time::sleep(Duration::from_millis(80)).await;
        h.dispatcher
            .plugins()
            .set_hook_timeout_override(Some(Duration::from_millis(120)));
        sqlx::query(
            "UPDATE delivery_tasks SET status = 'queued', next_attempt_at = 0 WHERE id = ?1",
        )
        .bind(healthy_task_id)
        .execute(h.db.write_pool())
        .await
        .unwrap();
        h.dispatcher.drain_due_outbox_tasks(1).await.unwrap();
        for jh in hog_handles {
            let _ = jh.await;
        }
    }

    let healthy_row = h.db.get_plugin("healthy-wait-chan").await.unwrap().unwrap();
    assert!(
        healthy_row.enabled && healthy_row.disabled_reason.is_none(),
        "slot-starvation timeout must NEVER disable healthy waiting plugin: {healthy_row:?}"
    );
    let healthy_task =
        h.db.get_delivery_task(healthy_task_id)
            .await
            .unwrap()
            .unwrap();
    assert!(
        healthy_task
            .diagnostic_log
            .as_deref()
            .is_some_and(|d| d.contains("waiting for JS sandbox slot")),
        "expected slot-wait diagnostic, got: {:?}",
        healthy_task.diagnostic_log
    );

    // 3. Even if a plugin catches the deadline error inside JS and returns `{ outcome: "delivered" }`,
    //    exhausting the hook's wall-clock deadline MUST still return `JsSandboxTimeout` and trip
    //    the circuit breaker on the 3rd consecutive timeout!
    h.dispatcher
        .plugins()
        .set_hook_timeout_override(Some(Duration::from_millis(150)));
    let (st, sw_ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Swallow Ch",
                "kind": "plugin",
                "min_interval_secs": 0,
                "jitter_max_secs": 0,
                "config": {
                    "kind": "plugin",
                    "plugin_id": "swallow-timeout-chan",
                    "config": { "base_url": slow_srv.uri() }
                },
                "initial_destination": {
                    "name": "Swallow Dest",
                    "config": { "kind": "plugin", "config": {} }
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let sw_ch_id = sw_ch["id"].as_i64().unwrap();
    let sw_dest_id = sw_ch["destinations"][0]["id"].as_i64().unwrap();
    let (feed3_id, entry3_id) = h
        .seed_feed_and_entry("Swallow Item", None, "<p>Body</p>", None, None, None)
        .await;
    let (st, sw_route) = h
        .req(
            Method::POST,
            "/api/v1/routes",
            Some(json!({
                "name": "Swallow Route",
                "feed_ids": [feed3_id],
                "channel_id": sw_ch_id,
                "destination_id": sw_dest_id
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let sw_task_id =
        h.db.enqueue_delivery_task(
            sw_route["id"].as_i64().unwrap(),
            sw_ch_id,
            sw_dest_id,
            entry3_id,
            now_unix(),
        )
        .await
        .unwrap()
        .unwrap()
        .id;

    for _ in 0..3 {
        sqlx::query(
            "UPDATE delivery_tasks SET status = 'queued', next_attempt_at = 0 WHERE id = ?1",
        )
        .bind(sw_task_id)
        .execute(h.db.write_pool())
        .await
        .unwrap();
        h.dispatcher.drain_due_outbox_tasks(1).await.unwrap();
    }
    let sw_row =
        h.db.get_plugin("swallow-timeout-chan")
            .await
            .unwrap()
            .unwrap();
    assert!(
        !sw_row.enabled && sw_row.disabled_reason.is_some(),
        "swallowed hook deadline overrun must still trip circuit breaker: {sw_row:?}"
    );
}

#[tokio::test]
async fn round4_allowed_hosts_uri_defaults_restore_api_ergonomics_without_private_bypass() {
    let h = JsHarness::new().await;
    let ntfy_js = include_str!("fixtures/plugins/ntfy_0_3_1.js");
    let bark_js = include_str!("fixtures/plugins/bark_0_3_1.js");
    for js in [ntfy_js, bark_js] {
        let (st, resp) = h
            .req(
                Method::POST,
                "/api/v1/plugins/upload",
                Some(json!({ "script_js": js, "enabled": true })),
            )
            .await;
        assert_eq!(st, StatusCode::CREATED, "upload failed: {resp}");
    }

    // 1. REST API callers omitting `server_url` for official `ntfy` and `bark` succeed because
    //    their defaults (`https://ntfy.sh`, `https://api.day.app`) belong to `meta.allowed_hosts`.
    let (st, ntfy_ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "ntfy Default API",
                "kind": "plugin",
                "config": { "kind": "plugin", "plugin_id": "ntfy", "config": {} },
                "initial_destination": {
                    "name": "Alerts",
                    "config": { "kind": "plugin", "config": { "topic": "fc-alerts" } }
                }
            })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "ntfy default REST create failed: {ntfy_ch}"
    );
    assert_eq!(ntfy_ch["config"]["config"]["server_url"], "https://ntfy.sh");

    let (st, bark_ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Bark Default API",
                "kind": "plugin",
                "config": { "kind": "plugin", "plugin_id": "bark", "config": {} },
                "initial_destination": {
                    "config": { "kind": "plugin", "config": { "device_key": "dk_api_default_test" } }
                }
            })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "bark default REST create failed: {bark_ch}"
    );
    assert_eq!(
        bark_ch["config"]["config"]["server_url"],
        "https://api.day.app"
    );

    // 2. Default public origins covered by `allowed_hosts` MUST NOT join `admin_uri_origins`
    //    (they traverse standard public SSRF protection), while explicit custom LAN URLs do.
    let ntfy_rt = h
        .dispatcher
        .plugins()
        .get_js("ntfy")
        .unwrap()
        .runtime
        .unwrap();
    let default_origins = feedcourier::plugins::js_runtime::extract_uri_origins_with_allowed_hosts(
        &ntfy_rt.manifest.account_schema,
        &json!({ "server_url": "https://ntfy.sh" }),
        &ntfy_rt.manifest.allowed_hosts,
    );
    assert!(
        default_origins.is_empty(),
        "allowed_hosts default must not receive private-origin bypass: {default_origins:?}"
    );
    let custom_lan_origins =
        feedcourier::plugins::js_runtime::extract_uri_origins_with_allowed_hosts(
            &ntfy_rt.manifest.account_schema,
            &json!({ "server_url": "http://192.168.1.50:8080" }),
            &ntfy_rt.manifest.allowed_hosts,
        );
    assert_eq!(custom_lan_origins.len(), 1);
    assert_eq!(custom_lan_origins[0].host, "192.168.1.50");

    // 3. Even if a malicious plugin lists `"127.0.0.1"` in `allowed_hosts` and sets a required
    //    `format: "uri"` default to `"http://127.0.0.1:6379"`, literal IPs / localhost are NEVER
    //    auto-filled!
    let evil_ip_default_js = r#"
        export const meta = {
            id: "evil-ip-default",
            name: "Evil IP Default",
            version: "0.1.0",
            api_version: "1",
            kind: "channel",
            allowed_hosts: ["127.0.0.1"],
            account_schema: {
                type: "object",
                required: ["server_url"],
                properties: {
                    server_url: { type: "string", format: "uri", default: "http://127.0.0.1:6379" }
                }
            },
            destination_schema: { type: "object", properties: {} }
        };
        export function deliver() { return { outcome: "delivered" }; }
    "#;
    let (st, _) = h
        .req(
            Method::POST,
            "/api/v1/plugins/upload",
            Some(json!({ "script_js": evil_ip_default_js, "enabled": true })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);

    let (st, err) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Evil Ch",
                "kind": "plugin",
                "config": { "kind": "plugin", "plugin_id": "evil-ip-default", "config": {} },
                "initial_destination": {
                    "name": "Dest",
                    "config": { "kind": "plugin", "config": {} }
                }
            })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "literal IP URI default must never be auto-filled: {err}"
    );
}

#[tokio::test]
async fn round4_on_delivered_fail_closed_when_bound_instance_or_channel_disabled_or_deleted() {
    let target_srv = MockServer::start().await;
    let hits = Arc::new(Mutex::new(0usize));
    let hits_clone = Arc::clone(&hits);
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(move |_req: &MockRequest| {
            *hits_clone.lock().unwrap() += 1;
            ResponseTemplate::new(200)
        })
        .mount(&target_srv)
        .await;

    // Allow loopback via `ssrf_allowlist` so a direct fallback to `allowed_hosts` WOULD succeed
    // and increment `hits` if fail-closed protection were ever bypassed!
    let h = JsHarness::with_options(None, vec!["127.0.0.1".to_string()]).await;

    let proc_js = format!(
        r#"
        export const meta = {{
            id: "proxy-proc",
            name: "Proxy Proc",
            version: "0.1.0",
            api_version: "1",
            kind: "processor",
            allowed_hosts: ["127.0.0.1"],
            hooks: ["on_render", "on_delivered"],
            instance_schema: {{
                type: "object",
                properties: {{
                    use_proxy: {{ type: "boolean", default: true }}
                }}
            }},
            route_option_schema: {{
                type: "object",
                properties: {{ provider_id: {{ type: "select", source: "plugin_instances" }} }}
            }}
        }};
        export function onRender() {{ return {{ outcome: "continue" }}; }}
        export function onDelivered() {{
            fc.fetch({{ method: "POST", url: "{}/hook", body: "direct-leak" }});
        }}
        "#,
        target_srv.uri()
    );
    let chan_js = format!(
        r#"
        export const meta = {{
            id: "proxy-chan",
            name: "Proxy Chan",
            version: "0.1.0",
            api_version: "1",
            kind: "channel",
            allowed_hosts: ["127.0.0.1"],
            hooks: ["on_delivered"],
            account_schema: {{ type: "object", properties: {{}} }},
            destination_schema: {{ type: "object", properties: {{}} }}
        }};
        export function deliver() {{ return {{ outcome: "delivered" }}; }}
        export function onDelivered() {{
            fc.fetch({{ method: "POST", url: "{}/hook", body: "chan-direct-leak" }});
        }}
        "#,
        target_srv.uri()
    );
    for js in [proc_js, chan_js] {
        let (st, resp) = h
            .req(
                Method::POST,
                "/api/v1/plugins/upload",
                Some(json!({ "script_js": js, "enabled": true })),
            )
            .await;
        assert_eq!(st, StatusCode::CREATED, "upload failed: {resp}");
    }

    // Create a `use_proxy: true` processor instance and a channel plugin account with `use_proxy: true`.
    let (st, inst) = h
        .req(
            Method::POST,
            "/api/v1/plugins/proxy-proc/instances",
            Some(json!({
                "name": "Proxy Inst",
                "config": { "use_proxy": true },
                "enabled": true
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let inst_id = inst["id"].as_i64().unwrap();

    let (st, ch) = h
        .req(
            Method::POST,
            "/api/v1/channels",
            Some(json!({
                "name": "Proxy Chan Acct",
                "kind": "plugin",
                "use_proxy": true,
                "min_interval_secs": 0,
                "jitter_max_secs": 0,
                "config": { "kind": "plugin", "plugin_id": "proxy-chan", "config": {} },
                "initial_destination": {
                    "name": "Dest",
                    "config": { "kind": "plugin", "config": {} }
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let ch_id = ch["id"].as_i64().unwrap();
    let dest_id = ch["destinations"][0]["id"].as_i64().unwrap();
    let (feed_id, entry_id) = h
        .seed_feed_and_entry("Proxy Guard Item", None, "<p>Body</p>", None, None, None)
        .await;
    let (st, route) = h
        .req(
            Method::POST,
            "/api/v1/routes",
            Some(json!({
                "name": "Proxy Route",
                "feed_ids": [feed_id],
                "channel_id": ch_id,
                "destination_id": dest_id,
                "options": {
                    "plugins": {
                        "proxy-proc": { "provider_id": inst_id }
                    }
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let route_id = route["id"].as_i64().unwrap();

    // Enqueue and deliver: this enqueues BOTH `proxy-proc` (with `provider_id: inst_id`) and
    // `proxy-chan` (`channel.id: ch_id`) `on_delivered` events in `plugin_events`.
    h.db.enqueue_delivery_task(route_id, ch_id, dest_id, entry_id, now_unix())
        .await
        .unwrap();
    h.dispatcher.drain_due_outbox_tasks(5).await.unwrap();

    let queued =
        h.db.list_due_plugin_events(now_unix() + 60, 10)
            .await
            .unwrap();
    assert_eq!(
        queued.len(),
        2,
        "expected 2 queued on_delivered events: {queued:?}"
    );

    // 1. Disable BOTH the processor instance AND the plugin channel before events are processed.
    let (st, _) = h
        .req(
            Method::PATCH,
            &format!("/api/v1/plugins/proxy-proc/instances/{inst_id}"),
            Some(json!({ "enabled": false })),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = h
        .req(
            Method::PATCH,
            &format!("/api/v1/channels/{ch_id}"),
            Some(json!({ "enabled": false })),
        )
        .await;
    assert_eq!(st, StatusCode::OK);

    assert_eq!(
        h.dispatcher
            .process_due_plugin_events_once(10)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        *hits.lock().unwrap(),
        0,
        "disabled instance/channel MUST NOT fall back to direct use_proxy=false dispatch"
    );
    // Both events must be rescheduled (`attempts == 1`), not dropped yet!
    let deferred =
        h.db.list_due_plugin_events(now_unix() + 3600, 10)
            .await
            .unwrap();
    assert_eq!(deferred.len(), 2);
    assert!(deferred.iter().all(|e| e.attempts == 1));

    // 2. Delete the route, then delete BOTH the processor instance AND the plugin channel, make
    //    events due again -> both must be dropped cleanly with ZERO outbound requests!
    let (st, _) = h
        .req(Method::DELETE, &format!("/api/v1/routes/{route_id}"), None)
        .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = h
        .req(
            Method::DELETE,
            &format!("/api/v1/plugins/proxy-proc/instances/{inst_id}"),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = h
        .req(Method::DELETE, &format!("/api/v1/channels/{ch_id}"), None)
        .await;
    assert_eq!(st, StatusCode::OK);

    sqlx::query("UPDATE plugin_events SET next_attempt_at = 0")
        .execute(h.db.write_pool())
        .await
        .unwrap();
    assert_eq!(
        h.dispatcher
            .process_due_plugin_events_once(10)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        *hits.lock().unwrap(),
        0,
        "deleted instance/channel MUST NOT fall back to direct use_proxy=false dispatch"
    );
    assert!(
        h.db.list_due_plugin_events(now_unix() + 99999, 10)
            .await
            .unwrap()
            .is_empty(),
        "events for deleted instance/channel must be dropped"
    );
}
