//! Integration tests for the debugging / queue-control / account-security wave:
//! chronological backlog ordering, global queue pause + merged delivery order, route delivery
//! windows, per-route delivery language, per-feed cache clearing, stateless debug push, system
//! timezone, and administrator username + TOTP two-factor authentication.

use axum::Router;
use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE, HOST, SET_COOKIE};
use axum::http::{Method, Request, StatusCode};
use bytes::Bytes;
use chrono::Datelike;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use feedcourier::api::{AppState, build_api_router};
use feedcourier::channels::{
    ChannelConfig, ChannelKind, DestinationConfig, RouteOptions, TelegramChannelConfig,
    TelegramDeliveryMode, TelegramDestinationConfig,
};
use feedcourier::config::AppConfig;
use feedcourier::crypto::MasterKey;
use feedcourier::db::Database;
use feedcourier::db::models::{
    DeliveryTaskStatus, NewChannel, NewDestination, NewFeed, NewRoute, now_unix,
};
use feedcourier::net::{NetSettings, ProtectedHttpClient};
use feedcourier::scheduler::DeliveryDispatcher;
use feedcourier::totp;

struct Harness {
    _temp_dir: TempDir,
    db: Database,
    master_key: MasterKey,
    dispatcher: DeliveryDispatcher,
    router: Router,
}

impl Harness {
    async fn new() -> Self {
        let temp_dir = TempDir::new().expect("tempdir");
        let config = AppConfig::with_data_dir(temp_dir.path());
        let db = Database::from_config(&config).await.expect("db");
        let master_key = MasterKey::from_config(&config).expect("master key");
        let http = ProtectedHttpClient::new(NetSettings {
            outbound_proxy_url: None,
            ssrf_allowlist: vec!["127.0.0.1/32".to_string()],
        })
        .expect("http client");
        let dispatcher = DeliveryDispatcher::new(config, db.clone(), master_key.clone(), http)
            .expect("dispatcher");
        let router = build_api_router(AppState::new(dispatcher.clone()));
        Self {
            _temp_dir: temp_dir,
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
        body: Option<Value>,
    ) -> (StatusCode, axum::http::HeaderMap, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(HOST, "localhost:8080");
        if let Some(c) = cookie {
            builder = builder.header(COOKIE, c);
        }
        let body = match body {
            Some(v) => {
                builder = builder.header(CONTENT_TYPE, "application/json");
                Body::from(serde_json::to_vec(&v).unwrap())
            }
            None => Body::empty(),
        };
        let resp = self
            .router
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes: Bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, headers, json)
    }

    async fn setup_admin(&self, username: Option<&str>, password: &str) -> String {
        let mut payload = json!({ "password": password });
        if let Some(u) = username {
            payload["username"] = json!(u);
        }
        let (status, headers, body) = self
            .send(Method::POST, "/api/v1/auth/setup", None, Some(payload))
            .await;
        assert_eq!(status, StatusCode::OK, "setup failed: {body}");
        cookie_pair(&headers)
    }

