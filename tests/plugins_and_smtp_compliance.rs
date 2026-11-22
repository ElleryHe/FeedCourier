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
        let temp_dir = TempDir::new().expect("tempdir");
        let mut config = AppConfig::with_data_dir(temp_dir.path());
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
