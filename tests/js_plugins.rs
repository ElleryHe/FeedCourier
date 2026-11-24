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

    let ntfy_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../FeedCourier-Plugins/plugins/channels/ntfy/0.3.1/plugin.js");
    let bark_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../FeedCourier-Plugins/plugins/channels/bark/0.3.1/plugin.js");

    let ntfy_js = std::fs::read_to_string(&ntfy_path)
        .unwrap_or_else(|e| panic!("failed reading {}: {e}", ntfy_path.display()));
    let bark_js = std::fs::read_to_string(&bark_path)
        .unwrap_or_else(|e| panic!("failed reading {}: {e}", bark_path.display()));

    // Upload both real official plugins into FeedCourier
    for script in [&ntfy_js, &bark_js] {
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
                    "plugins": { "proc-a": {}, "proc-b": {} }
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