    async fn telegram_channel(
        &self,
        api_base: &str,
    ) -> (
        feedcourier::db::models::Channel,
        feedcourier::db::models::ChannelDestination,
    ) {
        let cfg = ChannelConfig::Telegram(TelegramChannelConfig {
            bot_token: "123456:queue_test_token_abcdef".to_string(),
            api_base_url: Some(api_base.to_string()),
        });
        let dest_cfg = DestinationConfig::Telegram(TelegramDestinationConfig {
            chat_id: "-100777".to_string(),
            message_thread_id: None,
        });
        let ch = self
            .db
            .create_channel(&NewChannel {
                name: "Queue TG".to_string(),
                kind: ChannelKind::Telegram,
                encrypted_config: self.master_key.encrypt_json(&cfg).unwrap(),
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
        let dest = self
            .db
            .create_destination(&NewDestination {
                channel_id: ch.id,
                name: "Queue Chat".to_string(),
                encrypted_config: self.master_key.encrypt_json(&dest_cfg).unwrap(),
                enabled: true,
            })
            .await
            .unwrap();
        (ch, dest)
    }

    async fn catch_all_route(
        &self,
        channel_id: i64,
        destination_id: i64,
        options: RouteOptions,
    ) -> feedcourier::db::models::Route {
        self.db
            .create_route(&NewRoute {
                name: "Catch-all".to_string(),
                feed_ids: vec![],
                match_tags: vec![],
                channel_id,
                destination_id,
                include_keywords: vec![],
                exclude_keywords: vec![],
                include_regex: None,
                exclude_regex: None,
                options,
                enabled: true,
            })
            .await
            .unwrap()
    }

    async fn feed(&self, url: String) -> feedcourier::db::models::Feed {
        let mut nf = NewFeed::new(url);
        nf.silent_init = false;
        self.db.create_feed(&nf).await.unwrap()
    }
}

fn cookie_pair(headers: &axum::http::HeaderMap) -> String {
    headers
        .get(SET_COOKIE)
        .expect("Set-Cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .trim()
        .to_string()
}

fn compact_options() -> RouteOptions {
    RouteOptions {
        telegram_mode: TelegramDeliveryMode::CompactCard,
        link_preview: true,
        ..RouteOptions::default()
    }
}

/// RSS items are listed newest-first, as real feeds do.
fn rss(base: &str, items: &[(&str, &str, &str)]) -> String {
    let mut body = String::new();
    for (guid, title, pub_date) in items {
        body.push_str(&format!(
            "<item><guid>{guid}</guid><title>{title}</title><link>{base}/a/{guid}</link>\
             <pubDate>{pub_date}</pubDate><description><![CDATA[<p>Body of {title}.</p>]]></description></item>"
        ));
    }
    format!(
        "<?xml version=\"1.0\"?><rss version=\"2.0\"><channel><title>Feed</title><link>{base}</link>{body}</channel></rss>"
    )
}

async fn mount_feed(server: &MockServer, route: &str, xml: String) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/rss+xml")
                .set_body_string(xml),
        )
        .mount(server)
        .await;
}

async fn mount_telegram_ok(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path_regex(r"^/bot.*/sendMessage$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": { "message_id": 7 }
        })))
        .mount(server)
        .await;
}

async fn sent_texts(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().ends_with("/sendMessage"))
        .map(|r| {
            let v: Value = serde_json::from_slice(&r.body).unwrap();
            v["text"].as_str().unwrap_or_default().to_string()
        })
        .collect()
}

