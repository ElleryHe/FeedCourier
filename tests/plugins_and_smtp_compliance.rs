//! End-to-end integration tests for FeedCourier v0.3.0:
//! 1. Built-in Plugin Framework (`api_version = "1"`) & Link Rewrite (YOURLS) plugin
//!    (multi-provider CRUD, secret redaction/preservation, 409 route-reference guard,
//!    all 3 YOURLS auth modes, `error:url` reuse, 50-URL cap, `plugin_link_cache` hit,
//!    non-HTTP/self-domain/wildcard `exclude_domains` & media URL preservation,
//!    graceful degradation vs `strict_mode`, Telegram `link_preview_options.url` preservation,
//!    Notion `Link` + `Short URL` archival, marketplace snapshot fallback).
//! 2. MCP `list_plugins` (readonly, zero secret leakage) & `configure_route` `plugins` support.
//! 3. SMTP Per-Recipient Compliance (manual-trigger double opt-in, defer until verified,
//!    6-digit code attempts/expiry/cooldown, `/v/{token}` & `/u/{token}` read-only GET +
//!    idempotent POST, per-recipient single-`To` delivery, RFC 8058 headers + `mailto:` fallback,
//!    `sender_identity` CAN-SPAM footer, synchronous 5xx hard-bounce handling, auto-pause on
//!    last unsubscribe/bounce, reactivate -> re-verify, checkpoint resume, legacy backfill).
//! 4. R2-07 Notion lazy `database_id -> data_source_id` SQLite writeback during `send` & `test`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE, HOST, SET_COOKIE};
use axum::http::{Method, Request, StatusCode};
use bytes::Bytes;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request as MockRequest, ResponseTemplate};

use feedcourier::api::public::reset_public_rate_limiter_for_tests;
use feedcourier::api::{AppState, build_api_router};
use feedcourier::channels::notion::NotionSender;
use feedcourier::channels::{
    ChannelConfig, ChannelKind, DeliveryCheckpoint, DestinationConfig, NotionChannelConfig,
    NotionColumnMapping, NotionDestinationConfig, RouteOptions, SmtpChannelConfig,
    SmtpDestinationConfig, SmtpSecurity, TelegramChannelConfig, TelegramDestinationConfig,
};
use feedcourier::config::AppConfig;
use feedcourier::crypto::{MasterKey, md5_hex, sha256_hex};
use feedcourier::db::Database;
use feedcourier::db::models::{
    DeliveryTaskStatus, McpTokenScope, MediaAttachment, MediaAttachmentKind, NewChannel,
    NewDestination, NewEntry, NewFeed, NewRoute, SETTING_PUBLIC_URL, SmtpRecipientStatus, now_unix,
};
use feedcourier::mcp::{McpState, build_mcp_router_with_state};
use feedcourier::net::{NetSettings, ProtectedHttpClient};
use feedcourier::scheduler::DeliveryDispatcher;

struct V3Harness {
    _temp_dir: TempDir,
    db: Database,
    master_key: MasterKey,
    dispatcher: DeliveryDispatcher,
    router: Router,
    mcp_router: Router,
}

impl V3Harness {
    async fn new_with_allowlist(
        allow_loopback: bool,
        notion_base_url: Option<String>,
        plugin_index_url: Option<String>,
    ) -> Self {
        Self::new_with_options(allow_loopback, notion_base_url, plugin_index_url, false).await
    }

    async fn new_with_options(
        allow_loopback: bool,
        notion_base_url: Option<String>,
        plugin_index_url: Option<String>,
        trust_proxy: bool,
    ) -> Self {
        let temp_dir = TempDir::new().expect("tempdir");
        let mut config = AppConfig::with_data_dir(temp_dir.path());
        config.trust_proxy = trust_proxy;
        if allow_loopback {
            config.ssrf_allowlist = vec!["127.0.0.1/32".to_string()];
        }
        if let Some(url) = plugin_index_url {
            config.plugin_index_url = Some(url);
        }

        let db = Database::from_config(&config).await.expect("db");
        let master_key = MasterKey::from_config(&config).expect("master key");
        let mut dispatcher =
            DeliveryDispatcher::from_db_settings(config.clone(), db.clone(), master_key.clone())
                .await
                .expect("dispatcher")
                .with_smtp_per_recipient_delay(Duration::from_millis(1));

        if let Some(base) = notion_base_url {
            dispatcher = dispatcher.with_notion_sender(
                NotionSender::with_base_url(base, None).expect("notion sender"),
            );
        }

        let state = AppState::new(dispatcher.clone());
        let router = build_api_router(state);
        let mcp_state = McpState::new(db.clone(), dispatcher.ingest().clone())
            .with_outbox_notify(dispatcher.outbox_notify())
            .with_runtime(dispatcher.clone());
        let mcp_router = build_mcp_router_with_state(mcp_state);

        Self {
            _temp_dir: temp_dir,
            db,
            master_key,
            dispatcher,
            router,
            mcp_router,
        }
    }

    async fn send_json(
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
            Body::from(serde_json::to_vec(&payload).unwrap())
        } else {
            Body::empty()
        };
        let req = builder.body(body).unwrap();
        let resp = self.router.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, headers, bytes)
    }

    async fn send_public(
        &self,
        method: Method,
        uri: &str,
        client_ip: &str,
        raw_body: Option<&str>,
        content_type: Option<&str>,
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(HOST, "localhost:8080")
            .header("x-forwarded-for", client_ip);
        if let Some(ct) = content_type {
            builder = builder.header(CONTENT_TYPE, ct);
        }
        let body = match raw_body {
            Some(s) => Body::from(s.as_bytes().to_vec()),
            None => Body::empty(),
        };
        let req = builder.body(body).unwrap();
        let resp = self.router.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, headers, String::from_utf8_lossy(&bytes).to_string())
    }

    async fn setup_admin(&self) -> String {
        let (status, headers, body) = self
            .send_json(
                Method::POST,
                "/api/v1/auth/setup",
                None,
                Some(json!({ "password": "correct-horse-battery-v3" })),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "setup failed: {}",
            String::from_utf8_lossy(&body)
        );
        headers
            .get(SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .trim()
            .to_string()
    }
}

fn parse_json(bytes: &Bytes) -> Value {
    serde_json::from_slice(bytes)
        .unwrap_or_else(|e| panic!("invalid json ({e}): {}", String::from_utf8_lossy(bytes)))
}

/// Multi-connection mock SMTP server supporting per-recipient behaviors (250 vs 550).
#[derive(Default, Clone)]
struct CapturedMail {
    rcpt_to: Vec<String>,
    raw_data: String,
}

async fn spawn_multi_smtp_server(
    reject_550_substring: Option<String>,
) -> (
    u16,
    Arc<Mutex<Vec<CapturedMail>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let captured: Arc<Mutex<Vec<CapturedMail>>> = Arc::new(Mutex::new(Vec::new()));
    let cap_clone = Arc::clone(&captured);

    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let cap = Arc::clone(&cap_clone);
            let reject = reject_550_substring.clone();
            tokio::spawn(async move {
                let (r, mut w) = stream.into_split();
                let mut reader = BufReader::new(r);
                let _ = w.write_all(b"220 mock.smtp.test ESMTP Ready\r\n").await;
                let mut mail = CapturedMail::default();
                let mut line = String::new();
                loop {
                    line.clear();
                    let Ok(n) = reader.read_line(&mut line).await else {
                        break;
                    };
                    if n == 0 {
                        break;
                    }
                    let cmd = line.trim_end_matches(['\r', '\n']).to_string();
                    let upper = cmd.to_ascii_uppercase();
                    if upper.starts_with("EHLO") || upper.starts_with("HELO") {
                        let _ = w.write_all(b"250-mock.smtp.test\r\n250 8BITMIME\r\n").await;
                    } else if upper.starts_with("MAIL FROM:") {
                        mail = CapturedMail::default();
                        let _ = w.write_all(b"250 2.1.0 OK\r\n").await;
                    } else if upper.starts_with("RCPT TO:") {
                        mail.rcpt_to.push(cmd.clone());
                        if let Some(ref bad) = reject
                            && cmd.to_ascii_lowercase().contains(&bad.to_ascii_lowercase())
                        {
                            let _ = w
                                .write_all(
                                    b"550 5.1.1 Recipient address rejected: User unknown\r\n",
                                )
                                .await;
                        } else {
                            let _ = w.write_all(b"250 2.1.5 OK\r\n").await;
                        }
                    } else if upper == "DATA" {
                        let _ = w.write_all(b"354 Start mail input\r\n").await;
                        loop {
                            line.clear();
                            let Ok(dn) = reader.read_line(&mut line).await else {
                                break;
                            };
                            if dn == 0 || line == ".\r\n" || line == ".\n" {
                                break;
                            }
                            mail.raw_data.push_str(&line);
                        }
                        cap.lock().unwrap().push(mail.clone());
                        let _ = w.write_all(b"250 2.0.0 Queued\r\n").await;
                    } else if upper == "RSET" {
                        mail = CapturedMail::default();
                        let _ = w.write_all(b"250 2.0.0 Reset\r\n").await;
                    } else if upper == "QUIT" {
                        let _ = w.write_all(b"221 2.0.0 Bye\r\n").await;
                        break;
                    } else {
                        let _ = w.write_all(b"250 OK\r\n").await;
                    }
                }
            });
        }
    });

    (port, captured, handle)
}

fn extract_6_digit_code(raw_mime: &str) -> String {
    // Strip quoted-printable soft line breaks (`=\r\n` or `=\n`) if present.
    let unfolded = raw_mime.replace("=\r\n", "").replace("=\n", "");
    // Look for a standalone 6-digit number near verification text.
    let bytes = unfolded.as_bytes();
    for i in 0..bytes.len().saturating_sub(5) {
        let slice = &bytes[i..i + 6];
        if slice.iter().all(|b| b.is_ascii_digit()) {
            let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
            let after_ok = i + 6 == bytes.len() || !bytes[i + 6].is_ascii_alphanumeric();
            if before_ok && after_ok {
                let candidate = std::str::from_utf8(slice).unwrap();
                // Skip dates like 202611 if any; verification code sits inside `<div ...>` or `code: `
                let window_start = i.saturating_sub(40);
                let context = &unfolded[window_start..i];
                if context.contains("code")
                    || context.contains("验证码")
                    || context.contains("letter-spacing")
                {
                    return candidate.to_string();
                }
            }
        }
    }
    panic!("could not find 6-digit verification code in MIME:\n{unfolded}");
}

fn extract_public_token_path(raw_mime: &str, prefix: &str) -> String {
    let unfolded = raw_mime
        .replace("=\r\n", "")
        .replace("=\n", "")
        .replace("=3D", "=");
    let start = unfolded
        .find(prefix)
        .unwrap_or_else(|| panic!("missing '{prefix}' in MIME:\n{unfolded}"));
    let rest = &unfolded[start..];
    let end = rest
        .find(['>', '"', '\'', ' ', '\r', '\n', '<'])
        .unwrap_or(rest.len());
    let full_url = &rest[..end];
    let parsed = url::Url::parse(full_url).unwrap_or_else(|e| panic!("bad url '{full_url}': {e}"));
    parsed.path().to_string()
}

fn decode_qp_best_effort(raw: &str) -> String {
    raw.replace("=\r\n", "")
        .replace("=\n", "")
        .replace("=3D", "=")
}

// =============================================================================
// 1. Plugin Framework + Link Rewrite (YOURLS) + Telegram/Notion + Marketplace
// =============================================================================