#[tokio::test]
async fn paused_queue_accumulates_then_delivers_all_feeds_oldest_first_in_route_language() {
    let tg = MockServer::start().await;
    let origin = MockServer::start().await;
    mount_telegram_ok(&tg).await;
    let h = Harness::new().await;
    let cookie = h.setup_admin(None, "queue-admin-password").await;

    let base = origin.uri();
    mount_feed(
        &origin,
        "/a.xml",
        rss(
            &base,
            &[
                ("a3", "A3 newest", "Fri, 13 Nov 2026 12:00:00 GMT"),
                ("a2", "A2 middle", "Thu, 12 Nov 2026 12:00:00 GMT"),
                ("a1", "A1 oldest", "Wed, 11 Nov 2026 12:00:00 GMT"),
            ],
        ),
    )
    .await;
    mount_feed(
        &origin,
        "/b.xml",
        rss(
            &base,
            &[
                ("b2", "B2 later", "Thu, 12 Nov 2026 18:00:00 GMT"),
                ("b1", "B1 earlier", "Wed, 11 Nov 2026 18:00:00 GMT"),
            ],
        ),
    )
    .await;

    let (channel, dest) = h.telegram_channel(&tg.uri()).await;
    let mut opts = compact_options();
    opts.locale = Some(feedcourier::db::models::AppLocale::ZhCn);
    h.catch_all_route(channel.id, dest.id, opts).await;
    let feed_a = h.feed(format!("{base}/a.xml")).await;
    let feed_b = h.feed(format!("{base}/b.xml")).await;

    // Pause the global queue; feeds keep polling and accumulate tasks.
    let (status, _, state) = h
        .send(
            Method::PUT,
            "/api/v1/tasks/queue",
            Some(&cookie),
            Some(json!({ "action": "pause" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state["paused"], true);

    let ingest = h.dispatcher.ingest();
    assert_eq!(
        ingest
            .poll_feed(feed_a.id, false, false)
            .await
            .unwrap()
            .enqueued_tasks,
        3
    );
    assert_eq!(
        ingest
            .poll_feed(feed_b.id, false, false)
            .await
            .unwrap()
            .enqueued_tasks,
        2
    );

    assert!(!h.dispatcher.process_next_outbox_task().await.unwrap());
    assert!(sent_texts(&tg).await.is_empty());
    let queued =
        h.db.list_delivery_task_details(Some(DeliveryTaskStatus::Queued), 50)
            .await
            .unwrap();
    assert_eq!(queued.len(), 5);

    // Resume: all five tasks are delivered as one merged chronological stream.
    let (status, _, state) = h
        .send(
            Method::PUT,
            "/api/v1/tasks/queue",
            Some(&cookie),
            Some(json!({ "action": "resume" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state["paused"], false);
    assert_eq!(h.dispatcher.drain_due_outbox_tasks(10).await.unwrap(), 5);

    let texts = sent_texts(&tg).await;
    let order: Vec<&str> = ["A1", "B1", "A2", "B2", "A3"].to_vec();
    assert_eq!(texts.len(), 5);
    for (text, label) in texts.iter().zip(order) {
        assert!(text.contains(label), "expected {label} next, got: {text}");
        assert!(
            text.contains("阅读原文"),
            "route locale zh-CN not applied: {text}"
        );
    }
}

#[tokio::test]
async fn timed_pause_auto_resumes_and_rejects_past_resume_time() {
    let h = Harness::new().await;
    let cookie = h.setup_admin(None, "queue-admin-password").await;

    let (status, _, _) = h
        .send(
            Method::PUT,
            "/api/v1/tasks/queue",
            Some(&cookie),
            Some(json!({ "action": "pause", "resume_in_secs": -5 })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, state) = h
        .send(
            Method::PUT,
            "/api/v1/tasks/queue",
            Some(&cookie),
            Some(json!({ "action": "pause", "resume_in_secs": 3600 })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state["paused"], true);
    assert!(state["resume_at"].is_string());

    // Force the persisted resume time into the past: the queue reports itself resumed.
    h.db.set_outbox_queue_state(&feedcourier::schedule::OutboxQueueState {
        paused: true,
        resume_at: Some(now_unix() - 1),
    })
    .await
    .unwrap();
    let (_, _, state) = h
        .send(Method::GET, "/api/v1/tasks/queue", Some(&cookie), None)
        .await;
    assert_eq!(state["paused"], false);
}

#[tokio::test]
async fn route_windows_hold_tasks_until_the_next_allowed_window() {
    let tg = MockServer::start().await;
    let origin = MockServer::start().await;
    mount_telegram_ok(&tg).await;
    let h = Harness::new().await;
    let base = origin.uri();
    mount_feed(
        &origin,
        "/w.xml",
        rss(
            &base,
            &[("w1", "Windowed", "Wed, 11 Nov 2026 12:00:00 GMT")],
        ),
    )
    .await;
    let (channel, dest) = h.telegram_channel(&tg.uri()).await;

    // Allow every weekday except "today" in the (default) system timezone.
    let tz = h.db.get_timezone().await;
    let today = u8::try_from(
        chrono::Utc::now()
            .with_timezone(&tz)
            .weekday()
            .number_from_monday(),
    )
    .unwrap();
    let mut opts = compact_options();
    opts.active_weekdays = (1..=7).filter(|d| *d != today).collect();
    opts.active_windows = vec![feedcourier::channels::DeliveryTimeWindow {
        start: "00:00".to_string(),
        end: "00:00".to_string(),
    }];
    let route = h.catch_all_route(channel.id, dest.id, opts).await;
    let feed = h.feed(format!("{base}/w.xml")).await;

    let outcome = h
        .dispatcher
        .ingest()
        .poll_feed(feed.id, false, false)
        .await
        .unwrap();
    assert_eq!(outcome.enqueued_tasks, 1, "task is still queued");
    let tasks =
        h.db.list_delivery_task_details(Some(DeliveryTaskStatus::Queued), 10)
            .await
            .unwrap();
    assert_eq!(tasks.len(), 1);
    assert!(
        tasks[0].next_attempt_at > now_unix() + 60,
        "next_attempt_at must point to the next allowed window"
    );
    assert!(!h.dispatcher.process_next_outbox_task().await.unwrap());

    // Even if the task becomes due, the dispatcher re-checks the window and defers it again.
    sqlx::query("UPDATE delivery_tasks SET next_attempt_at = ?1")
        .bind(now_unix() - 1)
        .execute(h.db.write_pool())
        .await
        .unwrap();
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    let task = h.db.get_delivery_task(tasks[0].id).await.unwrap().unwrap();
    assert_eq!(task.status, DeliveryTaskStatus::Deferred);
    assert_eq!(task.attempt_count, 0);
    assert!(task.diagnostic_log.unwrap().contains("window"));
    assert!(sent_texts(&tg).await.is_empty());

    // Opening the window (all weekdays) delivers it.
    let route_patch = feedcourier::db::models::UpdateRoute {
        options: Some(compact_options()),
        ..Default::default()
    };
    h.db.update_route(route.id, &route_patch).await.unwrap();
    sqlx::query("UPDATE delivery_tasks SET next_attempt_at = ?1")
        .bind(now_unix() - 1)
        .execute(h.db.write_pool())
        .await
        .unwrap();
    assert!(h.dispatcher.process_next_outbox_task().await.unwrap());
    assert_eq!(sent_texts(&tg).await.len(), 1);
}

#[tokio::test]
async fn clear_feed_cache_reingests_the_full_backlog_oldest_first() {
    let tg = MockServer::start().await;
    let origin = MockServer::start().await;
    mount_telegram_ok(&tg).await;
    let h = Harness::new().await;
    let cookie = h.setup_admin(None, "queue-admin-password").await;
    let base = origin.uri();
    mount_feed(
        &origin,
        "/c.xml",
        rss(
            &base,
            &[
                ("c2", "C2 new", "Thu, 12 Nov 2026 12:00:00 GMT"),
                ("c1", "C1 old", "Wed, 11 Nov 2026 12:00:00 GMT"),
            ],
        ),
    )
    .await;
    let (channel, dest) = h.telegram_channel(&tg.uri()).await;
    h.catch_all_route(channel.id, dest.id, compact_options())
        .await;
    let feed = h.feed(format!("{base}/c.xml")).await;

    let first = h
        .dispatcher
        .ingest()
        .poll_feed(feed.id, false, false)
        .await
        .unwrap();
    assert_eq!(first.enqueued_tasks, 2);
    assert_eq!(h.dispatcher.drain_due_outbox_tasks(10).await.unwrap(), 2);
    let again = h
        .dispatcher
        .ingest()
        .poll_feed(feed.id, false, false)
        .await
        .unwrap();
    assert_eq!(again.new_entries, 0, "deduplicated before clearing");

    let (status, _, body) = h
        .send(
            Method::POST,
            &format!("/api/v1/feeds/{}/clear-cache", feed.id),
            Some(&cookie),
            Some(json!({ "poll_now": true })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["deleted_entries"], 2);
    assert_eq!(body["poll"]["new_entries"], 2);
    assert_eq!(body["poll"]["enqueued_tasks"], 2);

    assert_eq!(h.dispatcher.drain_due_outbox_tasks(10).await.unwrap(), 2);
    let texts = sent_texts(&tg).await;
    assert_eq!(texts.len(), 4);
    assert!(
        texts[2].contains("C1"),
        "backlog must replay oldest-first: {texts:?}"
    );
    assert!(texts[3].contains("C2"));

    // `poll_now: false` only wipes state.
    let (status, _, body) = h
        .send(
            Method::POST,
            &format!("/api/v1/feeds/{}/clear-cache", feed.id),
            Some(&cookie),
            Some(json!({ "poll_now": false })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["deleted_entries"], 2);
    assert!(body["poll"].is_null());
    let refreshed = h.db.get_feed(feed.id).await.unwrap().unwrap();
    assert!(refreshed.etag.is_none() && !refreshed.silent_init);
}

#[tokio::test]
async fn debug_push_is_stateless_and_honors_language() {
    let tg = MockServer::start().await;
    let origin = MockServer::start().await;
    mount_telegram_ok(&tg).await;
    let h = Harness::new().await;
    let cookie = h.setup_admin(None, "queue-admin-password").await;
    let base = origin.uri();
    mount_feed(
        &origin,
        "/d.xml",
        rss(
            &base,
            &[
                ("d2", "D2 latest", "Thu, 12 Nov 2026 12:00:00 GMT"),
                ("d1", "D1 older", "Wed, 11 Nov 2026 12:00:00 GMT"),
            ],
        ),
    )
    .await;
    let (channel, dest) = h.telegram_channel(&tg.uri()).await;

    let (status, _, body) = h
        .send(
            Method::POST,
            "/api/v1/feeds/preview/push",
            Some(&cookie),
            Some(json!({
                "url": format!("{base}/d.xml"),
                "destination_id": dest.id,
                "options": { "telegram_mode": "compact_card", "locale": "zh-CN", "timezone": "Asia/Shanghai" }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
    assert_eq!(body["entry_title"], "D2 latest");

    // A specific older item, in English, can be pushed repeatedly.
    let (_, _, preview) = h
        .send(
            Method::POST,
            "/api/v1/feeds/preview",
            Some(&cookie),
            Some(json!({ "url": format!("{base}/d.xml") })),
        )
        .await;
    let older_fp = preview["items"][1]["fingerprint_sha256"].as_str().unwrap();
    for _ in 0..2 {
        let (status, _, body) = h
            .send(
                Method::POST,
                "/api/v1/feeds/preview/push",
                Some(&cookie),
                Some(json!({
                    "url": format!("{base}/d.xml"),
                    "channel_id": channel.id,
                    "fingerprint_sha256": older_fp,
                    "options": { "telegram_mode": "compact_card", "locale": "en" }
                })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["entry_title"], "D1 older");
    }

    let texts = sent_texts(&tg).await;
    assert_eq!(texts.len(), 3);
    assert!(texts[0].contains("阅读原文") && texts[0].contains("D2"));
    assert!(texts[1].contains("Read original") && texts[1].contains("D1"));

    // Nothing persisted: no entries, fingerprints or tasks.
    let snap = h.db.get_overview_snapshot().await.unwrap();
    assert_eq!(snap.entries_total_fingerprints, 0);
    assert_eq!(snap.tasks_queued + snap.tasks_delivered, 0);

    // Unknown fingerprint is a clean 404.
    let (status, _, _) = h
        .send(
            Method::POST,
            "/api/v1/feeds/preview/push",
            Some(&cookie),
            Some(json!({
                "url": format!("{base}/d.xml"),
                "channel_id": channel.id,
                "fingerprint_sha256": "0".repeat(64),
            })),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn timezone_setting_and_route_schedule_validation() {
    let h = Harness::new().await;
    let cookie = h.setup_admin(None, "queue-admin-password").await;

    let (_, _, settings) = h
        .send(Method::GET, "/api/v1/settings", Some(&cookie), None)
        .await;
    assert_eq!(settings["timezone"], "America/Los_Angeles");

    let (status, _, settings) = h
        .send(
            Method::PUT,
            "/api/v1/settings",
            Some(&cookie),
            Some(json!({ "timezone": "Asia/Shanghai" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settings["timezone"], "Asia/Shanghai");
    // A timezone-only update must not wipe the network settings.
    assert!(settings["ssrf_allowlist"].is_array());

    let (status, _, _) = h
        .send(
            Method::PUT,
            "/api/v1/settings",
            Some(&cookie),
            Some(json!({ "timezone": "Mars/Olympus" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (channel, dest) = h.telegram_channel("http://127.0.0.1:1").await;
    let (status, _, _) = h
        .send(
            Method::POST,
            "/api/v1/routes",
            Some(&cookie),
            Some(json!({
                "name": "bad window",
                "destination_id": dest.id,
                "options": { "active_windows": [{ "start": "25:00", "end": "07:00" }] }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Invalid per-route timezone is rejected with 400.
    let (status, _, _) = h
        .send(
            Method::POST,
            "/api/v1/routes",
            Some(&cookie),
            Some(json!({
                "name": "bad route tz",
                "destination_id": dest.id,
                "options": { "timezone": "Mars/Olympus" }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Legacy `channel_id` fallback resolves when channel has exactly 1 destination; route `timezone` persists.
    let (status, _, route) = h
        .send(
            Method::POST,
            "/api/v1/routes",
            Some(&cookie),
            Some(json!({
                "name": "night",
                "channel_id": channel.id,
                "options": {
                    "locale": "zh-CN",
                    "timezone": "Asia/Tokyo",
                    "active_windows": [{ "start": "22:00", "end": "06:00" }],
                    "active_weekdays": [1, 2, 3, 4, 5, 6, 7]
                }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{route}");
    assert_eq!(route["destination_id"], dest.id);
    assert_eq!(route["options"]["locale"], "zh-CN");
    assert_eq!(route["options"]["timezone"], "Asia/Tokyo");
    assert_eq!(route["options"]["active_windows"][0]["end"], "06:00");
    assert_eq!(route["options"]["active_weekdays"], json!([]));
}

#[tokio::test]
async fn username_login_totp_two_factor_recovery_codes_and_account_security() {
    let h = Harness::new().await;
    let cookie = h.setup_admin(Some("Alice"), "alice-strong-password").await;

    // Wrong username with the right password is rejected generically.
    let (status, _, body) = h
        .send(
            Method::POST,
            "/api/v1/auth/login",
            None,
            Some(json!({ "username": "Courier", "password": "alice-strong-password" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "Invalid username or password");

    let (status, _, account) = h
        .send(Method::GET, "/api/v1/account", Some(&cookie), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(account["username"], "Alice");
    assert_eq!(account["totp_enabled"], false);
    assert_eq!(account["username_env_managed"], false);

    // 2FA setup requires the current password.
    let (status, _, _) = h
        .send(
            Method::POST,
            "/api/v1/account/2fa/setup",
            Some(&cookie),
            Some(json!({ "current_password": "wrong-password-here" })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, setup) = h
        .send(
            Method::POST,
            "/api/v1/account/2fa/setup",
            Some(&cookie),
            Some(json!({ "current_password": "alice-strong-password" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{setup}");
    assert!(setup["qr_svg"].as_str().unwrap().contains("<svg"));
    assert!(
        setup["otpauth_url"]
            .as_str()
            .unwrap()
            .starts_with("otpauth://totp/FeedCourier:Alice?secret=")
    );
    let secret = totp::base32_decode(setup["secret"].as_str().unwrap()).unwrap();

    let step = totp::time_step(now_unix());
    let (status, _, enabled) = h
        .send(
            Method::POST,
            "/api/v1/account/2fa/enable",
            Some(&cookie),
            Some(json!({ "code": totp::code_for_step(&secret, step) })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    let recovery: Vec<String> = enabled["recovery_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(recovery.len(), 10);

    // Login is now two-step; no session cookie is issued before the second factor.
    let login = || json!({ "username": "Alice", "password": "alice-strong-password" });
    let (status, headers, challenge) = h
        .send(Method::POST, "/api/v1/auth/login", None, Some(login()))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(challenge["mfa_required"], true);
    assert_eq!(challenge["ok"], false);
    assert!(headers.get(SET_COOKIE).is_none());
    let mfa_token = challenge["mfa_token"].as_str().unwrap().to_string();

    // Wrong code is rejected; the code consumed during enable cannot be replayed.
    let (status, _, _) = h
        .send(
            Method::POST,
            "/api/v1/auth/login/2fa",
            None,
            Some(json!({ "mfa_token": mfa_token, "code": "000000" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = h
        .send(
            Method::POST,
            "/api/v1/auth/login/2fa",
            None,
            Some(json!({ "mfa_token": mfa_token, "code": totp::code_for_step(&secret, step) })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "same-step replay must fail"
    );

    // A fresh time-step code works.
    let (status, headers, ok) = h
        .send(
            Method::POST,
            "/api/v1/auth/login/2fa",
            None,
            Some(json!({ "mfa_token": mfa_token, "code": totp::code_for_step(&secret, step + 1) })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{ok}");
    assert_eq!(ok["ok"], true);
    let session = cookie_pair(&headers);
    let (status, _, _) = h
        .send(Method::GET, "/api/v1/overview", Some(&session), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    // The challenge token is single-use.
    let (status, _, _) = h
        .send(
            Method::POST,
            "/api/v1/auth/login/2fa",
            None,
            Some(json!({ "mfa_token": mfa_token, "code": recovery[0] })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Recovery codes work once, case/dash-insensitively.
    let (_, _, challenge) = h
        .send(Method::POST, "/api/v1/auth/login", None, Some(login()))
        .await;
    let token2 = challenge["mfa_token"].as_str().unwrap().to_string();
    let (status, _, _) = h
        .send(
            Method::POST,
            "/api/v1/auth/login/2fa",
            None,
            Some(
                json!({ "mfa_token": token2, "code": recovery[0].to_lowercase().replace('-', "") }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, account) = h
        .send(Method::GET, "/api/v1/account", Some(&session), None)
        .await;
    assert_eq!(account["totp_enabled"], true);
    assert_eq!(account["recovery_codes_remaining"], 9);

    // Changing the password with 2FA enabled needs a verification code and revokes other sessions.
    let (status, _, _) = h
        .send(
            Method::PATCH,
            "/api/v1/account",
            Some(&session),
            Some(json!({ "current_password": "alice-strong-password", "new_password": "alice-new-password-1" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "code is required");
    let (status, _, account) = h
        .send(
            Method::PATCH,
            "/api/v1/account",
            Some(&session),
            Some(json!({
                "current_password": "alice-strong-password",
                "new_password": "alice-new-password-1",
                "new_username": "Bob",
                "code": recovery[1],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{account}");
    assert_eq!(account["username"], "Bob");
    let (status, _, _) = h
        .send(Method::GET, "/api/v1/overview", Some(&session), None)
        .await;
    assert_eq!(status, StatusCode::OK, "current session survives");
    let (status, _, _) = h
        .send(Method::GET, "/api/v1/overview", Some(&cookie), None)
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "other sessions are revoked"
    );

    // Regenerate recovery codes, then disable 2FA with password + code.
    let (status, _, regen) = h
        .send(
            Method::POST,
            "/api/v1/account/2fa/recovery-codes",
            Some(&session),
            Some(json!({ "current_password": "alice-new-password-1" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let fresh = regen["recovery_codes"][0].as_str().unwrap().to_string();
    let (status, _, account) = h
        .send(
            Method::POST,
            "/api/v1/account/2fa/disable",
            Some(&session),
            Some(json!({ "current_password": "alice-new-password-1", "code": fresh })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{account}");
    assert_eq!(account["totp_enabled"], false);

    let (status, _, direct) = h
        .send(
            Method::POST,
            "/api/v1/auth/login",
            None,
            Some(json!({ "username": "Bob", "password": "alice-new-password-1" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(direct["mfa_required"], false);
}

#[tokio::test]
async fn setup_defaults_username_to_courier() {
    let h = Harness::new().await;
    let cookie = h.setup_admin(None, "courier-default-password").await;
    let (_, _, account) = h
        .send(Method::GET, "/api/v1/account", Some(&cookie), None)
        .await;
    assert_eq!(account["username"], "Courier");
}