#[tokio::test]
async fn plugin_framework_and_link_rewrite_yourls_end_to_end() {
    let yourls_srv = MockServer::start().await;
    let tg_srv = MockServer::start().await;
    let notion_srv = MockServer::start().await;
    let yourls_calls = Arc::new(AtomicUsize::new(0));
    let yourls_calls_clone = Arc::clone(&yourls_calls);

    // Mount YOURLS mock verifying signature, timed_signature (MD5), password, error:url reuse, and 500 failure.
    Mock::given(method("POST"))
        .and(path("/yourls-api.php"))
        .respond_with(move |req: &MockRequest| {
            let params: BTreeMap<String, String> = url::form_urlencoded::parse(&req.body)
                .into_owned()
                .collect();
            assert_eq!(params.get("format").map(String::as_str), Some("json"));
            let action = params.get("action").map(String::as_str).unwrap_or("");

            // Verify auth mode correctness.
            if let Some(ts) = params.get("timestamp") {
                let expected = md5_hex(format!("{ts}timed-secret-tok").as_bytes());
                assert_eq!(
                    params.get("signature").map(String::as_str),
                    Some(expected.as_str()),
                    "timed_signature must equal md5(timestamp + token)"
                );
            } else if params.contains_key("username") {
                assert_eq!(params.get("username").map(String::as_str), Some("admin"));
                assert_eq!(params.get("password").map(String::as_str), Some("pass123"));
            } else {
                assert_eq!(
                    params.get("signature").map(String::as_str),
                    Some("sig-secret-tok")
                );
            }

            if action == "db-stats" {
                return ResponseTemplate::new(200).set_body_json(json!({
                    "statusCode": 200,
                    "db-stats": { "total_links": "42", "total_clicks": "100" }
                }));
            }

            assert_eq!(action, "shorturl");
            let idx = yourls_calls_clone.fetch_add(1, Ordering::SeqCst);
            let long_url = params.get("url").cloned().unwrap_or_default();

            if long_url.contains("fail-this-link.example") {
                return ResponseTemplate::new(500).set_body_string("upstream error");
            }
            if long_url.contains("already-exists.example") {
                return ResponseTemplate::new(200).set_body_json(json!({
                    "status": "fail",
                    "code": "error:url",
                    "message": "already in database",
                    "shorturl": "https://sho.rt/existing1"
                }));
            }

            let slug = format!("s{idx}");
            ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "shorturl": format!("https://sho.rt/{slug}"),
                "statusCode": 200
            }))
        })
        .mount(&yourls_srv)
        .await;

    // Telegram CompactCard mock
    Mock::given(method("POST"))
        .and(path("/botTG_PLUGIN_TOK/sendMessage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": { "message_id": 701 }
        })))
        .mount(&tg_srv)
        .await;

    // Notion mock
    Mock::given(method("POST"))
        .and(path("/v1/pages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "page",
            "id": "notion-page-plugin-1"
        })))
        .mount(&notion_srv)
        .await;

    // Point market index to an unreachable local port so snapshot fallback is exercised!
    let h = V3Harness::new_with_allowlist(
        true,
        Some(notion_srv.uri()),
        Some("http://127.0.0.1:1/unreachable-index.json".to_string()),
    )
    .await;
    let cookie = h.setup_admin().await;

    // 1. Built-in plugin auto-seeded (disabled by default)
    let (st, _, body) = h
        .send_json(Method::GET, "/api/v1/plugins", Some(&cookie), None)
        .await;
    assert_eq!(st, StatusCode::OK);
    let plugins = parse_json(&body);
    assert_eq!(plugins.as_array().unwrap().len(), 1);
    assert_eq!(plugins[0]["id"], "link-rewrite");
    assert_eq!(plugins[0]["enabled"], false);
    assert_eq!(plugins[0]["manifest"]["api_version"], "1");

    // 2. Marketplace fallback merges installed/enabled status cleanly
    let (st, _, body) = h
        .send_json(Method::GET, "/api/v1/plugins/market", Some(&cookie), None)
        .await;
    assert_eq!(st, StatusCode::OK);
    let market = parse_json(&body);
    assert_eq!(market["source"], "snapshot");
    let lr_market = market["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "link-rewrite")
        .unwrap();
    assert_eq!(lr_market["installed"], true);
    assert_eq!(lr_market["enabled"], false);

    // 3. Enable link-rewrite globally
    let (st, _, body) = h
        .send_json(
            Method::PATCH,
            "/api/v1/plugins/link-rewrite",
            Some(&cookie),
            Some(json!({ "enabled": true })),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(parse_json(&body)["enabled"], true);

    // 4. Create 3 providers testing all 3 auth modes + secret redaction + connectivity test
    let api_url = format!("{}/yourls-api.php", yourls_srv.uri());

    // Provider 1: signature
    let (st, _, body) = h
        .send_json(
            Method::POST,
            "/api/v1/plugins/link-rewrite/instances",
            Some(&cookie),
            Some(json!({
                "name": "Primary YOURLS",
                "enabled": true,
                "config": {
                    "api_url": api_url,
                    "auth_mode": "signature",
                    "signature": "sig-secret-tok",
                    "exclude_domains": ["skip.example.com", "*.internal.org"],
                    "strict_mode": false,
                    "pass_title": true,
                    "use_proxy": false
                }
            })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let p1 = parse_json(&body);
    let p1_id = p1["id"].as_i64().unwrap();
    assert!(
        !serde_json::to_string(&p1)
            .unwrap()
            .contains("sig-secret-tok"),
        "signature secret must be redacted in DTO: {p1}"
    );
    assert_eq!(p1["config"]["has_signature"], true);

    // Secret-preserving PATCH (omitting signature preserves it) + test connectivity!
    let (st, _, _) = h
        .send_json(
            Method::PATCH,
            &format!("/api/v1/plugins/link-rewrite/instances/{p1_id}"),
            Some(&cookie),
            Some(json!({
                "name": "Primary YOURLS Renamed",
                "config": {
                    "api_url": api_url,
                    "auth_mode": "signature",
                    "signature": "",
                    "exclude_domains": ["skip.example.com", "*.internal.org"],
                    "strict_mode": false,
                    "pass_title": true,
                    "use_proxy": false
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::OK);

    let (st, _, body) = h
        .send_json(
            Method::POST,
            &format!("/api/v1/plugins/link-rewrite/instances/{p1_id}/test"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(parse_json(&body)["ok"], true);

    // Provider 2: timed_signature + strict_mode = true
    let (st, _, body) = h
        .send_json(
            Method::POST,
            "/api/v1/plugins/link-rewrite/instances",
            Some(&cookie),
            Some(json!({
                "name": "Strict Timed YOURLS",
                "enabled": true,
                "config": {
                    "api_url": api_url,
                    "auth_mode": "timed_signature",
                    "signature": "timed-secret-tok",
                    "exclude_domains": [],
                    "strict_mode": true,
                    "pass_title": false,
                    "use_proxy": false
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let p2_id = parse_json(&body)["id"].as_i64().unwrap();
    let (st, _, body) = h
        .send_json(
            Method::POST,
            &format!("/api/v1/plugins/link-rewrite/instances/{p2_id}/test"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

    // Provider 3: password auth
    let (st, _, body) = h
        .send_json(
            Method::POST,
            "/api/v1/plugins/link-rewrite/instances",
            Some(&cookie),
            Some(json!({
                "name": "Password YOURLS",
                "enabled": true,
                "config": {
                    "api_url": api_url,
                    "auth_mode": "password",
                    "username": "admin",
                    "password": "pass123",
                    "exclude_domains": [],
                    "strict_mode": false,
                    "pass_title": true,
                    "use_proxy": false
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let p3_id = parse_json(&body)["id"].as_i64().unwrap();
    let (st, _, body) = h
        .send_json(
            Method::POST,
            &format!("/api/v1/plugins/link-rewrite/instances/{p3_id}/test"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

    // 5. Create Feed + Telegram & Notion channels/destinations + routes referencing p1_id
    let feed =
        h.db.create_feed(&NewFeed::new("https://blog.example.org/rss.xml"))
            .await
            .unwrap();

    let tg_ch_cfg = ChannelConfig::Telegram(TelegramChannelConfig {
        bot_token: "TG_PLUGIN_TOK".to_string(),
        api_base_url: Some(tg_srv.uri()),
    });
    let tg_ch =
        h.db.create_channel(&NewChannel {
            name: "TG".to_string(),
            kind: ChannelKind::Telegram,
            encrypted_config: h.master_key.encrypt_json(&tg_ch_cfg).unwrap(),
            credential_group_key: tg_ch_cfg.credential_group_key(),
            capabilities_json: r#"{"supports_rich_message":false}"#.to_string(),
            min_interval_secs: 0,
            jitter_max_secs: 0,
            hourly_quota: None,
            daily_quota: None,
            use_proxy: false,
            enabled: true,
        })
        .await
        .unwrap();
    let tg_dest =
        h.db.create_destination(&NewDestination {
            channel_id: tg_ch.id,
            name: "TG Chat".to_string(),
            encrypted_config: h
                .master_key
                .encrypt_json(&DestinationConfig::Telegram(TelegramDestinationConfig {
                    chat_id: "-100999".to_string(),
                    message_thread_id: None,
                }))
                .unwrap(),
            enabled: true,
        })
        .await
        .unwrap();

    let notion_ch_cfg = ChannelConfig::Notion(NotionChannelConfig {
        integration_secret: "ntn_plugin_test".to_string(),
    });
    let notion_ch =
        h.db.create_channel(&NewChannel {
            name: "Notion".to_string(),
            kind: ChannelKind::Notion,
            encrypted_config: h.master_key.encrypt_json(&notion_ch_cfg).unwrap(),
            credential_group_key: notion_ch_cfg.credential_group_key(),
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
    let notion_dest =
        h.db.create_destination(&NewDestination {
            channel_id: notion_ch.id,
            name: "Notion DB".to_string(),
            encrypted_config: h
                .master_key
                .encrypt_json(&DestinationConfig::Notion(NotionDestinationConfig {
                    data_source_id: "11111111-2222-3333-4444-555555555555".to_string(),
                    database_id: None,
                    parent_page_id: None,
                    column_mapping: NotionColumnMapping::standard(),
                }))
                .unwrap(),
            enabled: true,
        })
        .await
        .unwrap();

    // Create route via REST API with `plugins.link-rewrite` pointing to p1_id
    let (st, _, body) = h
        .send_json(
            Method::POST,
            "/api/v1/routes",
            Some(&cookie),
            Some(json!({
                "name": "TG Shortened Route",
                "feed_ids": [feed.id],
                "destination_id": tg_dest.id,
                "options": {
                    "telegram_mode": "compact_card",
                    "link_preview": true,
                    "plugins": {
                        "link-rewrite": { "provider_id": p1_id }
                    }
                }
            })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let tg_route_id = parse_json(&body)["id"].as_i64().unwrap();

    // Verify DELETE instance returns 409 Conflict listing referencing route!
    let (st, _, body) = h
        .send_json(
            Method::DELETE,
            &format!("/api/v1/plugins/link-rewrite/instances/{p1_id}"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let conflict = parse_json(&body);
    assert_eq!(conflict["referencing_routes"][0]["id"], tg_route_id);

    // Create Notion route sharing the SAME p1_id provider -> exercises plugin_link_cache hit!
    let (st, _, body) = h
        .send_json(
            Method::POST,
            "/api/v1/routes",
            Some(&cookie),
            Some(json!({
                "name": "Notion Shortened Route",
                "feed_ids": [feed.id],
                "destination_id": notion_dest.id,
                "options": {
                    "plugins": {
                        "link-rewrite": { "provider_id": p1_id }
                    }
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let notion_route_id = parse_json(&body)["id"].as_i64().unwrap();

    // Insert rich entry containing:
    // - primary canonical_url (rewritten)
    // - external link (rewritten)
    // - existing link (`error:url` -> treated as success -> https://sho.rt/existing1)
    // - exact excluded domain (`skip.example.com` -> untouched)
    // - wildcard excluded domain (`docs.internal.org` -> untouched)
    // - YOURLS self-host link (untouched)
    // - mailto / tel / #anchor (untouched)
    // - <img src> and <video src> (NEVER touched!)
    // - failing URL (`fail-this-link.example` -> graceful degradation + diagnostic warning!)
    let self_link = format!("{}/about", yourls_srv.uri());
    let entry_html = format!(
        r##"<p>Read <a href="https://ext.example.com/article">Ext</a>, <a href="https://already-exists.example/doc">Dup</a>, <a href="https://skip.example.com/no">Skip</a>, <a href="https://docs.internal.org/guide">Wild</a>, <a href="{self_link}">Self</a>, <a href="mailto:hi@example.com">Mail</a>, <a href="tel:+12345">Tel</a>, <a href="#top">Top</a>, and <a href="https://fail-this-link.example/bad">Bad</a>.</p><p><img src="https://media.example.com/hero.png" alt="Hero"><video src="https://media.example.com/clip.mp4"></video></p>"##
    );
    let entry = match h
        .db
        .insert_entry_if_new(&NewEntry {
            feed_id: feed.id,
            fingerprint_sha256: sha256_hex(b"plugin-e2e-entry-1"),
            published_at: Some(now_unix() - 60),
            discovered_at: now_unix(),
            title: Some("Plugin Rewrite Test".to_string()),
            canonical_url: Some("https://origin.example.com/post-1".to_string()),
            author: Some("Alice".to_string()),
            content_html: Some(entry_html),
            first_sentence: Some("Short test.".to_string()),
            summary_text: Some("Summary.".to_string()),
            media: vec![MediaAttachment {
                url: "https://media.example.com/hero.png".to_string(),
                mime_type: Some("image/png".to_string()),
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

    // Enqueue Telegram + Notion tasks
    let t_tg =
        h.db.enqueue_delivery_task(tg_route_id, tg_ch.id, tg_dest.id, entry.id, now_unix() - 2)
            .await
            .unwrap()
            .unwrap();
    let t_notion =
        h.db.enqueue_delivery_task(
            notion_route_id,
            notion_ch.id,
            notion_dest.id,
            entry.id,
            now_unix() - 1,
        )
        .await
        .unwrap()
        .unwrap();

    // Process Telegram task first
    let calls_before_tg = yourls_calls.load(Ordering::SeqCst);
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    let tg_task_after = h.db.get_delivery_task(t_tg.id).await.unwrap().unwrap();
    assert_eq!(tg_task_after.status, DeliveryTaskStatus::Delivered);
    assert!(
        tg_task_after
            .diagnostic_log
            .as_deref()
            .unwrap_or("")
            .contains("Link Rewrite partial"),
        "graceful failure must log partial warning: {:?}",
        tg_task_after.diagnostic_log
    );
    let calls_after_tg = yourls_calls.load(Ordering::SeqCst);
    // 4 URLs attempted: canonical_url, ext.example.com, already-exists.example, fail-this-link.example
    assert_eq!(
        calls_after_tg - calls_before_tg,
        4,
        "only non-excluded HTTP(S) <a href> + primary link should hit YOURLS (not media/mailto/excluded/self)"
    );

    // Check Telegram payload: inline URL button uses rewritten short link, while `link_preview_options.url` keeps original!
    let tg_reqs = tg_srv.received_requests().await.unwrap();
    assert_eq!(tg_reqs.len(), 1);
    let tg_json: Value = serde_json::from_slice(&tg_reqs[0].body).unwrap();
    assert_eq!(
        tg_json["link_preview_options"]["url"], "https://origin.example.com/post-1",
        "Telegram link_preview_options.url MUST keep original canonical URL"
    );
    let btn_url = tg_json["reply_markup"]["inline_keyboard"][0][0]["url"]
        .as_str()
        .unwrap_or_default();
    assert!(
        btn_url.starts_with("https://sho.rt/"),
        "Telegram CompactCard inline button should carry shortened link: {tg_json}"
    );

    // Process Notion task second -> cached URLs MUST NOT hit YOURLS again (only the failed URL retries)!
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    let notion_task_after = h.db.get_delivery_task(t_notion.id).await.unwrap().unwrap();
    assert_eq!(notion_task_after.status, DeliveryTaskStatus::Delivered);
    let calls_after_notion = yourls_calls.load(Ordering::SeqCst);
    assert_eq!(
        calls_after_notion,
        calls_after_tg + 1,
        "3 succeeded URLs must come from plugin_link_cache; only the previously failed URL retries"
    );

    // Verify Notion payload: Link keeps ORIGINAL canonical_url, Short URL stores rewritten short link,
    // body rewrites ext + existing1, keeps excluded/self/failed/media untouched!
    let notion_reqs = notion_srv.received_requests().await.unwrap();
    assert_eq!(notion_reqs.len(), 1);
    let n_json: Value = serde_json::from_slice(&notion_reqs[0].body).unwrap();
    assert_eq!(
        n_json["properties"]["Link"]["url"], "https://origin.example.com/post-1",
        "Notion Link property MUST preserve original canonical URL"
    );
    assert!(
        n_json["properties"]["Short URL"]["url"]
            .as_str()
            .unwrap()
            .starts_with("https://sho.rt/"),
        "Notion Short URL property must receive rewritten short link: {}",
        n_json["properties"]
    );
    let md = n_json["markdown"].as_str().unwrap();
    assert!(
        md.contains("https://sho.rt/existing1"),
        "error:url shorturl reused: {md}"
    );
    assert!(
        md.contains("https://skip.example.com/no"),
        "exact excluded domain preserved: {md}"
    );
    assert!(
        md.contains("https://docs.internal.org/guide"),
        "wildcard excluded domain preserved: {md}"
    );
    assert!(md.contains(&self_link), "YOURLS self-host preserved: {md}");
    assert!(
        md.contains("https://fail-this-link.example/bad"),
        "failed URL degraded gracefully: {md}"
    );
    assert!(
        md.contains("https://media.example.com/hero.png"),
        "image src NEVER rewritten: {md}"
    );

    // Clear provider cache via API
    let (st, _, body) = h
        .send_json(
            Method::DELETE,
            &format!("/api/v1/plugins/link-rewrite/instances/{p1_id}/cache"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(parse_json(&body)["cleared_entries"], 3);

    // 6. Strict mode: when any URL fails, task moves to `retrying` instead of delivering!
    let mut strict_opts = RouteOptions::default();
    strict_opts
        .plugins
        .insert("link-rewrite".to_string(), json!({ "provider_id": p2_id }));
    let strict_route =
        h.db.create_route(&NewRoute {
            name: "Strict Route".to_string(),
            feed_ids: vec![feed.id],
            match_tags: Vec::new(),
            channel_id: notion_ch.id,
            destination_id: notion_dest.id,
            include_keywords: Vec::new(),
            exclude_keywords: Vec::new(),
            include_regex: None,
            exclude_regex: None,
            options: strict_opts,
            enabled: true,
        })
        .await
        .unwrap();
    let t_strict =
        h.db.enqueue_delivery_task(
            strict_route.id,
            notion_ch.id,
            notion_dest.id,
            entry.id,
            now_unix() - 1,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    let strict_after = h.db.get_delivery_task(t_strict.id).await.unwrap().unwrap();
    assert_eq!(
        strict_after.status,
        DeliveryTaskStatus::Retrying,
        "strict_mode must retry task on link shortening error"
    );

    // 7. 50-URL cap: entry with 65 unique links must send AT MOST 50 YOURLS calls!
    let mut big_html = String::new();
    for i in 0..65 {
        big_html.push_str(&format!(
            "<p><a href=\"https://cap.example.com/item-{i}\">Item {i}</a></p>"
        ));
    }
    let big_entry = match h
        .db
        .insert_entry_if_new(&NewEntry {
            feed_id: feed.id,
            fingerprint_sha256: sha256_hex(b"plugin-cap-65"),
            published_at: Some(now_unix()),
            discovered_at: now_unix(),
            title: Some("Cap Test".to_string()),
            canonical_url: None,
            author: None,
            content_html: Some(big_html),
            first_sentence: None,
            summary_text: None,
            media: Vec::new(),
        })
        .await
        .unwrap()
    {
        feedcourier::db::models::EntryInsertOutcome::Inserted(e) => *e,
        _ => unreachable!(),
    };
    let before_cap_calls = yourls_calls.load(Ordering::SeqCst);
    let t_cap =
        h.db.enqueue_delivery_task(
            notion_route_id,
            notion_ch.id,
            notion_dest.id,
            big_entry.id,
            now_unix() - 1,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    assert_eq!(
        h.db.get_delivery_task(t_cap.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        DeliveryTaskStatus::Delivered
    );
    assert_eq!(
        yourls_calls.load(Ordering::SeqCst) - before_cap_calls,
        50,
        "per-entry URL shortening cap must stop at 50 URLs"
    );

    // 8. Disabling global plugin skips hook cleanly while preserving route plugin options;
    //    unassigning p1_id lets DELETE succeed!
    let (st, _, _) = h
        .send_json(
            Method::PATCH,
            "/api/v1/plugins/link-rewrite",
            Some(&cookie),
            Some(json!({ "enabled": false })),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let calls_before_disabled = yourls_calls.load(Ordering::SeqCst);
    let disabled_entry = match h
        .db
        .insert_entry_if_new(&NewEntry {
            feed_id: feed.id,
            fingerprint_sha256: sha256_hex(b"plugin-disabled-entry"),
            published_at: Some(now_unix()),
            discovered_at: now_unix(),
            title: Some("Disabled Plugin Entry".to_string()),
            canonical_url: Some("https://nocall.example.com/1".to_string()),
            author: None,
            content_html: Some(
                "<p><a href=\"https://nocall.example.com/2\">Link</a></p>".to_string(),
            ),
            first_sentence: None,
            summary_text: None,
            media: Vec::new(),
        })
        .await
        .unwrap()
    {
        feedcourier::db::models::EntryInsertOutcome::Inserted(e) => *e,
        _ => unreachable!(),
    };
    let t_dis =
        h.db.enqueue_delivery_task(
            notion_route_id,
            notion_ch.id,
            notion_dest.id,
            disabled_entry.id,
            now_unix() - 1,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    assert_eq!(
        h.db.get_delivery_task(t_dis.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        DeliveryTaskStatus::Delivered
    );
    assert_eq!(
        yourls_calls.load(Ordering::SeqCst),
        calls_before_disabled,
        "globally disabled plugin must not invoke YOURLS"
    );

    // Clean up p3_id (unreferenced) -> 200 OK
    let (st, _, _) = h
        .send_json(
            Method::DELETE,
            &format!("/api/v1/plugins/link-rewrite/instances/{p3_id}"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
}

// =============================================================================
// 2. MCP `list_plugins` (readonly, zero secrets) + `configure_route` plugins
// =============================================================================

#[tokio::test]
async fn mcp_list_plugins_zero_secret_leak_and_route_plugin_config() {
    let h = V3Harness::new_with_allowlist(false, None, None).await;
    let cookie = h.setup_admin().await;

    // Enable plugin + add provider with secret signature
    let (st, _, _) = h
        .send_json(
            Method::PATCH,
            "/api/v1/plugins/link-rewrite",
            Some(&cookie),
            Some(json!({ "enabled": true })),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _, body) = h
        .send_json(
            Method::POST,
            "/api/v1/plugins/link-rewrite/instances",
            Some(&cookie),
            Some(json!({
                "name": "Corporate YOURLS",
                "enabled": true,
                "config": {
                    "api_url": "https://sho.example.com/yourls-api.php",
                    "auth_mode": "signature",
                    "signature": "SUPER_SECRET_YOURLS_SIG_999"
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let inst_id = parse_json(&body)["id"].as_i64().unwrap();

    // Create readonly + admin MCP tokens via REST API
    let (st, _, body) = h
        .send_json(
            Method::POST,
            "/api/v1/mcp-tokens",
            Some(&cookie),
            Some(json!({ "name": "RO", "scope": "readonly" })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let ro_tok = parse_json(&body)["plaintext_token"]
        .as_str()
        .unwrap()
        .to_string();

    let (st, _, body) = h
        .send_json(
            Method::POST,
            "/api/v1/mcp-tokens",
            Some(&cookie),
            Some(json!({ "name": "ADM", "scope": "admin" })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let admin_tok = parse_json(&body)["plaintext_token"]
        .as_str()
        .unwrap()
        .to_string();

    // Call MCP `list_plugins` with readonly token
    let init_req = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(HOST, "localhost:8080")
        .header("authorization", format!("Bearer {ro_tok}"))
        .header("accept", "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": { "name": "test", "version": "1.0" }
                }
            }))
            .unwrap(),
        ))
        .unwrap();
    let init_resp = h.mcp_router.clone().oneshot(init_req).await.unwrap();
    assert_eq!(init_resp.status(), StatusCode::OK);
    let session_id = init_resp
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let call_req = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(HOST, "localhost:8080")
        .header("authorization", format!("Bearer {ro_tok}"))
        .header("mcp-session-id", &session_id)
        .header("accept", "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": { "name": "list_plugins", "arguments": {} }
            }))
            .unwrap(),
        ))
        .unwrap();
    let call_resp = h.mcp_router.clone().oneshot(call_req).await.unwrap();
    assert_eq!(call_resp.status(), StatusCode::OK);
    let raw_text = String::from_utf8(
        call_resp
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(
        raw_text.contains("link-rewrite") && raw_text.contains("Corporate YOURLS"),
        "list_plugins must list built-in plugin and provider name: {raw_text}"
    );
    assert!(
        !raw_text.contains("SUPER_SECRET_YOURLS_SIG_999"),
        "MCP list_plugins MUST NEVER expose plugin secrets: {raw_text}"
    );

    // Admin token can configure route with `plugins` and rejects non-existent instance_id
    let tg_cfg = ChannelConfig::Telegram(TelegramChannelConfig {
        bot_token: "123:mcp_tok".to_string(),
        api_base_url: None,
    });
    let ch =
        h.db.create_channel(&NewChannel {
            name: "TG".to_string(),
            kind: ChannelKind::Telegram,
            encrypted_config: h.master_key.encrypt_json(&tg_cfg).unwrap(),
            credential_group_key: tg_cfg.credential_group_key(),
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
    let dest =
        h.db.create_destination(&NewDestination {
            channel_id: ch.id,
            name: "Chat".to_string(),
            encrypted_config: h
                .master_key
                .encrypt_json(&DestinationConfig::Telegram(TelegramDestinationConfig {
                    chat_id: "1001".to_string(),
                    message_thread_id: None,
                }))
                .unwrap(),
            enabled: true,
        })
        .await
        .unwrap();

    let adm_init = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(HOST, "localhost:8080")
        .header("authorization", format!("Bearer {admin_tok}"))
        .header("accept", "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": 10,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": { "name": "test", "version": "1.0" }
                }
            }))
            .unwrap(),
        ))
        .unwrap();
    let adm_init_resp = h.mcp_router.clone().oneshot(adm_init).await.unwrap();
    let adm_sid = adm_init_resp
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let create_route_req = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(HOST, "localhost:8080")
        .header("authorization", format!("Bearer {admin_tok}"))
        .header("mcp-session-id", &adm_sid)
        .header("accept", "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": 11,
                "method": "tools/call",
                "params": {
                    "name": "configure_route",
                    "arguments": {
                        "action": "create",
                        "name": "MCP Plugin Route",
                        "destination_id": dest.id,
                        "plugins": {
                            "link-rewrite": { "provider_id": inst_id }
                        }
                    }
                }
            }))
            .unwrap(),
        ))
        .unwrap();
    let cr_resp = h
        .mcp_router
        .clone()
        .oneshot(create_route_req)
        .await
        .unwrap();
    assert_eq!(cr_resp.status(), StatusCode::OK);
    let cr_body = String::from_utf8(
        cr_resp
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(
        cr_body.contains("MCP Plugin Route") && cr_body.contains("link-rewrite"),
        "MCP configure_route should persist plugins: {cr_body}"
    );
    let _ = McpTokenScope::Readonly;
}

// =============================================================================
// 3. SMTP Compliance: Manual Double Opt-In, Public `/v` & `/u`, Single-`To`, RFC 8058
// =============================================================================

#[tokio::test]
async fn smtp_compliance_manual_verification_public_endpoints_and_per_recipient_delivery() {
    reset_public_rate_limiter_for_tests();
    let (smtp_port, captured, smtp_handle) = spawn_multi_smtp_server(None).await;
    let h = V3Harness::new_with_allowlist(true, None, None).await;
    let cookie = h.setup_admin().await;

    // Set PUBLIC_URL via PUT /api/v1/settings
    let (st, _, body) = h
        .send_json(
            Method::PUT,
            "/api/v1/settings",
            Some(&cookie),
            Some(json!({ "public_url": "https://feeds.example.org" })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(parse_json(&body)["public_url"], "https://feeds.example.org");

    // 1. Create SMTP account with `sender_identity` + initial destination with 2 recipients.
    let (st, _, body) = h
        .send_json(
            Method::POST,
            "/api/v1/channels",
            Some(&cookie),
            Some(json!({
                "name": "Compliance SMTP",
                "kind": "smtp",
                "config": {
                    "kind": "smtp",
                    "host": "127.0.0.1",
                    "port": smtp_port,
                    "security": "plain",
                    "from_address": "newsletter@verified.example.org",
                    "sender_identity": "Ellery Labs · 123 Main St, Example City"
                },
                "initial_destination": {
                    "name": "Digest Subscribers",
                    "config": {
                        "kind": "smtp",
                        "to_addresses": ["alice@inbox.example.com", "bob@inbox.example.com"],
                        "reply_to": "reply@verified.example.org"
                    }
                },
                "min_interval_secs": 0,
                "jitter_max_secs": 0
            })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let ch_dto = parse_json(&body);
    let ch_id = ch_dto["id"].as_i64().unwrap();
    let dest_id = ch_dto["destinations"][0]["id"].as_i64().unwrap();

    // INVARIANT: Creating SMTP destination MUST NOT auto-send verification email!
    assert_eq!(
        captured.lock().unwrap().len(),
        0,
        "creating SMTP destination must NEVER auto-send verification email"
    );

    // List recipients -> both start as `unverified`
    let (st, _, body) = h
        .send_json(
            Method::GET,
            &format!("/api/v1/destinations/{dest_id}/recipients"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let recipients = parse_json(&body);
    let r_alice_id = recipients
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["email"] == "alice@inbox.example.com")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let r_bob_id = recipients
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["email"] == "bob@inbox.example.com")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    assert_eq!(recipients[0]["status"], "unverified");
    assert_eq!(recipients[1]["status"], "unverified");

    // 2. Enqueue delivery while both recipients are unverified -> task defers without sending!
    let feed =
        h.db.create_feed(&NewFeed::new("https://news.example.com/rss"))
            .await
            .unwrap();
    let route =
        h.db.create_route(&NewRoute {
            name: "Digest Route".to_string(),
            feed_ids: vec![feed.id],
            match_tags: Vec::new(),
            channel_id: ch_id,
            destination_id: dest_id,
            include_keywords: Vec::new(),
            exclude_keywords: Vec::new(),
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions::default(),
            enabled: true,
        })
        .await
        .unwrap();
    let entry = match h
        .db
        .insert_entry_if_new(&NewEntry {
            feed_id: feed.id,
            fingerprint_sha256: sha256_hex(b"smtp-compliance-entry-1"),
            published_at: Some(now_unix() - 60),
            discovered_at: now_unix(),
            title: Some("Compliant Digest Issue #1".to_string()),
            canonical_url: Some("https://news.example.com/issue-1".to_string()),
            author: Some("Editor".to_string()),
            content_html: Some("<p>Hello verified readers!</p>".to_string()),
            first_sentence: Some("Hello verified readers!".to_string()),
            summary_text: Some("Hello verified readers!".to_string()),
            media: Vec::new(),
        })
        .await
        .unwrap()
    {
        feedcourier::db::models::EntryInsertOutcome::Inserted(e) => *e,
        _ => unreachable!(),
    };
    let t1 =
        h.db.enqueue_delivery_task(route.id, ch_id, dest_id, entry.id, now_unix() - 5)
            .await
            .unwrap()
            .unwrap();
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    let t1_deferred = h.db.get_delivery_task(t1.id).await.unwrap().unwrap();
    assert_eq!(t1_deferred.status, DeliveryTaskStatus::Deferred);
    assert!(
        t1_deferred
            .diagnostic_log
            .as_deref()
            .unwrap_or("")
            .contains("Awaiting SMTP recipient verification"),
        "expected verification defer message: {:?}",
        t1_deferred.diagnostic_log
    );
    assert_eq!(captured.lock().unwrap().len(), 0);

    // 3. Manually trigger verification email for Alice -> 6-digit code + `/v/{token}` link!
    let (st, _, body) = h
        .send_json(
            Method::POST,
            &format!("/api/v1/destinations/{dest_id}/recipients/{r_alice_id}/send-verification"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(parse_json(&body)["recipient"]["status"], "pending");

    // Immediate resend hits 60s cooldown -> 429!
    let (st, _, _) = h
        .send_json(
            Method::POST,
            &format!("/api/v1/destinations/{dest_id}/recipients/{r_alice_id}/send-verification"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);

    let alice_verify_mail = captured.lock().unwrap().last().unwrap().raw_data.clone();
    let alice_verify_path =
        extract_public_token_path(&alice_verify_mail, "https://feeds.example.org/v/");

    // Public `GET /v/{token}` MUST be read-only (status stays `pending`, security headers set)!
    let (st, hdrs, html) = h
        .send_public(Method::GET, &alice_verify_path, "203.0.113.10", None, None)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        hdrs.get("x-robots-tag").unwrap().to_str().unwrap(),
        "noindex, nofollow"
    );
    assert!(
        html.contains("a***e@inbox.example.com"),
        "masked email in GET /v: {html}"
    );
    assert_eq!(
        h.db.get_smtp_recipient(r_alice_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        SmtpRecipientStatus::Pending,
        "GET /v/{{token}} MUST NOT mutate recipient status!"
    );

    // Public `POST /v/{token}` confirms Alice (`consent_method = "link"`) and wakes deferred task!
    let (st, _, _) = h
        .send_public(Method::POST, &alice_verify_path, "203.0.113.10", None, None)
        .await;
    assert_eq!(st, StatusCode::OK);
    let alice_row = h.db.get_smtp_recipient(r_alice_id).await.unwrap().unwrap();
    assert_eq!(alice_row.status, SmtpRecipientStatus::Verified);
    assert_eq!(alice_row.consent_method.as_deref(), Some("link"));

    // 4. Verify Bob via 6-digit code (wrong code rejected first, valid code succeeds!)
    let (st, _, _) = h
        .send_json(
            Method::POST,
            &format!("/api/v1/destinations/{dest_id}/recipients/{r_bob_id}/send-verification"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let bob_verify_mail = captured.lock().unwrap().last().unwrap().raw_data.clone();
    let bob_code = extract_6_digit_code(&bob_verify_mail);

    let wrong_code = if bob_code == "000000" {
        "111111"
    } else {
        "000000"
    };
    let (st, _, _) = h
        .send_json(
            Method::POST,
            &format!("/api/v1/destinations/{dest_id}/recipients/{r_bob_id}/verify"),
            Some(&cookie),
            Some(json!({ "code": wrong_code })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(
        h.db.get_smtp_recipient(r_bob_id)
            .await
            .unwrap()
            .unwrap()
            .code_attempts,
        1
    );

    let (st, _, body) = h
        .send_json(
            Method::POST,
            &format!("/api/v1/destinations/{dest_id}/recipients/{r_bob_id}/verify"),
            Some(&cookie),
            Some(json!({ "code": bob_code })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(parse_json(&body)["status"], "verified");
    assert_eq!(parse_json(&body)["consent_method"], "code");

    // 5. Deliver task t1 (woken automatically by verification!) -> 2 separate single-`To` emails!
    let before_delivery = captured.lock().unwrap().len();
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    let t1_done = h.db.get_delivery_task(t1.id).await.unwrap().unwrap();
    assert_eq!(t1_done.status, DeliveryTaskStatus::Delivered);

    let sent_mails: Vec<CapturedMail> = captured.lock().unwrap()[before_delivery..].to_vec();
    assert_eq!(
        sent_mails.len(),
        2,
        "must send 1 distinct single-To email per verified recipient"
    );
    for m in &sent_mails {
        assert_eq!(m.rcpt_to.len(), 1, "each envelope must have single RCPT TO");
        let decoded = decode_qp_best_effort(&m.raw_data);
        assert!(decoded.contains("Auto-Submitted: auto-generated"));
        assert!(decoded.contains("List-Id:"));
        assert!(decoded.contains("List-Unsubscribe: <https://feeds.example.org/u/"));
        assert!(decoded.contains("List-Unsubscribe-Post: List-Unsubscribe=One-Click"));
        assert!(decoded.contains("Ellery Labs"));
    }

    // Extract Alice's and Bob's unique unsubscribe URLs
    let alice_mail = sent_mails
        .iter()
        .find(|m| {
            m.rcpt_to[0]
                .to_ascii_lowercase()
                .contains("alice@inbox.example.com")
        })
        .unwrap();
    let bob_mail = sent_mails
        .iter()
        .find(|m| {
            m.rcpt_to[0]
                .to_ascii_lowercase()
                .contains("bob@inbox.example.com")
        })
        .unwrap();
    let alice_unsub =
        extract_public_token_path(&alice_mail.raw_data, "https://feeds.example.org/u/");
    let bob_unsub = extract_public_token_path(&bob_mail.raw_data, "https://feeds.example.org/u/");
    assert_ne!(alice_unsub, bob_unsub);

    // 6. Tampered token -> 404
    let (st, _, _) = h
        .send_public(
            Method::GET,
            "/u/invalid-tampered-token",
            "203.0.113.11",
            None,
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // `GET /u/{alice}` is strictly read-only!
    let (st, _, html) = h
        .send_public(Method::GET, &alice_unsub, "203.0.113.11", None, None)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert!(html.contains("a***e@inbox.example.com"));
    assert_eq!(
        h.db.get_smtp_recipient(r_alice_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        SmtpRecipientStatus::Verified,
        "GET /u/{{token}} MUST NOT unsubscribe!"
    );

    // RFC 8058 `POST /u/{alice}` with `List-Unsubscribe=One-Click` -> unsubscribes Alice ONLY; dest stays enabled!
    let (st, _, _) = h
        .send_public(
            Method::POST,
            &alice_unsub,
            "203.0.113.11",
            Some("List-Unsubscribe=One-Click"),
            Some("application/x-www-form-urlencoded"),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let alice_after = h.db.get_smtp_recipient(r_alice_id).await.unwrap().unwrap();
    assert_eq!(alice_after.status, SmtpRecipientStatus::Unsubscribed);
    assert_eq!(alice_after.revoked_method.as_deref(), Some("one_click"));
    assert!(
        h.db.get_destination(dest_id)
            .await
            .unwrap()
            .unwrap()
            .enabled,
        "destination must stay enabled while Bob is still verified"
    );

    // 7. Unset PUBLIC_URL -> next delivery to Bob uses `mailto:` List-Unsubscribe fallback!
    h.db.set_setting_typed(SETTING_PUBLIC_URL, &Option::<String>::None)
        .await
        .unwrap();
    let entry2 = match h
        .db
        .insert_entry_if_new(&NewEntry {
            feed_id: feed.id,
            fingerprint_sha256: sha256_hex(b"smtp-compliance-entry-2"),
            published_at: Some(now_unix()),
            discovered_at: now_unix(),
            title: Some("Mailto Fallback Issue #2".to_string()),
            canonical_url: Some("https://news.example.com/issue-2".to_string()),
            author: None,
            content_html: Some("<p>Only Bob should receive this.</p>".to_string()),
            first_sentence: None,
            summary_text: None,
            media: Vec::new(),
        })
        .await
        .unwrap()
    {
        feedcourier::db::models::EntryInsertOutcome::Inserted(e) => *e,
        _ => unreachable!(),
    };
    let before_e2 = captured.lock().unwrap().len();
    let t2 =
        h.db.enqueue_delivery_task(route.id, ch_id, dest_id, entry2.id, now_unix() - 1)
            .await
            .unwrap()
            .unwrap();
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    assert_eq!(
        h.db.get_delivery_task(t2.id).await.unwrap().unwrap().status,
        DeliveryTaskStatus::Delivered
    );
    let e2_mails: Vec<CapturedMail> = captured.lock().unwrap()[before_e2..].to_vec();
    assert_eq!(e2_mails.len(), 1, "unsubscribed Alice must be skipped");
    assert!(
        e2_mails[0].rcpt_to[0]
            .to_ascii_lowercase()
            .contains("bob@inbox.example.com")
    );
    assert!(
        e2_mails[0]
            .raw_data
            .contains("List-Unsubscribe: <mailto:newsletter@verified.example.org"),
        "without PUBLIC_URL must fall back to mailto: List-Unsubscribe:\n{}",
        e2_mails[0].raw_data
    );
    assert!(
        !e2_mails[0].raw_data.contains("List-Unsubscribe-Post:"),
        "mailto-only List-Unsubscribe must not emit RFC 8058 One-Click header"
    );

    // 8. Bob unsubscribes too (last remaining recipient) -> destination AUTO-PAUSES (`enabled = false`)!
    let (st, _, _) = h
        .send_public(Method::POST, &bob_unsub, "203.0.113.12", None, None)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        !h.db
            .get_destination(dest_id)
            .await
            .unwrap()
            .unwrap()
            .enabled,
        "destination MUST auto-pause when all recipients are unsubscribed"
    );

    // 9. Admin reactivates Alice -> status resets to `unverified` (must re-verify before receiving mail!)
    let (st, _, body) = h
        .send_json(
            Method::POST,
            &format!("/api/v1/destinations/{dest_id}/recipients/{r_alice_id}/reactivate"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(parse_json(&body)["status"], "unverified");

    smtp_handle.abort();
}

// =============================================================================
// 4. SMTP Synchronous 5xx Hard-Bounce, Checkpoint Resume & Legacy Backfill
// =============================================================================

#[tokio::test]
async fn smtp_compliance_hard_bounce_checkpoint_resume_and_legacy_backfill() {
    let (smtp_port, captured, smtp_handle) =
        spawn_multi_smtp_server(Some("bad@bounce.example.com".to_string())).await;
    let h = V3Harness::new_with_allowlist(true, None, None).await;

    // 1. Legacy backfill test: insert an SMTP destination directly in SQLite (simulating v0.2.x upgrade),
    //    reset `smtp_recipient_model_version`, run `backfill_legacy_smtp_recipients` -> `verified / legacy`!
    let smtp_cfg = ChannelConfig::Smtp(SmtpChannelConfig {
        host: "127.0.0.1".to_string(),
        port: smtp_port,
        username: None,
        password: None,
        security: SmtpSecurity::Plain,
        from_address: "alerts@legacy.example.org".to_string(),
        sender_identity: None,
    });
    let ch =
        h.db.create_channel(&NewChannel {
            name: "Legacy SMTP".to_string(),
            kind: ChannelKind::Smtp,
            encrypted_config: h.master_key.encrypt_json(&smtp_cfg).unwrap(),
            credential_group_key: smtp_cfg.credential_group_key(),
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
    let dest =
        h.db.create_destination(&NewDestination {
            channel_id: ch.id,
            name: "Mixed Legacy List".to_string(),
            encrypted_config: h
                .master_key
                .encrypt_json(&DestinationConfig::Smtp(SmtpDestinationConfig {
                    to_addresses: vec![
                        "good@ok.example.com".to_string(),
                        "bad@bounce.example.com".to_string(),
                    ],
                    reply_to: None,
                }))
                .unwrap(),
            enabled: true,
        })
        .await
        .unwrap();

    // Clear version flag to simulate pre-v0.3.0 upgrade startup
    h.db.delete_setting(feedcourier::db::models::SETTING_SMTP_RECIPIENT_MODEL_VERSION)
        .await
        .unwrap();
    let backfilled =
        h.db.backfill_legacy_smtp_recipients(&h.master_key)
            .await
            .unwrap();
    assert_eq!(backfilled, 2);
    // Idempotent second run returns 0
    assert_eq!(
        h.db.backfill_legacy_smtp_recipients(&h.master_key)
            .await
            .unwrap(),
        0
    );

    let recs = h.db.list_smtp_recipients(dest.id).await.unwrap();
    assert_eq!(recs.len(), 2);
    assert!(
        recs.iter()
            .all(|r| r.status == SmtpRecipientStatus::Verified)
    );
    assert!(
        recs.iter()
            .all(|r| r.consent_method.as_deref() == Some("legacy"))
    );

    // 2. Deliver entry -> `good@ok` gets 250, `bad@bounce` gets 550 hard bounce!
    //    `bad@bounce` transitions to `bounced`, while task finishes `delivered` with warning.
    let feed =
        h.db.create_feed(&NewFeed::new("https://bounce.example.org/rss"))
            .await
            .unwrap();
    let route =
        h.db.create_route(&NewRoute {
            name: "Bounce Route".to_string(),
            feed_ids: vec![feed.id],
            match_tags: Vec::new(),
            channel_id: ch.id,
            destination_id: dest.id,
            include_keywords: Vec::new(),
            exclude_keywords: Vec::new(),
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions::default(),
            enabled: true,
        })
        .await
        .unwrap();
    let entry = match h
        .db
        .insert_entry_if_new(&NewEntry {
            feed_id: feed.id,
            fingerprint_sha256: sha256_hex(b"smtp-bounce-entry-1"),
            published_at: Some(now_unix()),
            discovered_at: now_unix(),
            title: Some("Partial Bounce Test".to_string()),
            canonical_url: Some("https://bounce.example.org/1".to_string()),
            author: None,
            content_html: Some("<p>Body</p>".to_string()),
            first_sentence: None,
            summary_text: None,
            media: Vec::new(),
        })
        .await
        .unwrap()
    {
        feedcourier::db::models::EntryInsertOutcome::Inserted(e) => *e,
        _ => unreachable!(),
    };

    let t1 =
        h.db.enqueue_delivery_task(route.id, ch.id, dest.id, entry.id, now_unix() - 2)
            .await
            .unwrap()
            .unwrap();
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    let t1_after = h.db.get_delivery_task(t1.id).await.unwrap().unwrap();
    assert_eq!(t1_after.status, DeliveryTaskStatus::Delivered);
    assert!(
        t1_after
            .diagnostic_log
            .as_deref()
            .unwrap_or("")
            .contains("hard-bounced"),
        "expected hard-bounce diagnostic: {:?}",
        t1_after.diagnostic_log
    );

    let bad_hmac = h.master_key.smtp_recipient_hmac("bad@bounce.example.com");
    let good_hmac = h.master_key.smtp_recipient_hmac("good@ok.example.com");
    let recs_after = h.db.list_smtp_recipients(dest.id).await.unwrap();
    let bad_row = recs_after
        .iter()
        .find(|r| r.address_hmac == bad_hmac)
        .unwrap();
    assert_eq!(bad_row.status, SmtpRecipientStatus::Bounced);
    assert_eq!(bad_row.revoked_method.as_deref(), Some("hard_bounce"));
    assert!(
        h.db.get_destination(dest.id)
            .await
            .unwrap()
            .unwrap()
            .enabled
    );

    // 3. Checkpoint resume: pre-seed `sent_recipients = [good_hmac]` on a new task -> skips `good@ok` (0 emails sent)!
    let entry2 = match h
        .db
        .insert_entry_if_new(&NewEntry {
            feed_id: feed.id,
            fingerprint_sha256: sha256_hex(b"smtp-resume-entry-2"),
            published_at: Some(now_unix()),
            discovered_at: now_unix(),
            title: Some("Checkpoint Resume Test".to_string()),
            canonical_url: None,
            author: None,
            content_html: Some("<p>Resume</p>".to_string()),
            first_sentence: None,
            summary_text: None,
            media: Vec::new(),
        })
        .await
        .unwrap()
    {
        feedcourier::db::models::EntryInsertOutcome::Inserted(e) => *e,
        _ => unreachable!(),
    };
    let t2 =
        h.db.enqueue_delivery_task(route.id, ch.id, dest.id, entry2.id, now_unix() - 2)
            .await
            .unwrap()
            .unwrap();
    let cp_json = serde_json::to_string(&DeliveryCheckpoint::Smtp {
        message_id: "<fc-resume@legacy.example.org>".to_string(),
        sent_recipients: vec![good_hmac],
    })
    .unwrap();
    h.db.save_task_checkpoint(t2.id, &cp_json).await.unwrap();
    let count_before_resume = captured.lock().unwrap().len();
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    assert_eq!(
        h.db.get_delivery_task(t2.id).await.unwrap().unwrap().status,
        DeliveryTaskStatus::Delivered
    );
    assert_eq!(
        captured.lock().unwrap().len(),
        count_before_resume,
        "already-sent recipient in checkpoint.sent_recipients must not receive duplicate mail"
    );

    // 4. All-bounced destination: single recipient gets 550 -> dead_letter AND destination auto-pauses!
    let bad_only_dest =
        h.db.create_destination(&NewDestination {
            channel_id: ch.id,
            name: "Only Bad".to_string(),
            encrypted_config: h
                .master_key
                .encrypt_json(&DestinationConfig::Smtp(SmtpDestinationConfig {
                    to_addresses: vec!["bad@bounce.example.com".to_string()],
                    reply_to: None,
                }))
                .unwrap(),
            enabled: true,
        })
        .await
        .unwrap();
    h.db.sync_smtp_recipients_for_destination(
        bad_only_dest.id,
        &[bad_hmac],
        SmtpRecipientStatus::Verified,
        Some("legacy"),
    )
    .await
    .unwrap();
    let bad_route =
        h.db.create_route(&NewRoute {
            name: "Bad Only Route".to_string(),
            feed_ids: vec![feed.id],
            match_tags: Vec::new(),
            channel_id: ch.id,
            destination_id: bad_only_dest.id,
            include_keywords: Vec::new(),
            exclude_keywords: Vec::new(),
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions::default(),
            enabled: true,
        })
        .await
        .unwrap();
    let t_bad =
        h.db.enqueue_delivery_task(
            bad_route.id,
            ch.id,
            bad_only_dest.id,
            entry.id,
            now_unix() - 1,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    assert_eq!(
        h.db.get_delivery_task(t_bad.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        DeliveryTaskStatus::DeadLetter
    );
    assert!(
        !h.db
            .get_destination(bad_only_dest.id)
            .await
            .unwrap()
            .unwrap()
            .enabled,
        "destination must auto-pause when all recipients hard-bounce"
    );

    smtp_handle.abort();
}

// =============================================================================
// 5. R2-07: Notion Lazy `database_id` -> `data_source_id` SQLite Writeback
// =============================================================================

#[tokio::test]
async fn r2_07_notion_lazy_data_source_id_writeback_persists_on_send_and_test() {
    let notion_srv = MockServer::start().await;
    let db_id = "11111111-aaaa-bbbb-cccc-222222222222";
    let real_ds_id = "33333333-dddd-eeee-ffff-444444444444";

    Mock::given(method("GET"))
        .and(path(format!("/v1/data_sources/{db_id}")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "object": "error",
            "status": 404,
            "code": "object_not_found"
        })))
        .mount(&notion_srv)
        .await;

    Mock::given(method("GET"))
        .and(path(format!("/v1/databases/{db_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "database",
            "id": db_id,
            "data_sources": [{ "id": real_ds_id, "name": "Resolved DS" }]
        })))
        .mount(&notion_srv)
        .await;

    Mock::given(method("GET"))
        .and(path(format!("/v1/data_sources/{real_ds_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "data_source",
            "id": real_ds_id,
            "title": [{ "plain_text": "Resolved DS" }],
            "properties": {
                "Title": { "type": "title" },
                "FC-ID": { "type": "rich_text" },
                "Link": { "type": "url" },
                "Short URL": { "type": "url" },
                "Source": { "type": "select" },
                "Published": { "type": "date" },
                "State": { "type": "select" },
                "Tag": { "type": "multi_select" }
            }
        })))
        .mount(&notion_srv)
        .await;

    let pages_404_count = Arc::new(AtomicUsize::new(0));
    let pages_404_clone = Arc::clone(&pages_404_count);
    Mock::given(method("POST"))
        .and(path("/v1/pages"))
        .respond_with(move |req: &MockRequest| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            if body["parent"]["data_source_id"] == db_id {
                pages_404_clone.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(404).set_body_json(json!({
                    "object": "error",
                    "status": 404,
                    "code": "object_not_found"
                }))
            } else {
                assert_eq!(body["parent"]["data_source_id"], real_ds_id);
                ResponseTemplate::new(200).set_body_json(json!({
                    "object": "page",
                    "id": "page-resolved-ok"
                }))
            }
        })
        .mount(&notion_srv)
        .await;

    let h = V3Harness::new_with_allowlist(true, Some(notion_srv.uri()), None).await;
    let cookie = h.setup_admin().await;

    let notion_ch_cfg = ChannelConfig::Notion(NotionChannelConfig {
        integration_secret: "ntn_writeback_secret".to_string(),
    });
    let ch =
        h.db.create_channel(&NewChannel {
            name: "Notion Writeback".to_string(),
            kind: ChannelKind::Notion,
            encrypted_config: h.master_key.encrypt_json(&notion_ch_cfg).unwrap(),
            credential_group_key: notion_ch_cfg.credential_group_key(),
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

    // Case A: `POST /api/v1/destinations/{id}/test` writes resolved `data_source_id` back to SQLite!
    let dest_test =
        h.db.create_destination(&NewDestination {
            channel_id: ch.id,
            name: "Test Writeback DB".to_string(),
            encrypted_config: h
                .master_key
                .encrypt_json(&DestinationConfig::Notion(NotionDestinationConfig {
                    data_source_id: db_id.to_string(),
                    database_id: None,
                    parent_page_id: None,
                    column_mapping: NotionColumnMapping::standard(),
                }))
                .unwrap(),
            enabled: true,
        })
        .await
        .unwrap();
    let (st, _, body) = h
        .send_json(
            Method::POST,
            &format!("/api/v1/destinations/{}/test", dest_test.id),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let refreshed_dest_test = h.db.get_destination(dest_test.id).await.unwrap().unwrap();
    let decrypted_test: DestinationConfig = h
        .master_key
        .decrypt_json(&refreshed_dest_test.encrypted_config)
        .unwrap();
    match decrypted_test {
        DestinationConfig::Notion(n) => {
            assert_eq!(n.data_source_id, real_ds_id);
            assert_eq!(n.database_id.as_deref(), Some(db_id));
        }
        other => panic!("expected Notion config, got {other:?}"),
    }

    // Case B: Outbox delivery resolves `db_id` -> `real_ds_id`, persists to SQLite, so 2nd delivery has 0 404s!
    let dest_send =
        h.db.create_destination(&NewDestination {
            channel_id: ch.id,
            name: "Send Writeback DB".to_string(),
            encrypted_config: h
                .master_key
                .encrypt_json(&DestinationConfig::Notion(NotionDestinationConfig {
                    data_source_id: db_id.to_string(),
                    database_id: None,
                    parent_page_id: None,
                    column_mapping: NotionColumnMapping::standard(),
                }))
                .unwrap(),
            enabled: true,
        })
        .await
        .unwrap();
    let feed =
        h.db.create_feed(&NewFeed::new("https://wb.example.org/rss"))
            .await
            .unwrap();
    let route =
        h.db.create_route(&NewRoute {
            name: "WB Route".to_string(),
            feed_ids: vec![feed.id],
            match_tags: Vec::new(),
            channel_id: ch.id,
            destination_id: dest_send.id,
            include_keywords: Vec::new(),
            exclude_keywords: Vec::new(),
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions::default(),
            enabled: true,
        })
        .await
        .unwrap();

    for i in 1..=2 {
        let entry = match h
            .db
            .insert_entry_if_new(&NewEntry {
                feed_id: feed.id,
                fingerprint_sha256: sha256_hex(format!("wb-entry-{i}").as_bytes()),
                published_at: Some(now_unix()),
                discovered_at: now_unix(),
                title: Some(format!("WB Entry {i}")),
                canonical_url: Some(format!("https://wb.example.org/{i}")),
                author: None,
                content_html: Some("<p>Hi</p>".to_string()),
                first_sentence: None,
                summary_text: None,
                media: Vec::new(),
            })
            .await
            .unwrap()
        {
            feedcourier::db::models::EntryInsertOutcome::Inserted(e) => *e,
            _ => unreachable!(),
        };
        let t =
            h.db.enqueue_delivery_task(route.id, ch.id, dest_send.id, entry.id, now_unix() - 1)
                .await
                .unwrap()
                .unwrap();
        assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
        assert_eq!(
            h.db.get_delivery_task(t.id).await.unwrap().unwrap().status,
            DeliveryTaskStatus::Delivered
        );
    }

    assert_eq!(
        pages_404_count.load(Ordering::SeqCst),
        1,
        "only the FIRST delivery should hit 404 fallback; 2nd delivery must use persisted data_source_id!"
    );
    let _ = (NetSettings::default(), ProtectedHttpClient::with_defaults());
}

// =============================================================================
// 6. QA v0.3.0 Security, Concurrency, State Machine & Parser Regression Suite
// =============================================================================

async fn spawn_custom_reply_smtp_server(
    rcpt_reply: &'static str,
    data_reply: &'static str,
) -> (
    u16,
    Arc<Mutex<Vec<CapturedMail>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let captured: Arc<Mutex<Vec<CapturedMail>>> = Arc::new(Mutex::new(Vec::new()));
    let cap_clone = Arc::clone(&captured);

    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let cap = Arc::clone(&cap_clone);
            tokio::spawn(async move {
                let (r, mut w) = stream.into_split();
                let mut reader = BufReader::new(r);
                let _ = w.write_all(b"220 mock.smtp.qa ESMTP Ready\r\n").await;
                let mut mail = CapturedMail::default();
                let mut line = String::new();
                loop {
                    line.clear();
                    let Ok(n) = reader.read_line(&mut line).await else {
                        break;
                    };
                    if n == 0 {
                        break;
                    }
                    let cmd = line.trim_end_matches(['\r', '\n']).to_string();
                    let upper = cmd.to_ascii_uppercase();
                    if upper.starts_with("EHLO") || upper.starts_with("HELO") {
                        let _ = w.write_all(b"250-mock.smtp.qa\r\n250 8BITMIME\r\n").await;
                    } else if upper.starts_with("MAIL FROM:") {
                        mail = CapturedMail::default();
                        let _ = w.write_all(b"250 2.1.0 OK\r\n").await;
                    } else if upper.starts_with("RCPT TO:") {
                        mail.rcpt_to.push(cmd.clone());
                        let _ = w.write_all(rcpt_reply.as_bytes()).await;
                    } else if upper == "DATA" {
                        let _ = w.write_all(b"354 Start mail input\r\n").await;
                        loop {
                            line.clear();
                            let Ok(dn) = reader.read_line(&mut line).await else {
                                break;
                            };
                            if dn == 0 || line == ".\r\n" || line == ".\n" {
                                break;
                            }
                            mail.raw_data.push_str(&line);
                        }
                        cap.lock().unwrap().push(mail.clone());
                        let _ = w.write_all(data_reply.as_bytes()).await;
                    } else if upper == "RSET" {
                        mail = CapturedMail::default();
                        let _ = w.write_all(b"250 2.0.0 Reset\r\n").await;
                    } else if upper == "QUIT" {
                        let _ = w.write_all(b"221 2.0.0 Bye\r\n").await;
                        break;
                    } else {
                        let _ = w.write_all(b"250 OK\r\n").await;
                    }
                }
            });
        }
    });

    (port, captured, handle)
}

#[tokio::test]
async fn qa01_qa02_qa05_smtp_autoescape_bounce_classification_and_zero_pii_diagnostics() {
    use feedcourier::channels::smtp::{SmtpSender, is_recipient_hard_bounce_parts};
    use feedcourier::db::models::{Entry, Feed, FeedHealthStatus};

    // 1. QA-01: RSS title, feed title, author, and sender_identity are strictly HTML-escaped in
    //    rendered emails and verification emails (while valid `https://` links remain intact).
    let xss = r#"<img src=x onerror=alert(1)>"#;
    let smtp_cfg = SmtpChannelConfig {
        host: "127.0.0.1".to_string(),
        port: 2525,
        username: None,
        password: None,
        security: SmtpSecurity::Plain,
        from_address: "alerts@verified.example.com".to_string(),
        sender_identity: Some(format!("Corp {xss}")),
    };
    let smtp_dest = SmtpDestinationConfig {
        to_addresses: vec!["reader@example.org".to_string()],
        reply_to: None,
    };
    let feed = Feed {
        id: 1,
        url: "https://evil.example/rss".to_string(),
        title: format!("Feed {xss}"),
        site_url: Some("https://evil.example".to_string()),
        favicon_url: None,
        tags_json: "[]".to_string(),
        poll_interval_secs: 1800,
        fetch_full_content: false,
        use_proxy: false,
        etag: None,
        last_modified: None,
        health_status: FeedHealthStatus::Active,
        consecutive_failures: 0,
        next_poll_at: None,
        last_error: None,
        silent_init: false,
        created_at: now_unix(),
        updated_at: now_unix(),
    };
    let entry = Entry {
        id: 1,
        feed_id: 1,
        fingerprint_sha256: sha256_hex(b"xss-entry"),
        published_at: Some(now_unix()),
        discovered_at: now_unix(),
        title: Some(format!("Headline {xss}")),
        canonical_url: Some("https://evil.example/post?a=1&b=2".to_string()),
        author: Some(format!("Author {xss}")),
        content_html: Some("<p>Safe sanitized body</p>".to_string()),
        first_sentence: None,
        summary_text: None,
        media_json: None,
        payload_purged_at: None,
    };
    let built = SmtpSender::build_entry_email(&smtp_cfg, &smtp_dest, &feed, &entry, 1, None)
        .expect("build email");
    assert!(
        !built.html_body.contains("<img src=x onerror=alert(1)>"),
        "raw XSS payload must NEVER appear unescaped in email HTML:\n{}",
        built.html_body
    );
    assert!(
        built
            .html_body
            .contains("&lt;img src=x onerror=alert(1)&gt;"),
        "XSS payload must be HTML-escaped in email HTML:\n{}",
        built.html_body
    );
    assert!(
        built
            .html_body
            .contains("href=\"https://evil.example/post?a=1&amp;b=2\""),
        "URL slashes must remain valid while `&` is escaped in href:\n{}",
        built.html_body
    );

    // Verification email also auto-escapes destination name and sender_identity.
    let (v_port, v_captured, v_handle) = spawn_multi_smtp_server(None).await;
    let mut v_cfg = smtp_cfg.clone();
    v_cfg.port = v_port;
    SmtpSender::new()
        .send_verification_email(
            &v_cfg,
            "victim@example.org",
            &format!("Dest {xss}"),
            "123456",
            Some("https://fc.example.com/v/tok?a=1&b=2"),
            feedcourier::db::models::AppLocale::En,
        )
        .await
        .expect("verification email");
    let raw_verify = v_captured.lock().unwrap()[0].raw_data.clone();
    let unfolded_verify = raw_verify.replace("=\r\n", "").replace("=\n", "");
    assert!(
        !unfolded_verify.contains("<img src=x onerror=alert(1)>"),
        "verification email must escape destination_name and sender_identity"
    );
    v_handle.abort();

    // 2. QA-02: Pure bounce classifier rejects auth/quota/content/policy 5xx and accepts 5.1.1 etc.
    assert!(!is_recipient_hard_bounce_parts(
        Some(535),
        "535 5.7.8 Authentication credentials invalid"
    ));
    assert!(!is_recipient_hard_bounce_parts(
        Some(552),
        "552 5.3.4 Message size exceeds fixed limit"
    ));
    assert!(!is_recipient_hard_bounce_parts(
        Some(554),
        "554 5.7.1 Message rejected as spam"
    ));
    assert!(!is_recipient_hard_bounce_parts(
        Some(550),
        "550 5.4.5 Daily user sending limit exceeded"
    ));
    assert!(!is_recipient_hard_bounce_parts(
        Some(550),
        "550 5.7.1 DMARC policy rejected"
    ));
    assert!(!is_recipient_hard_bounce_parts(
        Some(550),
        "550 5.1.7 Bad sender mailbox"
    ));
    assert!(is_recipient_hard_bounce_parts(
        Some(550),
        "550 5.1.1 <bad@example.com>: Recipient address rejected: User unknown"
    ));
    assert!(is_recipient_hard_bounce_parts(
        Some(553),
        "553 5.1.3 Invalid recipient address"
    ));

    // 3. QA-02 + QA-05 live SMTP test: 552 size-limit error MUST NOT bounce verified recipients!
    let (err552_port, _cap552, err552_handle) =
        spawn_custom_reply_smtp_server("250 2.1.5 OK\r\n", "552 5.3.4 Message too large\r\n").await;
    let h = V3Harness::new_with_allowlist(true, None, None).await;
    let ch552_cfg = ChannelConfig::Smtp(SmtpChannelConfig {
        host: "127.0.0.1".to_string(),
        port: err552_port,
        username: None,
        password: None,
        security: SmtpSecurity::Plain,
        from_address: "alerts@fc.test".to_string(),
        sender_identity: None,
    });
    let ch552 =
        h.db.create_channel(&NewChannel {
            name: "SMTP 552".to_string(),
            kind: ChannelKind::Smtp,
            encrypted_config: h.master_key.encrypt_json(&ch552_cfg).unwrap(),
            credential_group_key: ch552_cfg.credential_group_key(),
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
    let dest552 =
        h.db.create_destination(&NewDestination {
            channel_id: ch552.id,
            name: "Verified List".to_string(),
            encrypted_config: h
                .master_key
                .encrypt_json(&DestinationConfig::Smtp(SmtpDestinationConfig {
                    to_addresses: vec!["vip@example.org".to_string()],
                    reply_to: None,
                }))
                .unwrap(),
            enabled: true,
        })
        .await
        .unwrap();
    let hmac_vip = h.master_key.smtp_recipient_hmac("vip@example.org");
    let recips =
        h.db.sync_smtp_recipients_for_destination(
            dest552.id,
            &[hmac_vip],
            SmtpRecipientStatus::Verified,
            Some("code"),
        )
        .await
        .unwrap();
    let feed_row =
        h.db.create_feed(&NewFeed::new("https://qa02.example/rss"))
            .await
            .unwrap();
    let route_row =
        h.db.create_route(&NewRoute {
            name: "QA02 Route".to_string(),
            feed_ids: vec![feed_row.id],
            match_tags: Vec::new(),
            channel_id: ch552.id,
            destination_id: dest552.id,
            include_keywords: Vec::new(),
            exclude_keywords: Vec::new(),
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions::default(),
            enabled: true,
        })
        .await
        .unwrap();
    let entry_row = match h
        .db
        .insert_entry_if_new(&NewEntry {
            feed_id: feed_row.id,
            fingerprint_sha256: sha256_hex(b"qa02-552-entry"),
            published_at: Some(now_unix()),
            discovered_at: now_unix(),
            title: Some("Huge Post".to_string()),
            canonical_url: Some("https://qa02.example/huge".to_string()),
            author: None,
            content_html: Some("<p>Body</p>".to_string()),
            first_sentence: None,
            summary_text: None,
            media: Vec::new(),
        })
        .await
        .unwrap()
    {
        feedcourier::db::models::EntryInsertOutcome::Inserted(e) => *e,
        _ => unreachable!(),
    };
    let task552 =
        h.db.enqueue_delivery_task(
            route_row.id,
            ch552.id,
            dest552.id,
            entry_row.id,
            now_unix() - 1,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    assert_eq!(
        h.db.get_delivery_task(task552.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        DeliveryTaskStatus::DeadLetter
    );
    assert_eq!(
        h.db.get_smtp_recipient(recips[0].id)
            .await
            .unwrap()
            .unwrap()
            .status,
        SmtpRecipientStatus::Verified,
        "552 content/size error must NEVER mark a verified recipient as bounced!"
    );
    assert!(
        h.db.get_destination(dest552.id)
            .await
            .unwrap()
            .unwrap()
            .enabled,
        "destination must remain enabled after a 552 size error"
    );
    err552_handle.abort();

    // 4. QA-05: True 550 5.1.1 hard bounce masks plaintext email in BOTH `bounce_diagnostic` and `diagnostic_log`.
    let (b_port, _b_cap, b_handle) = spawn_custom_reply_smtp_server(
        "550 5.1.1 <secret-bounce@example.org>: User unknown\r\n",
        "250 OK\r\n",
    )
    .await;
    let ch_b_cfg = ChannelConfig::Smtp(SmtpChannelConfig {
        host: "127.0.0.1".to_string(),
        port: b_port,
        username: None,
        password: None,
        security: SmtpSecurity::Plain,
        from_address: "alerts@fc.test".to_string(),
        sender_identity: None,
    });
    let ch_b =
        h.db.create_channel(&NewChannel {
            name: "SMTP Bounce Mask".to_string(),
            kind: ChannelKind::Smtp,
            encrypted_config: h.master_key.encrypt_json(&ch_b_cfg).unwrap(),
            credential_group_key: ch_b_cfg.credential_group_key(),
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
    let dest_b =
        h.db.create_destination(&NewDestination {
            channel_id: ch_b.id,
            name: "Bounce Mask Dest".to_string(),
            encrypted_config: h
                .master_key
                .encrypt_json(&DestinationConfig::Smtp(SmtpDestinationConfig {
                    to_addresses: vec!["secret-bounce@example.org".to_string()],
                    reply_to: None,
                }))
                .unwrap(),
            enabled: true,
        })
        .await
        .unwrap();
    let hmac_b = h
        .master_key
        .smtp_recipient_hmac("secret-bounce@example.org");
    let recips_b =
        h.db.sync_smtp_recipients_for_destination(
            dest_b.id,
            &[hmac_b],
            SmtpRecipientStatus::Verified,
            Some("code"),
        )
        .await
        .unwrap();
    let route_b =
        h.db.create_route(&NewRoute {
            name: "Bounce Route".to_string(),
            feed_ids: vec![feed_row.id],
            match_tags: Vec::new(),
            channel_id: ch_b.id,
            destination_id: dest_b.id,
            include_keywords: Vec::new(),
            exclude_keywords: Vec::new(),
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions::default(),
            enabled: true,
        })
        .await
        .unwrap();
    let t_b =
        h.db.enqueue_delivery_task(route_b.id, ch_b.id, dest_b.id, entry_row.id, now_unix() - 1)
            .await
            .unwrap()
            .unwrap();
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    let task_b_after = h.db.get_delivery_task(t_b.id).await.unwrap().unwrap();
    let recip_b_after =
        h.db.get_smtp_recipient(recips_b[0].id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(recip_b_after.status, SmtpRecipientStatus::Bounced);
    let task_diag = task_b_after.diagnostic_log.unwrap_or_default();
    let bounce_diag = recip_b_after.bounce_diagnostic.unwrap_or_default();
    assert!(
        !task_diag.contains("secret-bounce@example.org")
            && !bounce_diag.contains("secret-bounce@example.org"),
        "plaintext email MUST be masked in diagnostics! task={task_diag:?} bounce={bounce_diag:?}"
    );
    assert!(
        task_diag.contains("s***e@example.org") && bounce_diag.contains("s***e@example.org"),
        "masked email must be present in diagnostics! task={task_diag:?} bounce={bounce_diag:?}"
    );
    b_handle.abort();
}

#[tokio::test]
async fn qa03_public_rate_limit_connect_info_trust_proxy_404_and_relative_form() {
    use axum::extract::ConnectInfo;
    use std::net::SocketAddr;

    // 1. Default `trust_proxy = false`: spoofing 31 distinct `X-Forwarded-For` values from the
    //    same real peer `ConnectInfo<SocketAddr>` MUST hit 429 on request #31!
    let h_direct = V3Harness::new_with_options(true, None, None, false).await;
    let peer = SocketAddr::from(([203, 0, 113, 77], 43210));
    for i in 1..=31u32 {
        let mut req = Request::builder()
            .method(Method::GET)
            .uri("/u/nonexistent-token")
            .header(HOST, "localhost:8080")
            .header("x-forwarded-for", format!("198.51.100.{i}"))
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(ConnectInfo(peer));
        let resp = h_direct.router.clone().oneshot(req).await.unwrap();
        if i <= 30 {
            assert_eq!(
                resp.status(),
                StatusCode::NOT_FOUND,
                "req #{i} should not be limited"
            );
        } else {
            assert_eq!(
                resp.status(),
                StatusCode::TOO_MANY_REQUESTS,
                "req #31 with spoofed X-Forwarded-For must be rate-limited when trust_proxy=false"
            );
        }
    }

    // 2. Bare `/u`, `/u/`, `/v`, `/v/` return public 404 HTML (never SPA `index.html`).
    for bare in ["/u", "/u/", "/v", "/v/"] {
        let mut req = Request::builder()
            .method(Method::GET)
            .uri(bare)
            .header(HOST, "localhost:8080")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([203, 0, 113, 78], 1000))));
        let resp = h_direct.router.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "bare path {bare} must return 404"
        );
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8_lossy(&bytes);
        assert!(
            !html.contains("<div id=\"root\">"),
            "bare path {bare} must NOT fall through to SPA index.html"
        );
    }

    // 3. `trust_proxy = true`: uses RIGHTMOST hop of `X-Forwarded-For`, immune to prepended spoofs.
    let h_proxy = V3Harness::new_with_options(true, None, None, true).await;
    let proxy_peer = SocketAddr::from(([10, 0, 0, 1], 8080));
    for i in 1..=31u32 {
        let mut req = Request::builder()
            .method(Method::GET)
            .uri("/v/nonexistent-token")
            .header(HOST, "localhost:8080")
            .header("x-forwarded-for", format!("1.2.3.{i}, 198.51.100.42"))
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(ConnectInfo(proxy_peer));
        let resp = h_proxy.router.clone().oneshot(req).await.unwrap();
        if i <= 30 {
            assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        } else {
            assert_eq!(
                resp.status(),
                StatusCode::TOO_MANY_REQUESTS,
                "rightmost XFF hop must govern rate limit when trust_proxy=true"
            );
        }
    }
    // A different rightmost hop behind the same reverse proxy is NOT blocked.
    let mut other_req = Request::builder()
        .method(Method::GET)
        .uri("/v/nonexistent-token")
        .header(HOST, "localhost:8080")
        .header("x-forwarded-for", "1.2.3.4, 198.51.100.99")
        .body(Body::empty())
        .unwrap();
    other_req.extensions_mut().insert(ConnectInfo(proxy_peer));
    let other_resp = h_proxy.router.clone().oneshot(other_req).await.unwrap();
    assert_eq!(other_resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn qa05_qa06_auto_paused_recovery_lockout_v_token_smtp_test_debug_push_and_channel_409() {
    let (smtp_port, captured, smtp_handle) = spawn_multi_smtp_server(None).await;
    let h = V3Harness::new_with_allowlist(true, None, None).await;
    let cookie = h.setup_admin().await;
    h.db.set_setting_typed(
        SETTING_PUBLIC_URL,
        &"https://fc.example.com/subpath".to_string(),
    )
    .await
    .unwrap();

    // 1. SMTP channel rejects `use_proxy: true` on create.
    let (st, _, _) = h
        .send_json(
            Method::POST,
            "/api/v1/channels",
            Some(&cookie),
            Some(json!({
                "kind": "smtp",
                "config": {
                    "kind": "smtp",
                    "host": "127.0.0.1",
                    "port": smtp_port,
                    "security": "plain",
                    "from_address": "bot@fc.test"
                },
                "use_proxy": true,
                "enabled": true
            })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "SMTP channel must reject use_proxy=true"
    );

    // Create valid SMTP channel with 2 recipients (`alice@example.org`, `bob@example.org`).
    let (st, _, ch_body) = h
        .send_json(
            Method::POST,
            "/api/v1/channels",
            Some(&cookie),
            Some(json!({
                "name": "Compliance SMTP",
                "kind": "smtp",
                "config": {
                    "kind": "smtp",
                    "host": "127.0.0.1",
                    "port": smtp_port,
                    "security": "plain",
                    "from_address": "bot@fc.test"
                },
                "initial_destination": {
                    "config": {
                        "kind": "smtp",
                        "to_addresses": ["alice@example.org", "bob@example.org"]
                    },
                    "enabled": true
                },
                "min_interval_secs": 0,
                "jitter_max_secs": 0,
                "use_proxy": false,
                "enabled": true
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let ch_json = parse_json(&ch_body);
    let ch_id = ch_json["id"].as_i64().unwrap();
    let dest_id = ch_json["destinations"][0]["id"].as_i64().unwrap();
    assert_eq!(
        ch_json["destinations"][0]["name"], "a***e@example.org",
        "auto-named SMTP destination must be masked"
    );

    // PATCH SMTP channel with `use_proxy: true` is also rejected.
    let (st, _, _) = h
        .send_json(
            Method::PATCH,
            &format!("/api/v1/channels/{ch_id}"),
            Some(&cookie),
            Some(json!({ "use_proxy": true })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // 2. Q3: Account-level `POST /api/v1/channels/{id}/test` sends self-test ONLY to `from_address`!
    captured.lock().unwrap().clear();
    let (st, _, _) = h
        .send_json(
            Method::POST,
            &format!("/api/v1/channels/{ch_id}/test"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    {
        let sent = captured.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].rcpt_to.len(), 1);
        assert!(sent[0].rcpt_to[0].contains("bot@fc.test"));
    }

    // 3. Q3: Destination-level `test` & `debug_push` fail with 400 when 0 recipients are verified!
    let (st, _, _) = h
        .send_json(
            Method::POST,
            &format!("/api/v1/destinations/{dest_id}/test"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "SMTP destination test must reject when 0 verified recipients exist"
    );

    let feed_srv = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rss.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<?xml version="1.0"?><rss version="2.0"><channel><title>QA Feed</title><item><title>Item 1</title><link>https://qa.example/1</link><description>Hello</description></item></channel></rss>"#,
        ))
        .mount(&feed_srv)
        .await;
    let (st, _, _) = h
        .send_json(
            Method::POST,
            "/api/v1/feeds/preview/push",
            Some(&cookie),
            Some(json!({
                "url": format!("{}/rss.xml", feed_srv.uri()),
                "destination_id": dest_id
            })),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "SMTP debug_push must reject when 0 verified recipients exist"
    );

    // 4. QA-06: 5 wrong code guesses atomically clear `code_sha256` -> old `/v/{token}` returns 410 Gone!
    let (st, _, recips_bytes) = h
        .send_json(
            Method::GET,
            &format!("/api/v1/destinations/{dest_id}/recipients"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let recips = parse_json(&recips_bytes);
    let alice_id = recips[0]["id"].as_i64().unwrap();
    let bob_id = recips[1]["id"].as_i64().unwrap();

    captured.lock().unwrap().clear();
    let (st, _, _) = h
        .send_json(
            Method::POST,
            &format!("/api/v1/destinations/{dest_id}/recipients/{alice_id}/send-verification"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let verify_raw = captured.lock().unwrap()[0].raw_data.clone();
    let v_path = extract_public_token_path(&verify_raw, "https://fc.example.com/subpath/v/");
    let v_route = v_path
        .strip_prefix("/subpath")
        .expect("subpath prefix in verify URL");

    // Relative `<form method="POST" action="">` on GET /v/{token}!
    let (v_get_st, _, v_get_html) = h
        .send_public(Method::GET, v_route, "198.51.100.11", None, None)
        .await;
    assert_eq!(v_get_st, StatusCode::OK);
    assert!(
        v_get_html.contains(r#"<form method="POST" action="">"#),
        "verify form must use relative action=\"\" for subpath PUBLIC_URL compatibility:\n{v_get_html}"
    );

    // Guess 5 wrong codes -> locks out BOTH code verification AND `/v/{token}` link!
    for _ in 0..5 {
        let (wst, _, _) = h
            .send_json(
                Method::POST,
                &format!("/api/v1/destinations/{dest_id}/recipients/{alice_id}/verify"),
                Some(&cookie),
                Some(json!({ "code": "000000" })),
            )
            .await;
        assert_eq!(wst, StatusCode::BAD_REQUEST);
    }
    let (gone_st, _, _) = h
        .send_public(Method::POST, v_route, "198.51.100.11", None, None)
        .await;
    assert_eq!(
        gone_st,
        StatusCode::GONE,
        "after 5 failed code guesses, old /v/{{token}} link MUST immediately return 410 Gone"
    );

    // 5. Verify Alice ONLY (Bob stays unverified); destination `test` and `debug_push` deliver
    //    ONLY 1 isolated single-`To` email (to Alice, NEVER Bob) with RFC 8058 headers!
    h.db.mark_smtp_recipient_verified(alice_id, "code", now_unix())
        .await
        .unwrap();
    captured.lock().unwrap().clear();
    let (st, _, _) = h
        .send_json(
            Method::POST,
            &format!("/api/v1/destinations/{dest_id}/test"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    {
        let sent = captured.lock().unwrap();
        assert_eq!(
            sent.len(),
            1,
            "only verified recipient Alice should receive test"
        );
        assert!(sent[0].rcpt_to[0].contains("alice@example.org"));
        assert!(
            sent[0]
                .raw_data
                .contains("List-Unsubscribe-Post: List-Unsubscribe=One-Click")
        );
    }

    captured.lock().unwrap().clear();
    let (st, _, dp_body) = h
        .send_json(
            Method::POST,
            "/api/v1/feeds/preview/push",
            Some(&cookie),
            Some(json!({
                "url": format!("{}/rss.xml", feed_srv.uri()),
                "destination_id": dest_id
            })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&dp_body));
    {
        let sent = captured.lock().unwrap();
        assert_eq!(
            sent.len(),
            1,
            "debug_push must deliver ONLY to verified Alice"
        );
        assert!(sent[0].rcpt_to[0].contains("alice@example.org"));
        assert!(
            sent[0]
                .raw_data
                .contains("List-Unsubscribe-Post: List-Unsubscribe=One-Click")
        );
    }

    // 6. QA-06: All-unsubscribed sets `enabled = false, auto_paused = true`; reactivate + verify
    //    automatically restores `enabled = true, auto_paused = false`!
    h.db.mark_smtp_recipient_unsubscribed(bob_id, "link", now_unix())
        .await
        .unwrap();
    let (_, paused) =
        h.db.mark_smtp_recipient_unsubscribed(alice_id, "link", now_unix())
            .await
            .unwrap();
    assert!(paused);
    let dest_paused = h.db.get_destination(dest_id).await.unwrap().unwrap();
    assert!(!dest_paused.enabled);
    assert!(dest_paused.auto_paused);

    // Check `/u/{token}` confirmation page uses relative `action=""`.
    h.db.reactivate_smtp_recipient(alice_id, now_unix())
        .await
        .unwrap();
    let unsub_tok = h.master_key.sign_unsubscribe_token(dest_id, alice_id);
    let (u_get_st, _, u_get_html) = h
        .send_public(
            Method::GET,
            &format!("/u/{unsub_tok}"),
            "198.51.100.12",
            None,
            None,
        )
        .await;
    assert_eq!(u_get_st, StatusCode::OK);
    assert!(u_get_html.contains(r#"<form method="POST" action="">"#));

    // Re-verifying Alice auto-resumes `enabled = true, auto_paused = false`!
    h.db.mark_smtp_recipient_verified(alice_id, "link", now_unix())
        .await
        .unwrap();
    let dest_resumed = h.db.get_destination(dest_id).await.unwrap().unwrap();
    assert!(
        dest_resumed.enabled,
        "auto_paused destination must auto-resume on re-verification"
    );
    assert!(!dest_resumed.auto_paused);

    // Manual disable clears `auto_paused = false` so verifying Bob does NOT re-enable it.
    let (st, _, _) = h
        .send_json(
            Method::PATCH,
            &format!("/api/v1/destinations/{dest_id}"),
            Some(&cookie),
            Some(json!({ "enabled": false })),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    h.db.mark_smtp_recipient_verified(bob_id, "code", now_unix())
        .await
        .unwrap();
    assert!(
        !h.db
            .get_destination(dest_id)
            .await
            .unwrap()
            .unwrap()
            .enabled,
        "manually disabled destination must stay disabled when a recipient verifies"
    );

    // 7. QA-08: `DELETE /api/v1/channels/{id}` returns `409 Conflict` with `referencing_routes`
    //    unless `?cascade=true` is passed!
    let route_row =
        h.db.create_route(&NewRoute {
            name: "Blocking Route".to_string(),
            feed_ids: Vec::new(),
            match_tags: Vec::new(),
            channel_id: ch_id,
            destination_id: dest_id,
            include_keywords: Vec::new(),
            exclude_keywords: Vec::new(),
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions::default(),
            enabled: true,
        })
        .await
        .unwrap();
    let (st, _, conflict_body) = h
        .send_json(
            Method::DELETE,
            &format!("/api/v1/channels/{ch_id}"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let conflict_json = parse_json(&conflict_body);
    assert_eq!(conflict_json["referencing_routes"][0]["id"], route_row.id);

    let (st, _, _) = h
        .send_json(
            Method::DELETE,
            &format!("/api/v1/channels/{ch_id}?cascade=true"),
            Some(&cookie),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert!(h.db.get_channel(ch_id).await.unwrap().is_none());

    smtp_handle.abort();
}

#[tokio::test]
async fn qa04_qa10_link_rewrite_parser_cache_before_cap_trailing_dot_cache_clear_and_market_safety()
{
    use feedcourier::plugins::link_rewrite::{
        LINK_REWRITE_PLUGIN_ID, YourlsAuthMode, YourlsProviderConfig, extract_anchor_hrefs,
        is_eligible_rewrite_url, rewrite_anchor_hrefs,
    };
    use feedcourier::plugins::{MarketIndexCache, PluginRegistry, RenderOverrides};
    use std::collections::HashMap;

    // 1. QA-04: Unquoted `href=.../`, apostrophe `title=Bob's`, and `<script>`/`<style>` skipping.
    let tricky_html = concat!(
        r#"<p><span title=Bob's><a href=https://example.com/blog/>Blog</a></span>"#,
        r#"<script>const s = '<a href="https://script.example/x">x</a>';</script>"#,
        r#"<style>/* <a href="https://style.example/y">y</a> */</style></p>"#
    );
    let hrefs = extract_anchor_hrefs(tricky_html);
    assert_eq!(
        hrefs,
        vec!["https://example.com/blog/".to_string()],
        "must extract full unquoted URL with trailing slash and skip <script>/<style>"
    );
    let mut repl = HashMap::new();
    repl.insert(
        "https://example.com/blog/".to_string(),
        "https://sho.rt/b1".to_string(),
    );
    let rewritten = rewrite_anchor_hrefs(tricky_html, &repl);
    assert!(
        rewritten.contains(r#"<a href="https://sho.rt/b1">Blog</a>"#),
        "<a> must NEVER be emitted as self-closing `<a ... />`: {rewritten}"
    );
    assert!(
        rewritten
            .contains(r#"<script>const s = '<a href="https://script.example/x">x</a>';</script>"#)
    );
    assert!(rewritten.contains(r#"<style>/* <a href="https://style.example/y">y</a> */</style>"#));

    // 2. QA-10: Trailing dot FQDN domain normalization + inactive auth secret cleanup.
    assert!(!is_eligible_rewrite_url(
        "https://skip.example.com./article",
        Some("sho.rt."),
        &["skip.example.com".to_string()]
    ));
    assert!(!is_eligible_rewrite_url(
        "https://sho.rt./abc",
        Some("sho.rt"),
        &[]
    ));
    let switched = YourlsProviderConfig {
        api_url: "https://sho.rt/yourls-api.php".to_string(),
        auth_mode: YourlsAuthMode::Signature,
        signature: Some("sig_tok".to_string()),
        username: Some("old_user".to_string()),
        password: Some("old_pass".to_string()),
        exclude_domains: vec!["Skip.Example.COM.".to_string()],
        strict_mode: false,
        pass_title: true,
        use_proxy: false,
    }
    .validate_and_normalize()
    .unwrap();
    assert_eq!(switched.username, None);
    assert_eq!(switched.password, None);
    assert_eq!(
        switched.exclude_domains,
        vec!["skip.example.com".to_string()]
    );

    // 3. QA-10: 50 already-cached URLs + 51st uncached URL -> 51st URL MUST still be shortened!
    let yourls_srv = MockServer::start().await;
    let api_calls = Arc::new(AtomicUsize::new(0));
    let api_calls_c = Arc::clone(&api_calls);
    Mock::given(method("POST"))
        .and(path("/yourls-api.php"))
        .respond_with(move |_req: &MockRequest| {
            let n = api_calls_c.fetch_add(1, Ordering::SeqCst) + 1;
            ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "shorturl": format!("https://sho.rt/new{n}")
            }))
        })
        .mount(&yourls_srv)
        .await;

    let h = V3Harness::new_with_allowlist(true, None, None).await;
    let cookie = h.setup_admin().await;
    h.db.update_plugin(LINK_REWRITE_PLUGIN_ID, Some(true), None)
        .await
        .unwrap();

    // Check GET /api/v1/plugins does NOT bump `plugins.updated_at` (read-only path).
    let ts_before =
        h.db.get_plugin(LINK_REWRITE_PLUGIN_ID)
            .await
            .unwrap()
            .unwrap()
            .updated_at;
    let (st, _, _) = h
        .send_json(Method::GET, "/api/v1/plugins", Some(&cookie), None)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        h.db.get_plugin(LINK_REWRITE_PLUGIN_ID)
            .await
            .unwrap()
            .unwrap()
            .updated_at,
        ts_before,
        "GET /api/v1/plugins must not write to plugins table"
    );

    let (st, _, inst_body) = h
        .send_json(
            Method::POST,
            "/api/v1/plugins/link-rewrite/instances",
            Some(&cookie),
            Some(json!({
                "name": "YOURLS Cap Test",
                "config": {
                    "api_url": format!("{}/yourls-api.php", yourls_srv.uri()),
                    "auth_mode": "signature",
                    "signature": "sig123"
                },
                "enabled": true
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let inst_id = parse_json(&inst_body)["id"].as_i64().unwrap();

    // Pre-populate 50 cached URLs.
    let mut pairs = Vec::new();
    let mut html_51 = String::from("<p>");
    for i in 1..=50 {
        let u = format!("https://cached.example/p/{i}");
        pairs.push((sha256_hex(u.as_bytes()), format!("https://sho.rt/c{i}")));
        html_51.push_str(&format!("<a href=\"{u}\">{i}</a> "));
    }
    let url_51 = "https://uncached.example/p/51";
    html_51.push_str(&format!("<a href=\"{url_51}\">51</a></p>"));
    h.db.put_plugin_link_cache_batch(inst_id, &pairs, now_unix())
        .await
        .unwrap();

    // Run LinkRewritePlugin::on_render directly via PluginRegistry!
    let ch =
        h.db.create_channel(&NewChannel {
            name: "Dummy TG".to_string(),
            kind: ChannelKind::Telegram,
            encrypted_config: h
                .master_key
                .encrypt_json(&ChannelConfig::Telegram(TelegramChannelConfig {
                    bot_token: "123:ABC".to_string(),
                    api_base_url: None,
                }))
                .unwrap(),
            credential_group_key: "tg:dummy".to_string(),
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
    let dest =
        h.db.create_destination(&NewDestination {
            channel_id: ch.id,
            name: "Dummy Dest".to_string(),
            encrypted_config: h
                .master_key
                .encrypt_json(&DestinationConfig::Telegram(TelegramDestinationConfig {
                    chat_id: "-1001".to_string(),
                    message_thread_id: None,
                }))
                .unwrap(),
            enabled: true,
        })
        .await
        .unwrap();
    let feed =
        h.db.create_feed(&NewFeed::new("https://cap.example/rss"))
            .await
            .unwrap();
    let mut plugins_map = BTreeMap::new();
    plugins_map.insert(
        LINK_REWRITE_PLUGIN_ID.to_string(),
        json!({ "provider_id": inst_id }),
    );
    let route_opts = RouteOptions {
        plugins: plugins_map,
        ..RouteOptions::default()
    };
    let route =
        h.db.create_route(&NewRoute {
            name: "Cap Route".to_string(),
            feed_ids: vec![feed.id],
            match_tags: Vec::new(),
            channel_id: ch.id,
            destination_id: dest.id,
            include_keywords: Vec::new(),
            exclude_keywords: Vec::new(),
            include_regex: None,
            exclude_regex: None,
            options: route_opts.clone(),
            enabled: true,
        })
        .await
        .unwrap();
    let entry = match h
        .db
        .insert_entry_if_new(&NewEntry {
            feed_id: feed.id,
            fingerprint_sha256: sha256_hex(b"cap-51-entry"),
            published_at: Some(now_unix()),
            discovered_at: now_unix(),
            title: Some("51 Links".to_string()),
            canonical_url: None,
            author: None,
            content_html: Some(html_51),
            first_sentence: None,
            summary_text: None,
            media: Vec::new(),
        })
        .await
        .unwrap()
    {
        feedcourier::db::models::EntryInsertOutcome::Inserted(e) => *e,
        _ => unreachable!(),
    };
    let registry = PluginRegistry::with_builtins();
    let http = ProtectedHttpClient::new(NetSettings {
        outbound_proxy_url: None,
        ssrf_allowlist: vec!["127.0.0.1/32".to_string()],
    })
    .unwrap();
    let (overrides, retry): (RenderOverrides, _) = registry
        .run_on_render(
            &h.db,
            &h.master_key,
            &http,
            &entry,
            &feed,
            &route,
            &route_opts,
            &ch,
            &dest,
            true,
        )
        .await
        .unwrap();
    assert!(retry.is_none());
    assert_eq!(
        api_calls.load(Ordering::SeqCst),
        1,
        "only the 51st uncached link should hit YOURLS API"
    );
    let rendered = overrides.content_html.unwrap();
    assert!(rendered.contains("https://sho.rt/c1"));
    assert!(rendered.contains("https://sho.rt/c50"));
    assert!(
        rendered.contains("https://sho.rt/new1"),
        "51st uncached URL must still be shortened when first 50 hit SQLite cache"
    );

    // 4. QA-10: Changing YOURLS `api_url` via PATCH automatically clears `plugin_link_cache`!
    assert_eq!(
        h.db.count_plugin_link_cache_by_instance(inst_id)
            .await
            .unwrap(),
        51
    );
    let (st, _, _) = h
        .send_json(
            Method::PATCH,
            &format!("/api/v1/plugins/link-rewrite/instances/{inst_id}"),
            Some(&cookie),
            Some(json!({
                "config": {
                    "api_url": format!("{}/yourls-api-v2.php", yourls_srv.uri())
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        h.db.count_plugin_link_cache_by_instance(inst_id)
            .await
            .unwrap(),
        0,
        "changing api_url must automatically invalidate stale plugin_link_cache rows"
    );

    // 5. QA-10: Marketplace strips non-`http(s)` URLs and preserves valid remote cache on failed refresh!
    let market_srv = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/index.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "schema_version": 1,
            "generated_at": "2026-11-23T00:00:00Z",
            "plugins": [{
                "id": "xss-plugin",
                "name": "XSS Test",
                "version": "1.0.0",
                "homepage": "javascript:alert(1)",
                "repository": "https://github.com/ElleryHe/FeedCourier-Plugins"
            }]
        })))
        .up_to_n_times(1)
        .mount(&market_srv)
        .await;
    Mock::given(method("GET"))
        .and(path("/index.json"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&market_srv)
        .await;

    let cache = MarketIndexCache::new(Some(format!("{}/index.json", market_srv.uri())));
    let (doc1, src1) = cache.get_index(&http, false).await;
    assert_eq!(src1, "remote");
    assert_eq!(
        doc1.plugins[0].homepage, None,
        "javascript: homepage URL must be stripped to None"
    );
    assert_eq!(
        doc1.plugins[0].repository.as_deref(),
        Some("https://github.com/ElleryHe/FeedCourier-Plugins")
    );

    // Forced refresh hits 503 -> returns snapshot for this call, WITHOUT clobbering the valid remote cache!
    let (_doc2, src2) = cache.get_index(&http, true).await;
    assert_eq!(src2, "snapshot");
    let (doc3, src3) = cache.get_index(&http, false).await;
    assert_eq!(
        src3, "remote",
        "failed forced refresh must NOT overwrite an existing valid remote cache entry"
    );
    assert_eq!(doc3.plugins[0].id, "xss-plugin");

    // Ensure `Plugin` trait is exercised without unused-import warnings.
    assert_eq!(
        registry.get(LINK_REWRITE_PLUGIN_ID).unwrap().manifest().id,
        LINK_REWRITE_PLUGIN_ID
    );
}

#[tokio::test]
async fn round2_qa_regressions_rg01_rg02_rg04_rg05_and_bounce_classifier() {
    use axum::extract::ConnectInfo;
    use feedcourier::channels::smtp::{
        SmtpComplianceFooter, SmtpSender, is_recipient_hard_bounce_parts,
    };
    use feedcourier::crypto::email_to_ascii_punycode;
    use feedcourier::db::models::{AppLocale, Feed, FeedHealthStatus, SmtpRecipientStatus};
    use feedcourier::plugins::link_rewrite::{
        LINK_REWRITE_PLUGIN_ID, MAX_CANDIDATE_URLS_PER_ENTRY,
    };
    use feedcourier::plugins::{PluginRegistry, RenderOverrides};
    use std::net::SocketAddr;

    // 1. RG-01a: `trust_proxy = true` prioritizes rightmost `X-Forwarded-For` over spoofed
    //    `X-Real-IP`, including across multiple `X-Forwarded-For` headers.
    let h_proxy = V3Harness::new_with_options(true, None, None, true).await;
    let proxy_peer = SocketAddr::from(([10, 0, 0, 9], 8080));
    for i in 1..=31u32 {
        let mut req = Request::builder()
            .method(Method::GET)
            .uri("/u/nonexistent-rg01-token")
            .header(HOST, "localhost:8080")
            .header("x-real-ip", format!("198.51.100.{i}"))
            .header("x-forwarded-for", format!("1.2.3.{i}"))
            .header("x-forwarded-for", "203.0.113.201")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(ConnectInfo(proxy_peer));
        let resp = h_proxy.router.clone().oneshot(req).await.unwrap();
        if i <= 30 {
            assert_eq!(
                resp.status(),
                StatusCode::NOT_FOUND,
                "req #{i} under limit should return 404"
            );
        } else {
            assert_eq!(
                resp.status(),
                StatusCode::TOO_MANY_REQUESTS,
                "req #31 must hit 429 using rightmost X-Forwarded-For despite rotating X-Real-IP"
            );
        }
    }

    // 2. RG-01b: Multi-segment `/u/foo/bar` and `/v/foo/bar` return 404 (never SPA `index.html`).
    for multi in ["/u/foo/bar", "/v/foo/bar", "/u/a/b/c", "/v/a/b/c"] {
        let mut req = Request::builder()
            .method(Method::GET)
            .uri(multi)
            .header(HOST, "localhost:8080")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([203, 0, 113, 202], 1234))));
        let resp = h_proxy.router.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "multi-segment public path {multi} must return 404"
        );
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8_lossy(&bytes);
        assert!(
            !html.contains("<div id=\"root\">"),
            "multi-segment public path {multi} must NOT fall through to SPA index.html"
        );
    }

    // 3. RG-02: `auto_paused` preserved on no-op PATCH (omitting `enabled` or re-sending unchanged
    //    `enabled: false`), and manually disabled destinations NEVER flip to `auto_paused = true`.
    let cookie = h_proxy.setup_admin().await;
    let (st, _, ch_body) = h_proxy
        .send_json(
            Method::POST,
            "/api/v1/channels",
            Some(&cookie),
            Some(json!({
                "name": "RG02 SMTP",
                "kind": "smtp",
                "config": {
                    "kind": "smtp",
                    "host": "127.0.0.1",
                    "port": 2525,
                    "security": "plain",
                    "from_address": "bot@fc.test"
                },
                "initial_destination": {
                    "name": "RG02 Dest",
                    "config": {
                        "kind": "smtp",
                        "to_addresses": ["alice@example.org"]
                    },
                    "enabled": true
                },
                "min_interval_secs": 0,
                "jitter_max_secs": 0,
                "use_proxy": false,
                "enabled": true
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let ch_json = parse_json(&ch_body);
    let ch_id = ch_json["id"].as_i64().unwrap();
    let dest_id = ch_json["destinations"][0]["id"].as_i64().unwrap();

    let recips = h_proxy.db.list_smtp_recipients(dest_id).await.unwrap();
    let alice_id = recips[0].id;

    // Unsubscribe Alice -> auto-pauses enabled destination (`enabled=false, auto_paused=true`).
    let (_, paused) = h_proxy
        .db
        .mark_smtp_recipient_unsubscribed(alice_id, "link", now_unix())
        .await
        .unwrap();
    assert!(paused);
    let d1 = h_proxy.db.get_destination(dest_id).await.unwrap().unwrap();
    assert!(!d1.enabled && d1.auto_paused);

    // PATCH omitting `enabled` (rename only) preserves `auto_paused = true`.
    let (st, _, _) = h_proxy
        .send_json(
            Method::PATCH,
            &format!("/api/v1/destinations/{dest_id}"),
            Some(&cookie),
            Some(json!({ "name": "RG02 Renamed" })),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        h_proxy
            .db
            .get_destination(dest_id)
            .await
            .unwrap()
            .unwrap()
            .auto_paused,
        "PATCH omitting enabled must preserve auto_paused=true"
    );

    // PATCH re-sending unchanged `enabled: false` ALSO preserves `auto_paused = true`.
    let (st, _, _) = h_proxy
        .send_json(
            Method::PATCH,
            &format!("/api/v1/destinations/{dest_id}"),
            Some(&cookie),
            Some(json!({ "name": "RG02 Renamed Again", "enabled": false })),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        h_proxy
            .db
            .get_destination(dest_id)
            .await
            .unwrap()
            .unwrap()
            .auto_paused,
        "no-op PATCH with unchanged enabled=false must preserve auto_paused=true"
    );

    // Re-verifying Alice auto-resumes the auto_paused destination!
    h_proxy
        .db
        .reactivate_smtp_recipient(alice_id, now_unix())
        .await
        .unwrap();
    h_proxy
        .db
        .mark_smtp_recipient_verified(alice_id, "code", now_unix())
        .await
        .unwrap();
    let d_resumed = h_proxy.db.get_destination(dest_id).await.unwrap().unwrap();
    assert!(d_resumed.enabled && !d_resumed.auto_paused);

    // RG-02b: Create a MANUALLY DISABLED destination (`enabled: false`), then unsubscribe its sole
    // recipient -> must NOT flip `auto_paused` to `true`, so later re-verification stays disabled!
    let (st, _, man_body) = h_proxy
        .send_json(
            Method::POST,
            &format!("/api/v1/channels/{ch_id}/destinations"),
            Some(&cookie),
            Some(json!({
                "name": "Manually Disabled Dest",
                "config": {
                    "kind": "smtp",
                    "to_addresses": ["bob@example.org"]
                },
                "enabled": false
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let man_dest_id = parse_json(&man_body)["id"].as_i64().unwrap();
    let bob_id = h_proxy.db.list_smtp_recipients(man_dest_id).await.unwrap()[0].id;
    let (_, man_paused) = h_proxy
        .db
        .mark_smtp_recipient_unsubscribed(bob_id, "link", now_unix())
        .await
        .unwrap();
    assert!(
        !man_paused,
        "manually disabled destination must NOT be auto-paused"
    );
    let man_after = h_proxy
        .db
        .get_destination(man_dest_id)
        .await
        .unwrap()
        .unwrap();
    assert!(!man_after.enabled);
    assert!(!man_after.auto_paused);
    // Reactivating + verifying Bob must NOT re-enable the manually disabled destination!
    h_proxy
        .db
        .reactivate_smtp_recipient(bob_id, now_unix())
        .await
        .unwrap();
    h_proxy
        .db
        .mark_smtp_recipient_verified(bob_id, "code", now_unix())
        .await
        .unwrap();
    let man_still_off = h_proxy
        .db
        .get_destination(man_dest_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !man_still_off.enabled,
        "manually disabled destination must stay disabled after recipient verification"
    );

    // 4. RG-05a: CAS guard on `rollback_smtp_recipient_verification_reservation` never clobbers
    //    concurrent state changes or newer reservations.
    let (st, _, cas_body) = h_proxy
        .send_json(
            Method::POST,
            &format!("/api/v1/channels/{ch_id}/destinations"),
            Some(&cookie),
            Some(json!({
                "name": "CAS Dest",
                "config": {
                    "kind": "smtp",
                    "to_addresses": ["cas@example.org"]
                },
                "enabled": true
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let cas_dest_id = parse_json(&cas_body)["id"].as_i64().unwrap();
    let cas_recip = h_proxy
        .db
        .list_smtp_recipients(cas_dest_id)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let now = now_unix();
    let hash_a = sha256_hex(b"code-A");
    let hash_b = sha256_hex(b"code-B");

    let (_, reserved_a) = h_proxy
        .db
        .reserve_smtp_recipient_verification_code(
            cas_dest_id,
            cas_recip.id,
            &hash_a,
            now + 1800,
            now,
            0,
            10,
        )
        .await
        .unwrap();
    assert_eq!(reserved_a.status, SmtpRecipientStatus::Pending);

    // Concurrent newer reservation B overwrites `code_sha256`.
    let _reserved_b = h_proxy
        .db
        .reserve_smtp_recipient_verification_code(
            cas_dest_id,
            cas_recip.id,
            &hash_b,
            now + 1800,
            now + 1,
            0,
            10,
        )
        .await
        .unwrap();

    // Stale rollback for A MUST be rejected by CAS (`Ok(false)`), preserving reservation B!
    let rolled_stale = h_proxy
        .db
        .rollback_smtp_recipient_verification_reservation(&cas_recip, &hash_a)
        .await
        .unwrap();
    assert!(
        !rolled_stale,
        "stale reservation rollback must not clobber newer code_sha256"
    );
    let cur = h_proxy
        .db
        .get_smtp_recipient(cas_recip.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cur.code_sha256.as_deref(), Some(hash_b.as_str()));

    // Matching rollback for B succeeds and restores original `unverified` state.
    let rolled_b = h_proxy
        .db
        .rollback_smtp_recipient_verification_reservation(&cas_recip, &hash_b)
        .await
        .unwrap();
    assert!(rolled_b);
    let restored = h_proxy
        .db
        .get_smtp_recipient(cas_recip.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored.status, SmtpRecipientStatus::Unverified);
    assert!(restored.code_sha256.is_none());

    // 5. RG-05b: IDN SMTP addresses convert cleanly to UTS-46 ASCII Punycode across validation
    //    and `SmtpSender::build_entry_email_full`.
    assert_eq!(
        email_to_ascii_punycode("Alice <alice@Bücher.DE>"),
        "Alice <alice@xn--bcher-kva.de>"
    );
    let idn_ch_cfg = SmtpChannelConfig {
        host: "smtp.example.com".to_string(),
        port: 587,
        security: SmtpSecurity::StartTls,
        username: None,
        password: None,
        from_address: "bot@bücher.de".to_string(),
        sender_identity: None,
    };
    ChannelConfig::Smtp(idn_ch_cfg.clone())
        .validate()
        .expect("IDN from_address must validate");
    let idn_dest_cfg = SmtpDestinationConfig {
        to_addresses: vec!["alice@bücher.de".to_string()],
        reply_to: Some("reply@bücher.de".to_string()),
    };
    DestinationConfig::Smtp(idn_dest_cfg.clone())
        .validate()
        .expect("IDN to_addresses and reply_to must validate");
    let dummy_feed = Feed {
        id: 1,
        url: "https://example.com/rss".to_string(),
        title: "IDN Feed".to_string(),
        site_url: None,
        favicon_url: None,
        tags_json: "[]".to_string(),
        poll_interval_secs: 900,
        fetch_full_content: false,
        use_proxy: false,
        etag: None,
        last_modified: None,
        health_status: FeedHealthStatus::Active,
        consecutive_failures: 0,
        next_poll_at: Some(0),
        last_error: None,
        silent_init: false,
        created_at: 0,
        updated_at: 0,
    };
    let dummy_entry = feedcourier::db::models::Entry {
        id: 1,
        feed_id: 1,
        fingerprint_sha256: sha256_hex(b"idn-entry"),
        published_at: Some(now),
        discovered_at: now,
        payload_purged_at: None,
        title: Some("IDN Delivery".to_string()),
        canonical_url: Some("https://example.com/post".to_string()),
        author: None,
        content_html: Some("<p>Hello IDN</p>".to_string()),
        first_sentence: Some("Hello IDN".to_string()),
        summary_text: Some("Hello IDN".to_string()),
        media_json: None,
    };
    let built = SmtpSender::build_entry_email_full(
        &idn_ch_cfg,
        &idn_dest_cfg.to_addresses,
        idn_dest_cfg.reply_to.as_deref(),
        &dummy_feed,
        &dummy_entry,
        1,
        None,
        AppLocale::En,
        chrono_tz::UTC,
        &RenderOverrides::default(),
        &SmtpComplianceFooter {
            destination_id: Some(1),
            route_name: Some("IDN"),
            unsubscribe_url: Some("https://fc.example.com/u/tok"),
            sender_identity: None,
            include_compliance_headers: true,
        },
    )
    .expect("IDN email build must succeed");
    let raw_mime = String::from_utf8_lossy(&built.message.formatted()).to_string();
    assert!(raw_mime.contains("bot@xn--bcher-kva.de"));
    assert!(raw_mime.contains("alice@xn--bcher-kva.de"));
    assert!(raw_mime.contains("reply@xn--bcher-kva.de"));

    // 6. P2 SMTP Hard Bounce Classifier: 5.2.1 disabled mailbox, `Addressee unknown`, and
    //    email local-parts (`author@`, `policy@`, `spam@`) no longer cause false negatives.
    assert!(is_recipient_hard_bounce_parts(
        Some(550),
        "550 5.2.1 The email account that you tried to reach is disabled"
    ));
    assert!(is_recipient_hard_bounce_parts(
        Some(550),
        "550 <author@example.com>: User unknown"
    ));
    assert!(is_recipient_hard_bounce_parts(
        Some(550),
        "550 <policy@example.com>: Addressee unknown"
    ));
    assert!(is_recipient_hard_bounce_parts(
        Some(550),
        "550 spam@example.com: Mailbox unavailable"
    ));
    assert!(!is_recipient_hard_bounce_parts(
        Some(550),
        "550 5.7.1 <alice@example.com>: Rejected by policy"
    ));
    assert!(!is_recipient_hard_bounce_parts(
        Some(550),
        "550 Relay access denied for alice@example.com"
    ));

    // 7. RG-04: Batch `touch_plugin_link_cache` across >500 chunk boundary AND
    //    `MAX_CANDIDATE_URLS_PER_ENTRY` (1,000) cap in `LinkRewritePlugin::on_render`.
    let (st, _, _) = h_proxy
        .send_json(
            Method::PATCH,
            "/api/v1/plugins/link-rewrite",
            Some(&cookie),
            Some(json!({ "enabled": true })),
        )
        .await;
    assert_eq!(st, StatusCode::OK);

    let (st, _, inst_body) = h_proxy
        .send_json(
            Method::POST,
            "/api/v1/plugins/link-rewrite/instances",
            Some(&cookie),
            Some(json!({
                "name": "YOURLS Batch Touch & Cap",
                "config": {
                    "api_url": "https://sho.rt/yourls-api.php",
                    "auth_mode": "signature",
                    "signature": "sig-rg04"
                },
                "enabled": true
            })),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let inst_id = parse_json(&inst_body)["id"].as_i64().unwrap();

    let total_links = MAX_CANDIDATE_URLS_PER_ENTRY + 5;
    let mut cache_pairs = Vec::with_capacity(total_links);
    let mut big_html = String::from("<p>");
    for i in 1..=total_links {
        let u = format!("https://mass.example/item/{i}");
        cache_pairs.push((sha256_hex(u.as_bytes()), format!("https://sho.rt/m{i}")));
        big_html.push_str(&format!("<a href=\"{u}\">{i}</a> "));
    }
    big_html.push_str("</p>");

    // Insert all 1,005 rows with old timestamp `1000`, then batch-touch 620 hashes with `now + 50`.
    h_proxy
        .db
        .put_plugin_link_cache_batch(inst_id, &cache_pairs, 1_000)
        .await
        .unwrap();
    let touch_hashes: Vec<String> = cache_pairs[..620].iter().map(|(h, _)| h.clone()).collect();
    h_proxy
        .db
        .touch_plugin_link_cache(inst_id, &touch_hashes, now + 50)
        .await
        .unwrap();

    // Run `on_render` on the 1,005-link article: first 1,000 cached links rewrite, links 1001..1005
    // stay original and emit a `"capped at"` warning without making any HTTP calls!
    let ch_row = h_proxy.db.get_channel(ch_id).await.unwrap().unwrap();
    let dest_row = h_proxy.db.get_destination(dest_id).await.unwrap().unwrap();
    let feed_row = h_proxy
        .db
        .create_feed(&NewFeed::new("https://mass.example/rss"))
        .await
        .unwrap();
    let mut p_map = BTreeMap::new();
    p_map.insert(
        LINK_REWRITE_PLUGIN_ID.to_string(),
        json!({ "provider_id": inst_id }),
    );
    let r_opts = RouteOptions {
        plugins: p_map,
        ..RouteOptions::default()
    };
    let route_row = h_proxy
        .db
        .create_route(&NewRoute {
            name: "RG04 Cap Route".to_string(),
            feed_ids: vec![feed_row.id],
            match_tags: Vec::new(),
            channel_id: ch_row.id,
            destination_id: dest_row.id,
            include_keywords: Vec::new(),
            exclude_keywords: Vec::new(),
            include_regex: None,
            exclude_regex: None,
            options: r_opts.clone(),
            enabled: true,
        })
        .await
        .unwrap();
    let big_entry = match h_proxy
        .db
        .insert_entry_if_new(&NewEntry {
            feed_id: feed_row.id,
            fingerprint_sha256: sha256_hex(b"rg04-1005-links"),
            published_at: Some(now),
            discovered_at: now,
            title: Some("1005 Links".to_string()),
            canonical_url: None,
            author: None,
            content_html: Some(big_html),
            first_sentence: None,
            summary_text: None,
            media: Vec::new(),
        })
        .await
        .unwrap()
    {
        feedcourier::db::models::EntryInsertOutcome::Inserted(e) => *e,
        _ => unreachable!(),
    };
    let registry = PluginRegistry::with_builtins();
    let http = ProtectedHttpClient::new(NetSettings::default()).unwrap();
    let (overrides, retry) = registry
        .run_on_render(
            &h_proxy.db,
            &h_proxy.master_key,
            &http,
            &big_entry,
            &feed_row,
            &route_row,
            &r_opts,
            &ch_row,
            &dest_row,
            true,
        )
        .await
        .unwrap();
    assert!(retry.is_none());
    let out_html = overrides.content_html.unwrap();
    assert!(out_html.contains("https://sho.rt/m1\""));
    assert!(out_html.contains(&format!("https://sho.rt/m{MAX_CANDIDATE_URLS_PER_ENTRY}\"")));
    assert!(
        out_html.contains(&format!(
            "https://mass.example/item/{}",
            MAX_CANDIDATE_URLS_PER_ENTRY + 1
        )),
        "links beyond MAX_CANDIDATE_URLS_PER_ENTRY must remain original"
    );
    assert!(
        overrides
            .warnings
            .iter()
            .any(|w| w.contains("capped at") && w.contains("1000")),
        "candidate URL cap must emit a 'capped at' warning: {:?}",
        overrides.warnings
    );
}
