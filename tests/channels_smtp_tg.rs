//! Integration tests for Wave 2a SMTP (`lettre` + `minijinja` + `html2text`) and
//! Telegram (`Bot API 10.4` + `telegram-rich` + `media_mode`) channel senders.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use serde_json::{Value, json};
use tempfile as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_test as _;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use feedcourier::channels::smtp::{MAX_EMAIL_HTML_BYTES, SmtpSender, deterministic_message_id};
use feedcourier::channels::telegram::{
    DefaultMediaStreamFetcher, FetchedMedia, MAX_CAPTION_UTF16_UNITS, MAX_MESSAGE_UTF16_UNITS,
    MAX_PHOTO_BYTES, MediaStreamFetcher, TelegramSender, format_compact_card_html,
    format_media_mode_caption,
};
use feedcourier::channels::{
    ChannelConfig, ChannelKind, ChannelSender, CheckpointSink, DeliveryCheckpoint, DeliveryContext,
    DeliveryOutcome, RouteOptions, SmtpChannelConfig, SmtpSecurity, TelegramCapabilities,
    TelegramChannelConfig, TelegramDeliveryMode,
};
use feedcourier::crypto::sha256_hex;
use feedcourier::db::models::{
    Channel, DeliveryTask, DeliveryTaskStatus, Entry, Feed, FeedHealthStatus, MediaAttachment,
    MediaAttachmentKind, Route,
};
use feedcourier::error::{AppError, AppResult};

// =============================================================================
// Shared Fixture Builders
// =============================================================================

fn sample_feed(id: i64, title: &str) -> Feed {
    Feed {
        id,
        url: "https://blog.example.com/rss.xml".to_owned(),
        title: title.to_owned(),
        site_url: Some("https://blog.example.com".to_owned()),
        favicon_url: Some("https://blog.example.com/favicon.ico".to_owned()),
        tags_json: r#"["rust","systems"]"#.to_owned(),
        poll_interval_secs: 1800,
        fetch_full_content: false,
        use_proxy: false,
        etag: None,
        last_modified: None,
        health_status: FeedHealthStatus::Active,
        consecutive_failures: 0,
        next_poll_at: Some(1_770_000_000),
        last_error: None,
        silent_init: true,
        created_at: 1_770_000_000,
        updated_at: 1_770_000_000,
    }
}

fn sample_entry(fp_seed: &str, media: Vec<MediaAttachment>) -> Entry {
    Entry {
        id: 42,
        feed_id: 7,
        fingerprint_sha256: sha256_hex(fp_seed.as_bytes()),
        published_at: Some(1_770_000_600),
        discovered_at: 1_770_000_700,
        payload_purged_at: None,
        title: Some("Zero-Copy Streaming Gateways in Rust".to_owned()),
        canonical_url: Some("https://blog.example.com/posts/zero-copy-rust".to_owned()),
        author: Some("Alice Chen".to_owned()),
        content_html: Some(
            r#"<div class="post"><p>Welcome to <b>FeedCourier</b>! See <a href="/docs">the guide</a>.</p><img src="/assets/diagram.png" alt="Diagram"><script>evil()</script></div>"#
                .to_owned(),
        ),
        first_sentence: Some("Welcome to FeedCourier, a pure-Rust delivery gateway.".to_owned()),
        summary_text: Some(
            "Welcome to FeedCourier, a pure-Rust delivery gateway built for low memory footprint."
                .to_owned(),
        ),
        media_json: Some(serde_json::to_string(&media).expect("serialize media")),
    }
}

fn sample_channel(kind: ChannelKind, capabilities_json: &str) -> Channel {
    Channel {
        id: 11,
        name: "Test Channel".to_owned(),
        kind,
        encrypted_config: Vec::new(),
        credential_group_key: "cred-group-1".to_owned(),
        capabilities_json: capabilities_json.to_owned(),
        min_interval_secs: 10,
        jitter_max_secs: 5,
        hourly_quota: None,
        daily_quota: None,
        use_proxy: false,
        cooldown_until: None,
        enabled: true,
        created_at: 1_770_000_000,
        updated_at: 1_770_000_000,
    }
}

fn sample_route(id: i64, options: RouteOptions) -> Route {
    Route {
        id,
        name: "Primary Route".to_owned(),
        feed_ids_json: "[7]".to_owned(),
        match_tags_json: "[]".to_owned(),
        channel_id: 11,
        include_keywords_json: "[]".to_owned(),
        exclude_keywords_json: "[]".to_owned(),
        include_regex: None,
        exclude_regex: None,
        options_json: serde_json::to_string(&options).expect("serialize route options"),
        enabled: true,
        created_at: 1_770_000_000,
        updated_at: 1_770_000_000,
    }
}

fn sample_task(route_id: i64) -> DeliveryTask {
    DeliveryTask {
        id: 99,
        route_id,
        channel_id: 11,
        entry_id: 42,
        status: DeliveryTaskStatus::Delivering,
        lease_until: Some(1_770_001_000),
        checkpoint_json: None,
        attempt_count: 1,
        next_attempt_at: 1_770_000_700,
        last_status_code: None,
        diagnostic_log: None,
        completed_at: None,
        created_at: 1_770_000_700,
    }
}

#[derive(Default)]
struct RecordingCheckpointSink {
    saved: Mutex<Vec<DeliveryCheckpoint>>,
}

impl RecordingCheckpointSink {
    fn snapshots(&self) -> Vec<DeliveryCheckpoint> {
        self.saved.lock().expect("lock checkpoints").clone()
    }
}

#[async_trait]
impl CheckpointSink for RecordingCheckpointSink {
    async fn save_checkpoint(&self, checkpoint: &DeliveryCheckpoint) -> AppResult<()> {
        self.saved
            .lock()
            .expect("lock checkpoints")
            .push(checkpoint.clone());
        Ok(())
    }
}

// =============================================================================
// 1. SMTP Channel Sender Tests
// =============================================================================

#[test]
fn smtp_headers_force_from_multipart_and_95kib_truncation() {
    let smtp_cfg = SmtpChannelConfig {
        host: "smtp.verified.example.com".to_owned(),
        port: 587,
        username: Some("postmaster@verified.example.com".to_owned()),
        password: Some("secret".to_owned()),
        security: SmtpSecurity::StartTls,
        from_address: "alerts@verified.example.com".to_owned(),
        to_addresses: vec![
            "Reader One <reader1@inbox.example.org>".to_owned(),
            "reader2@inbox.example.org".to_owned(),
        ],
        reply_to: Some("noreply@verified.example.com".to_owned()),
    };

    // Include control chars in feed title to verify header sanitization.
    let feed = sample_feed(7, "Rust\r\n  Weekly\tDigest");
    let entry = sample_entry("smtp-entry-1", Vec::new());

    let built1 =
        SmtpSender::build_entry_email(&smtp_cfg, &feed, &entry, 19, None).expect("build email 1");
    let built2 =
        SmtpSender::build_entry_email(&smtp_cfg, &feed, &entry, 19, None).expect("build email 2");

    let expected_msg_id = format!(
        "<fc-{}-r19@verified.example.com>",
        &entry.fingerprint_sha256[..24]
    );
    assert_eq!(built1.message_id, expected_msg_id);
    assert_eq!(
        built1.message_id,
        deterministic_message_id(&entry.fingerprint_sha256, 19, "verified.example.com")
    );
    assert_eq!(
        built1.message_id, built2.message_id,
        "Message-ID must be deterministic across retries"
    );
    assert!(!built1.html_truncated);

    let formatted = String::from_utf8(built1.message.formatted()).expect("valid UTF-8 MIME");
    assert!(
        formatted.contains("Rust Weekly Digest via FeedCourier")
            && formatted.contains("<alerts@verified.example.com>"),
        "Force-From must combine sanitized feed title with strict SPF/DKIM from_address:\n{formatted}"
    );
    assert!(
        formatted.contains("Auto-Submitted: auto-generated\r\n"),
        "missing Auto-Submitted header:\n{formatted}"
    );
    assert!(
        formatted.contains("List-Id: <feed-7.verified.example.com>\r\n"),
        "missing List-Id header:\n{formatted}"
    );
    assert!(
        formatted.contains(&format!("Message-ID: {expected_msg_id}\r\n")),
        "missing deterministic Message-ID header:\n{formatted}"
    );
    assert!(formatted.contains("Reply-To: noreply@verified.example.com\r\n"));
    assert!(formatted.contains("Content-Type: multipart/alternative;"));
    assert!(formatted.contains("Content-Type: text/plain;"));
    assert!(formatted.contains("Content-Type: text/html;"));
    assert!(built1.html_body.contains("max-width:640px"));
    assert!(built1.html_body.contains("Read original →"));
    assert!(built1.plain_body.contains("Welcome to"));
    assert!(built1.plain_body.contains("Read original"));

    // Verify >95 KiB HTML truncation keeps HTML strictly <= MAX_EMAIL_HTML_BYTES with clean tag closure.
    let mut huge_entry = entry.clone();
    huge_entry.title = None; // Also verify fallback subject!
    let mut giant_html = String::from("<article><section><div class=\"content\">");
    for i in 0..2_200 {
        giant_html.push_str(&format!(
            "<p>Paragraph {i}: Memory-safe asynchronous streaming in Rust prevents unbounded heap growth &amp; latency spikes.</p>"
        ));
    }
    giant_html.push_str("</div></section></article>");
    assert!(giant_html.len() > 180 * 1024);
    huge_entry.content_html = Some(giant_html);

    let truncated_email = SmtpSender::build_entry_email(&smtp_cfg, &feed, &huge_entry, 19, None)
        .expect("build truncated email");
    assert!(truncated_email.html_truncated);
    assert!(
        truncated_email.html_body.len() <= MAX_EMAIL_HTML_BYTES,
        "HTML part ({} bytes) must stay <= {} bytes (95 KiB)",
        truncated_email.html_body.len(),
        MAX_EMAIL_HTML_BYTES
    );
    assert!(
        truncated_email
            .html_body
            .contains("Continue reading original article →"),
        "truncated HTML must contain continuation link"
    );
    assert!(
        truncated_email
            .html_body
            .contains("</div></section></article>"),
        "open HTML tags should be closed cleanly on truncation"
    );
    let truncated_mime = String::from_utf8(truncated_email.message.formatted()).expect("utf8");
    assert!(truncated_mime.contains("Subject: Untitled update from Rust Weekly Digest\r\n"));
}

#[derive(Clone, Copy)]
enum MockSmtpBehavior {
    Accept250,
    Transient421,
    Permanent550,
}

#[derive(Default)]
struct CapturedSmtpSession {
    commands: Vec<String>,
    raw_data: String,
}

async fn spawn_mock_smtp_server(
    behavior: MockSmtpBehavior,
) -> (
    u16,
    Arc<Mutex<Vec<CapturedSmtpSession>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock SMTP listener");
    let port = listener.local_addr().expect("local addr").port();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);

    let handle = tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let (reader_half, mut writer_half) = stream.into_split();
        let mut reader = BufReader::new(reader_half);
        let mut session = CapturedSmtpSession::default();

        let _ = writer_half
            .write_all(b"220 mock.verified.example.com ESMTP FeedCourierTest\r\n")
            .await;

        let mut line = String::new();
        loop {
            line.clear();
            let Ok(n) = reader.read_line(&mut line).await else {
                break;
            };
            if n == 0 {
                break;
            }
            let cmd = line.trim_end_matches(['\r', '\n']).to_owned();
            let upper = cmd.to_ascii_uppercase();
            session.commands.push(cmd);

            if upper.starts_with("EHLO") || upper.starts_with("HELO") {
                let _ = writer_half
                    .write_all(b"250-mock.verified.example.com\r\n250 8BITMIME\r\n")
                    .await;
            } else if upper.starts_with("MAIL FROM:") {
                let _ = writer_half.write_all(b"250 2.1.0 Originator OK\r\n").await;
            } else if upper.starts_with("RCPT TO:") {
                match behavior {
                    MockSmtpBehavior::Permanent550 => {
                        let _ = writer_half
                            .write_all(b"550 5.1.1 <bad@dest.example.org>: Recipient address rejected: User unknown\r\n")
                            .await;
                    }
                    _ => {
                        let _ = writer_half.write_all(b"250 2.1.5 Recipient OK\r\n").await;
                    }
                }
            } else if upper == "DATA" {
                let _ = writer_half
                    .write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
                    .await;
                loop {
                    line.clear();
                    let Ok(dn) = reader.read_line(&mut line).await else {
                        break;
                    };
                    if dn == 0 || line == ".\r\n" || line == ".\n" {
                        break;
                    }
                    session.raw_data.push_str(&line);
                }
                match behavior {
                    MockSmtpBehavior::Accept250 => {
                        let _ = writer_half
                            .write_all(b"250 2.0.0 Ok: queued as MOCK250\r\n")
                            .await;
                    }
                    MockSmtpBehavior::Transient421 => {
                        let _ = writer_half
                            .write_all(
                                b"421 4.7.0 Upstream rate limit exceeded, retry after 12 seconds\r\n",
                            )
                            .await;
                        break;
                    }
                    MockSmtpBehavior::Permanent550 => {
                        let _ = writer_half
                            .write_all(b"550 5.7.1 Message rejected\r\n")
                            .await;
                    }
                }
            } else if upper == "RSET" {
                let _ = writer_half.write_all(b"250 2.0.0 Reset OK\r\n").await;
            } else if upper == "NOOP" {
                let _ = writer_half.write_all(b"250 2.0.0 OK\r\n").await;
            } else if upper == "QUIT" {
                let _ = writer_half.write_all(b"221 2.0.0 Bye\r\n").await;
                break;
            } else {
                let _ = writer_half.write_all(b"250 OK\r\n").await;
            }
        }

        captured_clone.lock().expect("lock sessions").push(session);
    });

    (port, captured, handle)
}

#[tokio::test]
async fn smtp_live_mock_tcp_handshake_250_421_and_550() {
    let feed = sample_feed(7, "Systems Engineering");
    let entry = sample_entry("smtp-live-fp", Vec::new());
    let channel = sample_channel(ChannelKind::Smtp, "{}");
    let route = sample_route(5, RouteOptions::default());
    let task = sample_task(route.id);

    // 1) 250 OK handshake
    {
        let (port, captured, handle) = spawn_mock_smtp_server(MockSmtpBehavior::Accept250).await;
        let sender = SmtpSender::new();
        let cfg = ChannelConfig::Smtp(SmtpChannelConfig {
            host: "127.0.0.1".to_owned(),
            port,
            username: None,
            password: None,
            security: SmtpSecurity::Plain,
            from_address: "alerts@verified.example.com".to_owned(),
            to_addresses: vec!["reader@dest.example.org".to_owned()],
            reply_to: None,
        });

        let ctx = DeliveryContext {
            task: &task,
            entry: &entry,
            feed: &feed,
            route: &route,
            route_options: RouteOptions::default(),
            channel: &channel,
            channel_config: &cfg,
            checkpoint: None,
            checkpoint_sink: None,
        };

        let outcome = sender.send(&ctx).await.expect("send 250");
        match outcome {
            DeliveryOutcome::Delivered {
                status_code,
                checkpoint: Some(DeliveryCheckpoint::Smtp { message_id }),
                ..
            } => {
                assert_eq!(status_code, Some(250));
                assert!(message_id.starts_with("<fc-"));
                assert!(message_id.ends_with("-r5@verified.example.com>"));
            }
            other => panic!("expected Delivered(250), got {other:?}"),
        }

        drop(sender);
        let _ = handle.await;

        let sessions = captured.lock().expect("lock sessions");
        assert_eq!(sessions.len(), 1);
        assert!(
            sessions[0]
                .commands
                .iter()
                .any(|c| c.eq_ignore_ascii_case("MAIL FROM:<alerts@verified.example.com>")),
            "envelope MAIL FROM must match Force-From address: {:?}",
            sessions[0].commands
        );
        assert!(
            sessions[0]
                .commands
                .iter()
                .any(|c| c.eq_ignore_ascii_case("RCPT TO:<reader@dest.example.org>"))
        );
        assert!(
            sessions[0]
                .raw_data
                .contains("Auto-Submitted: auto-generated")
        );
        assert!(
            sessions[0]
                .raw_data
                .contains("List-Id: <feed-7.verified.example.com>")
        );
    }

    // 1b) Interactive `send_test` against 250 OK mock relay
    {
        let (port, _captured, handle) = spawn_mock_smtp_server(MockSmtpBehavior::Accept250).await;
        let sender = SmtpSender::new();
        let cfg = ChannelConfig::Smtp(SmtpChannelConfig {
            host: "127.0.0.1".to_owned(),
            port,
            username: None,
            password: None,
            security: SmtpSecurity::Plain,
            from_address: "alerts@verified.example.com".to_owned(),
            to_addresses: vec!["reader@dest.example.org".to_owned()],
            reply_to: None,
        });
        let summary = sender.send_test(&cfg, false).await.expect("send_test 250");
        assert!(summary.contains("250"));
        assert!(summary.contains("alerts@verified.example.com"));
        drop(sender);
        let _ = handle.await;
    }

    // 2) 421 Transient error -> Retry with cooldown_credential_group = true
    {
        let (port, _captured, handle) =
            spawn_mock_smtp_server(MockSmtpBehavior::Transient421).await;
        let sender = SmtpSender::new();
        let cfg = ChannelConfig::Smtp(SmtpChannelConfig {
            host: "127.0.0.1".to_owned(),
            port,
            username: None,
            password: None,
            security: SmtpSecurity::Plain,
            from_address: "alerts@verified.example.com".to_owned(),
            to_addresses: vec!["reader@dest.example.org".to_owned()],
            reply_to: None,
        });

        let ctx = DeliveryContext {
            task: &task,
            entry: &entry,
            feed: &feed,
            route: &route,
            route_options: RouteOptions::default(),
            channel: &channel,
            channel_config: &cfg,
            checkpoint: None,
            checkpoint_sink: None,
        };

        let outcome = sender.send(&ctx).await.expect("send 421");
        match outcome {
            DeliveryOutcome::Retry {
                retry_after_secs,
                status_code,
                checkpoint: Some(DeliveryCheckpoint::Smtp { message_id }),
                cooldown_credential_group,
                ..
            } => {
                assert_eq!(status_code, Some(421));
                assert_eq!(retry_after_secs, Some(12));
                assert!(
                    cooldown_credential_group,
                    "421 rate limit must trigger credential group cooldown"
                );
                assert!(message_id.starts_with("<fc-"));
            }
            other => panic!("expected Retry(421), got {other:?}"),
        }

        drop(sender);
        let _ = handle.await;
    }

    // 3) 550 Permanent error -> PermanentFailure
    {
        let (port, _captured, handle) =
            spawn_mock_smtp_server(MockSmtpBehavior::Permanent550).await;
        let sender = SmtpSender::new();
        let cfg = ChannelConfig::Smtp(SmtpChannelConfig {
            host: "127.0.0.1".to_owned(),
            port,
            username: None,
            password: None,
            security: SmtpSecurity::Plain,
            from_address: "alerts@verified.example.com".to_owned(),
            to_addresses: vec!["bad@dest.example.org".to_owned()],
            reply_to: None,
        });

        let ctx = DeliveryContext {
            task: &task,
            entry: &entry,
            feed: &feed,
            route: &route,
            route_options: RouteOptions::default(),
            channel: &channel,
            channel_config: &cfg,
            checkpoint: None,
            checkpoint_sink: None,
        };

        let outcome = sender.send(&ctx).await.expect("send 550");
        match outcome {
            DeliveryOutcome::PermanentFailure {
                status_code,
                checkpoint: Some(DeliveryCheckpoint::Smtp { message_id }),
                diagnostic,
            } => {
                assert_eq!(status_code, Some(550));
                assert!(message_id.starts_with("<fc-"));
                assert!(diagnostic.contains("550"));
            }
            other => panic!("expected PermanentFailure(550), got {other:?}"),
        }

        drop(sender);
        let _ = handle.await;
    }
}

// =============================================================================
// 2. Telegram Channel Sender Tests (`wiremock`)
// =============================================================================

#[tokio::test]
async fn telegram_probe_capabilities_10_4_vs_legacy_404() {
    let sender = TelegramSender::new();

    // Case A: Bot API 10.4 server returns `400 Bad Request: chat_id is empty` on `{}` probe
    let modern_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/botTEST_TOKEN/getMe"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": {
                "id": 123456,
                "is_bot": true,
                "first_name": "FeedCourier",
                "username": "feedcourier_bot"
            }
        })))
        .mount(&modern_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/botTEST_TOKEN/sendRichMessage"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "ok": false,
            "error_code": 400,
            "description": "Bad Request: chat_id is empty"
        })))
        .mount(&modern_server)
        .await;

    let modern_cfg = TelegramChannelConfig {
        bot_token: "TEST_TOKEN".to_owned(),
        chat_id: "-100998877".to_owned(),
        message_thread_id: Some(12),
        api_base_url: Some(modern_server.uri()),
    };

    let caps = sender
        .probe_capabilities(&modern_cfg, false)
        .await
        .expect("probe 10.4");
    assert!(caps.supports_rich_message);
    assert_eq!(caps.bot_username.as_deref(), Some("feedcourier_bot"));
    assert!(caps.fallback_reason.is_none());
    assert!(caps.probed_at.is_some());

    // Case B: Legacy self-hosted Local Bot API Server `<10.2` returns `404 Not Found`
    let legacy_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/botLEGACY_TOK/getMe"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": {
                "id": 654321,
                "is_bot": true,
                "first_name": "LegacyBot",
                "username": "legacy_local_bot"
            }
        })))
        .mount(&legacy_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/botLEGACY_TOK/sendRichMessage"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "ok": false,
            "error_code": 404,
            "description": "Not Found"
        })))
        .mount(&legacy_server)
        .await;

    let legacy_cfg = TelegramChannelConfig {
        bot_token: "LEGACY_TOK".to_owned(),
        chat_id: "@mychannel".to_owned(),
        message_thread_id: None,
        api_base_url: Some(legacy_server.uri()),
    };

    let legacy_caps = sender
        .probe_capabilities(&legacy_cfg, false)
        .await
        .expect("probe legacy");
    assert!(!legacy_caps.supports_rich_message);
    assert_eq!(
        legacy_caps.bot_username.as_deref(),
        Some("legacy_local_bot")
    );
    assert!(
        legacy_caps
            .fallback_reason
            .as_deref()
            .is_some_and(|r| r.contains("sendRichMessage"))
    );
}

#[tokio::test]
async fn telegram_rich_message_and_runtime_404_fallback_to_compact_card() {
    let sender = TelegramSender::new();
    let feed = sample_feed(7, "Rust Insider");
    let entry = sample_entry("tg-rich-fp", Vec::new());
    let route_opts = RouteOptions {
        telegram_mode: TelegramDeliveryMode::RichMessage,
        link_preview: false,
    };
    let route = sample_route(3, route_opts.clone());
    let task = sample_task(route.id);

    // 1) Successful `sendRichMessage` with `telegram-rich` normalization + "Read original" button
    {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botTOK1/sendRichMessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "result": { "message_id": 501 }
            })))
            .mount(&server)
            .await;

        let channel = sample_channel(
            ChannelKind::Telegram,
            r#"{"supports_rich_message":true,"bot_username":"fc_bot"}"#,
        );
        let cfg = ChannelConfig::Telegram(TelegramChannelConfig {
            bot_token: "TOK1".to_owned(),
            chat_id: "-10012345".to_owned(),
            message_thread_id: Some(88),
            api_base_url: Some(server.uri()),
        });
        let sink = RecordingCheckpointSink::default();

        let ctx = DeliveryContext {
            task: &task,
            entry: &entry,
            feed: &feed,
            route: &route,
            route_options: route_opts.clone(),
            channel: &channel,
            channel_config: &cfg,
            checkpoint: None,
            checkpoint_sink: Some(&sink),
        };

        let outcome = sender.send(&ctx).await.expect("sendRichMessage");
        match outcome {
            DeliveryOutcome::Delivered {
                status_code,
                checkpoint:
                    Some(DeliveryCheckpoint::Telegram {
                        next_batch_offset,
                        sent_message_ids,
                        degraded_to_compact,
                    }),
                updated_capabilities,
                ..
            } => {
                assert_eq!(status_code, Some(200));
                assert_eq!(next_batch_offset, 1);
                assert_eq!(sent_message_ids, vec![501]);
                assert!(!degraded_to_compact);
                assert!(updated_capabilities.is_none());
            }
            other => panic!("expected Delivered RichMessage, got {other:?}"),
        }

        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 1);
        let body_json: Value = serde_json::from_slice(&requests[0].body).expect("json body");
        assert_eq!(body_json["chat_id"], "-10012345");
        assert_eq!(body_json["message_thread_id"], 88);
        let rich_html = body_json["rich_message"]["html"]
            .as_str()
            .expect("rich_message.html");
        assert!(
            rich_html.contains("<h1><a href=\"https://blog.example.com/posts/zero-copy-rust\">Zero-Copy Streaming Gateways in Rust</a></h1>"),
            "missing linked <h1> header in rich_html: {rich_html}"
        );
        assert!(
            rich_html.contains("https://blog.example.com/docs"),
            "relative link must be resolved against entry base URL: {rich_html}"
        );
        assert!(
            rich_html.contains("<img src=\"https://blog.example.com/assets/diagram.png\"/>"),
            "inline image must be normalized & lifted to top-level media block: {rich_html}"
        );
        assert!(
            !rich_html.contains("<script>"),
            "scripts must be stripped by telegram-rich: {rich_html}"
        );
        assert!(
            rich_html.contains(
                "<tg-button-row><tg-button type=\"url\" url=\"https://blog.example.com/posts/zero-copy-rust\">Read original</tg-button></tg-button-row>"
            ),
            "missing Read original button row: {rich_html}"
        );
    }

    // 2) Runtime fallback: server returns 404 on `sendRichMessage` -> auto-degrade to `CompactCard`
    {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botTOK2/sendRichMessage"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "ok": false,
                "error_code": 404,
                "description": "Not Found: method sendRichMessage not found"
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/botTOK2/sendMessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "result": { "message_id": 777 }
            })))
            .mount(&server)
            .await;

        let channel = sample_channel(ChannelKind::Telegram, r#"{"bot_username":"fc_bot"}"#);
        let cfg = ChannelConfig::Telegram(TelegramChannelConfig {
            bot_token: "TOK2".to_owned(),
            chat_id: "@rust_news".to_owned(),
            message_thread_id: None,
            api_base_url: Some(server.uri()),
        });

        let ctx = DeliveryContext {
            task: &task,
            entry: &entry,
            feed: &feed,
            route: &route,
            route_options: route_opts.clone(),
            channel: &channel,
            channel_config: &cfg,
            checkpoint: None,
            checkpoint_sink: None,
        };

        let outcome = sender.send(&ctx).await.expect("runtime fallback");
        match outcome {
            DeliveryOutcome::Delivered {
                checkpoint:
                    Some(DeliveryCheckpoint::Telegram {
                        sent_message_ids,
                        degraded_to_compact,
                        ..
                    }),
                updated_capabilities: Some(caps_val),
                ..
            } => {
                assert_eq!(sent_message_ids, vec![777]);
                assert!(
                    degraded_to_compact,
                    "checkpoint must record degraded_to_compact = true"
                );
                let parsed_caps: TelegramCapabilities =
                    serde_json::from_value(caps_val).expect("deserialize updated capabilities");
                assert!(!parsed_caps.supports_rich_message);
                assert_eq!(parsed_caps.bot_username.as_deref(), Some("fc_bot"));
                assert!(parsed_caps.fallback_reason.is_some());
            }
            other => panic!("expected Delivered with fallback capabilities, got {other:?}"),
        }

        let requests = server.received_requests().await.expect("requests");
        assert_eq!(
            requests.len(),
            2,
            "1 sendRichMessage + 1 fallback sendMessage"
        );
        let compact_req: Value = serde_json::from_slice(&requests[1].body).expect("compact json");
        assert_eq!(compact_req["parse_mode"], "HTML");
        assert_eq!(compact_req["link_preview_options"]["is_disabled"], true);
        assert_eq!(
            compact_req["link_preview_options"]["url"],
            "https://blog.example.com/posts/zero-copy-rust"
        );
        let text = compact_req["text"].as_str().expect("text");
        assert!(text.contains("<b>Zero-Copy Streaming Gateways in Rust</b>"));
        assert!(text.contains("<i>Rust Insider · Alice Chen</i>"));
        assert!(text.contains(
            "<a href=\"https://blog.example.com/posts/zero-copy-rust\">Read original</a>"
        ));
    }

    // 3) Pre-probed `supports_rich_message == false` skips `sendRichMessage` directly + `send_test`
    {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/botTOK3/getMe"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "result": { "id": 9, "is_bot": true, "username": "cached_bot" }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/botTOK3/sendRichMessage"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "ok": false,
                "error_code": 404,
                "description": "Not Found"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/botTOK3/sendMessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "result": { "message_id": 888 }
            })))
            .mount(&server)
            .await;

        let channel = sample_channel(
            ChannelKind::Telegram,
            r#"{"supports_rich_message":false,"bot_username":"cached_bot"}"#,
        );
        let cfg = ChannelConfig::Telegram(TelegramChannelConfig {
            bot_token: "TOK3".to_owned(),
            chat_id: "987654".to_owned(),
            message_thread_id: None,
            api_base_url: Some(server.uri()),
        });

        let ctx = DeliveryContext {
            task: &task,
            entry: &entry,
            feed: &feed,
            route: &route,
            route_options: route_opts,
            channel: &channel,
            channel_config: &cfg,
            checkpoint: None,
            checkpoint_sink: None,
        };
        let outcome = sender.send(&ctx).await.expect("cached degrade");
        assert!(matches!(
            outcome,
            DeliveryOutcome::Delivered {
                checkpoint: Some(DeliveryCheckpoint::Telegram {
                    degraded_to_compact: true,
                    ..
                }),
                ..
            }
        ));
        let reqs = server.received_requests().await.expect("reqs");
        assert_eq!(
            reqs.len(),
            1,
            "pre-cached supports_rich_message=false must skip sendRichMessage and call only sendMessage"
        );
        assert_eq!(reqs[0].url.path(), "/botTOK3/sendMessage");

        let test_msg = sender.send_test(&cfg, false).await.expect("tg send_test");
        assert!(test_msg.contains("@cached_bot"));
    }

    // 4) CompactCard <= 4096 UTF-16 code unit strict bound (including surrogate-pair emojis & HTML escaping)
    let mut long_entry = entry.clone();
    long_entry.summary_text = Some("🦀 <Rust & Async> ".repeat(500));
    let compact_html = format_compact_card_html(&feed, &long_entry);
    assert!(
        compact_html.encode_utf16().count() <= MAX_MESSAGE_UTF16_UNITS,
        "CompactCard UTF-16 length {} exceeded 4096",
        compact_html.encode_utf16().count()
    );
    assert!(
        compact_html.ends_with(
            "<a href=\"https://blog.example.com/posts/zero-copy-rust\">Read original</a>"
        )
    );
}

struct SequenceMediaGroupResponder {
    call_count: AtomicUsize,
}

impl Respond for SequenceMediaGroupResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
        let body: Value = serde_json::from_slice(&request.body).expect("sendMediaGroup JSON");
        let items = body["media"].as_array().expect("media array");

        match idx {
            // Attempt 1, Batch 0 (10 images) -> 200 OK
            0 => {
                assert_eq!(items.len(), 10);
                let msgs: Vec<Value> = (0..items.len())
                    .map(|i| json!({ "message_id": 1001 + i as i64 }))
                    .collect();
                ResponseTemplate::new(200).set_body_json(json!({
                    "ok": true,
                    "result": msgs
                }))
            }
            // Attempt 1, Batch 1 (4 images) -> 429 Too Many Requests (`retry_after: 7`)
            1 => {
                assert_eq!(items.len(), 4);
                ResponseTemplate::new(429).set_body_json(json!({
                    "ok": false,
                    "error_code": 429,
                    "description": "Too Many Requests: retry after 7",
                    "parameters": { "retry_after": 7 }
                }))
            }
            // Attempt 2, Resumed at Batch 1 (ONLY remaining 4 images!) -> 200 OK
            2 => {
                assert_eq!(items.len(), 4);
                let msgs: Vec<Value> = (0..items.len())
                    .map(|i| json!({ "message_id": 1011 + i as i64 }))
                    .collect();
                ResponseTemplate::new(200).set_body_json(json!({
                    "ok": true,
                    "result": msgs
                }))
            }
            other => panic!("unexpected sendMediaGroup call #{other}"),
        }
    }
}

#[tokio::test]
async fn telegram_media_mode_caption_14_images_checkpoint_sink_and_429_resume() {
    // 1) Verify strict 3-line minimalist caption rules (including trailing `.` and `。` stripping)
    let feed = sample_feed(7, "Visual Photography");
    let mut zh_entry = sample_entry("caption-zh", Vec::new());
    zh_entry.author = None; // Fallback to `feed.title`
    zh_entry.first_sentence = Some("这是一段按 Unicode 切分的首句测试。".to_owned());
    let cap_zh = format_media_mode_caption(&feed, &zh_entry, MAX_CAPTION_UTF16_UNITS);
    assert_eq!(
        cap_zh,
        "Visual Photography\n这是一段按 Unicode 切分的首句测试\nhttps://blog.example.com/posts/zero-copy-rust"
    );

    let mut no_url_entry = sample_entry("caption-en", Vec::new());
    no_url_entry.canonical_url = None;
    let cap_no_url = format_media_mode_caption(&feed, &no_url_entry, MAX_CAPTION_UTF16_UNITS);
    assert_eq!(
        cap_no_url,
        "Alice Chen\nWelcome to FeedCourier, a pure-Rust delivery gateway"
    );

    // 2) 0-media fallback -> calls `sendMessage` with 3-line text
    let sender = TelegramSender::new();
    let media_opts = RouteOptions {
        telegram_mode: TelegramDeliveryMode::MediaMode,
        link_preview: true,
    };
    let route = sample_route(8, media_opts.clone());
    let task = sample_task(route.id);
    let channel = sample_channel(ChannelKind::Telegram, "{}");

    {
        let zero_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botTOK_ZERO/sendMessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "result": { "message_id": 301 }
            })))
            .mount(&zero_server)
            .await;

        let zero_cfg = ChannelConfig::Telegram(TelegramChannelConfig {
            bot_token: "TOK_ZERO".to_owned(),
            chat_id: "12345".to_owned(),
            message_thread_id: None,
            api_base_url: Some(zero_server.uri()),
        });
        let zero_entry = sample_entry("zero-media", Vec::new());
        let ctx = DeliveryContext {
            task: &task,
            entry: &zero_entry,
            feed: &feed,
            route: &route,
            route_options: media_opts.clone(),
            channel: &channel,
            channel_config: &zero_cfg,
            checkpoint: None,
            checkpoint_sink: None,
        };
        let outcome = sender.send(&ctx).await.expect("0-media send");
        assert!(matches!(outcome, DeliveryOutcome::Delivered { .. }));

        let reqs = zero_server.received_requests().await.expect("reqs");
        assert_eq!(reqs.len(), 1);
        let body: Value = serde_json::from_slice(&reqs[0].body).expect("json");
        assert_eq!(
            body["text"],
            "Alice Chen\nWelcome to FeedCourier, a pure-Rust delivery gateway\nhttps://blog.example.com/posts/zero-copy-rust"
        );
        assert_eq!(body["link_preview_options"]["is_disabled"], false);
    }

    // 3) 14 images (`>10` batched media): Batch 0 (10) flushes `next_batch_offset = 1` to
    //    `CheckpointSink`, Batch 1 (4) hits `429 retry_after: 7`, and retry resumes at `offset = 1`!
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botTOK_BATCH/sendMediaGroup"))
        .respond_with(SequenceMediaGroupResponder {
            call_count: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let fourteen_images: Vec<MediaAttachment> = (1..=14)
        .map(|n| MediaAttachment {
            url: format!("https://cdn.example.com/gallery/img{n:02}.jpg"),
            mime_type: Some("image/jpeg".to_owned()),
            kind: MediaAttachmentKind::Image,
            length_bytes: Some(120_000),
            caption: None,
        })
        .collect();
    let entry_14 = sample_entry("batch-14", fourteen_images);

    let batch_cfg = ChannelConfig::Telegram(TelegramChannelConfig {
        bot_token: "TOK_BATCH".to_owned(),
        chat_id: "-100777".to_owned(),
        message_thread_id: Some(9),
        api_base_url: Some(server.uri()),
    });

    let sink_attempt1 = RecordingCheckpointSink::default();
    let ctx1 = DeliveryContext {
        task: &task,
        entry: &entry_14,
        feed: &feed,
        route: &route,
        route_options: media_opts.clone(),
        channel: &channel,
        channel_config: &batch_cfg,
        checkpoint: None,
        checkpoint_sink: Some(&sink_attempt1),
    };

    let outcome1 = sender.send(&ctx1).await.expect("attempt 1");
    let saved_after_attempt1 = sink_attempt1.snapshots();
    assert_eq!(
        saved_after_attempt1.len(),
        1,
        "CheckpointSink must flush Batch 0 completion BEFORE attempting Batch 1"
    );
    let expected_mid_cp = DeliveryCheckpoint::Telegram {
        next_batch_offset: 1,
        sent_message_ids: (1001..=1010).collect(),
        degraded_to_compact: false,
    };
    assert_eq!(saved_after_attempt1[0], expected_mid_cp);

    let retry_checkpoint = match outcome1 {
        DeliveryOutcome::Retry {
            retry_after_secs,
            status_code,
            checkpoint: Some(cp),
            cooldown_credential_group,
            ..
        } => {
            assert_eq!(retry_after_secs, Some(7));
            assert_eq!(status_code, Some(429));
            assert!(cooldown_credential_group);
            assert_eq!(cp, expected_mid_cp);
            cp
        }
        other => panic!("expected Retry(429) on Batch 1, got {other:?}"),
    };

    // Attempt 2: Resume from `retry_checkpoint` (`next_batch_offset = 1`)
    let sink_attempt2 = RecordingCheckpointSink::default();
    let ctx2 = DeliveryContext {
        task: &task,
        entry: &entry_14,
        feed: &feed,
        route: &route,
        route_options: media_opts,
        channel: &channel,
        channel_config: &batch_cfg,
        checkpoint: Some(retry_checkpoint),
        checkpoint_sink: Some(&sink_attempt2),
    };

    let outcome2 = sender.send(&ctx2).await.expect("attempt 2 resume");
    match outcome2 {
        DeliveryOutcome::Delivered {
            status_code,
            checkpoint:
                Some(DeliveryCheckpoint::Telegram {
                    next_batch_offset,
                    sent_message_ids,
                    degraded_to_compact,
                }),
            ..
        } => {
            assert_eq!(status_code, Some(200));
            assert_eq!(next_batch_offset, 2);
            assert!(!degraded_to_compact);
            let expected_all: Vec<i64> = (1001..=1014).collect();
            assert_eq!(sent_message_ids, expected_all);
        }
        other => panic!("expected Delivered after resume, got {other:?}"),
    }

    // Verify wire calls: Call 0 had img01..img10 + 3-line caption on item 0; Call 2 had ONLY img11..img14!
    let all_reqs = server.received_requests().await.expect("received requests");
    assert_eq!(
        all_reqs.len(),
        3,
        "Batch0(200) + Batch1(429) + ResumedBatch1(200)"
    );

    let req0_json: Value = serde_json::from_slice(&all_reqs[0].body).expect("req0 json");
    assert_eq!(
        req0_json["media"][0]["media"],
        "https://cdn.example.com/gallery/img01.jpg"
    );
    assert_eq!(
        req0_json["media"][0]["caption"],
        "Alice Chen\nWelcome to FeedCourier, a pure-Rust delivery gateway\nhttps://blog.example.com/posts/zero-copy-rust"
    );
    assert!(req0_json["media"][1].get("caption").is_none());

    let req2_json: Value = serde_json::from_slice(&all_reqs[2].body).expect("req2 json");
    let resumed_media = req2_json["media"].as_array().expect("resumed media");
    assert_eq!(resumed_media.len(), 4);
    assert_eq!(
        resumed_media[0]["media"],
        "https://cdn.example.com/gallery/img11.jpg"
    );
    assert_eq!(
        resumed_media[3]["media"],
        "https://cdn.example.com/gallery/img14.jpg"
    );
    assert!(
        resumed_media[0].get("caption").is_none(),
        "subsequent batches must not repeat the 3-line caption"
    );
}

struct MockMediaFetcher {
    blobs: HashMap<String, FetchedMedia>,
}

#[async_trait]
impl MediaStreamFetcher for MockMediaFetcher {
    async fn fetch_media(
        &self,
        url: &str,
        max_bytes: usize,
        _use_proxy: bool,
    ) -> AppResult<FetchedMedia> {
        let Some(media) = self.blobs.get(url) else {
            return Err(AppError::Upstream(format!("mock 404 for {url}")));
        };
        if media.bytes.len() > max_bytes {
            return Err(AppError::Validation(format!(
                "media {} ({} bytes) exceeds cap ({max_bytes} bytes)",
                url,
                media.bytes.len()
            )));
        }
        Ok(media.clone())
    }
}

struct HotlinkFallbackResponder {
    calls: AtomicUsize,
}

impl Respond for HotlinkFallbackResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let content_type = request
            .headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if n == 0 {
            assert!(
                content_type.contains("application/json"),
                "first attempt must be URL pass-through JSON, got {content_type}"
            );
            ResponseTemplate::new(400).set_body_json(json!({
                "ok": false,
                "error_code": 400,
                "description": "Bad Request: WEBPAGE_CURL_FAILED"
            }))
        } else {
            assert!(
                content_type.contains("multipart/form-data"),
                "fallback attempt must stream multipart/form-data, got {content_type}"
            );
            let body_lossy = String::from_utf8_lossy(&request.body);
            assert!(
                body_lossy.contains("attach://file0") && body_lossy.contains("attach://file1"),
                "multipart sendMediaGroup must reference attach://file0 and attach://file1:\n{body_lossy}"
            );
            assert!(body_lossy.contains("FAKE_JPEG_DATA_ONE"));
            assert!(body_lossy.contains("FAKE_JPEG_DATA_TWO"));
            assert!(body_lossy.contains("Alice Chen"));
            ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "result": [
                    { "message_id": 2001 },
                    { "message_id": 2002 }
                ]
            }))
        }
    }
}

#[tokio::test]
async fn telegram_media_mode_webpage_curl_failed_multipart_fallback_and_10mb_cap_degradation() {
    let feed = sample_feed(7, "Hotlink Protected Feed");
    let media_opts = RouteOptions {
        telegram_mode: TelegramDeliveryMode::MediaMode,
        link_preview: true,
    };
    let route = sample_route(12, media_opts.clone());
    let task = sample_task(route.id);
    let channel = sample_channel(ChannelKind::Telegram, "{}");

    // 1) URL pass-through fails with `400 WEBPAGE_CURL_FAILED` -> streaming `attach://` multipart upload succeeds!
    {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botTOK_MP/sendMediaGroup"))
            .respond_with(HotlinkFallbackResponder {
                calls: AtomicUsize::new(0),
            })
            .mount(&server)
            .await;

        let url1 = "https://protected.example.org/img/a.jpg";
        let url2 = "https://protected.example.org/img/b.jpg";
        let mut blobs = HashMap::new();
        blobs.insert(
            url1.to_owned(),
            FetchedMedia {
                bytes: Bytes::from_static(b"FAKE_JPEG_DATA_ONE"),
                content_type: Some("image/jpeg".to_owned()),
                filename: "a.jpg".to_owned(),
            },
        );
        blobs.insert(
            url2.to_owned(),
            FetchedMedia {
                bytes: Bytes::from_static(b"FAKE_JPEG_DATA_TWO"),
                content_type: Some("image/jpeg".to_owned()),
                filename: "b.jpg".to_owned(),
            },
        );

        let sender = TelegramSender::with_media_fetcher(Arc::new(MockMediaFetcher { blobs }));
        let entry = sample_entry(
            "mp-fallback",
            vec![
                MediaAttachment {
                    url: url1.to_owned(),
                    mime_type: Some("image/jpeg".to_owned()),
                    kind: MediaAttachmentKind::Image,
                    length_bytes: None,
                    caption: None,
                },
                MediaAttachment {
                    url: url2.to_owned(),
                    mime_type: Some("image/jpeg".to_owned()),
                    kind: MediaAttachmentKind::Image,
                    length_bytes: None,
                    caption: None,
                },
            ],
        );

        let cfg = ChannelConfig::Telegram(TelegramChannelConfig {
            bot_token: "TOK_MP".to_owned(),
            chat_id: "-100555".to_owned(),
            message_thread_id: None,
            api_base_url: Some(server.uri()),
        });

        let ctx = DeliveryContext {
            task: &task,
            entry: &entry,
            feed: &feed,
            route: &route,
            route_options: media_opts.clone(),
            channel: &channel,
            channel_config: &cfg,
            checkpoint: None,
            checkpoint_sink: None,
        };

        let outcome = sender.send(&ctx).await.expect("multipart fallback");
        match outcome {
            DeliveryOutcome::Delivered {
                checkpoint:
                    Some(DeliveryCheckpoint::Telegram {
                        next_batch_offset,
                        sent_message_ids,
                        ..
                    }),
                ..
            } => {
                assert_eq!(next_batch_offset, 1);
                assert_eq!(sent_message_ids, vec![2001, 2002]);
            }
            other => panic!("expected Delivered via multipart fallback, got {other:?}"),
        }
    }

    // 2) >10 MiB photo fallback: streaming cap aborts mid-download and gracefully degrades item to text link!
    {
        let media_origin = MockServer::start().await;
        let oversized_photo_bytes = vec![b'X'; MAX_PHOTO_BYTES + 64 * 1024];
        Mock::given(method("GET"))
            .and(path("/huge-photo.jpg"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/jpeg")
                    .set_body_bytes(oversized_photo_bytes),
            )
            .mount(&media_origin)
            .await;

        let tg_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botTOK_CAP/sendPhoto"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "ok": false,
                "error_code": 400,
                "description": "Bad Request: failed to get HTTP URL content"
            })))
            .mount(&tg_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/botTOK_CAP/sendMessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "result": { "message_id": 909 }
            })))
            .mount(&tg_server)
            .await;

        let fetcher = DefaultMediaStreamFetcher::new().with_allow_private_for_tests(true);
        let sender = TelegramSender::with_media_fetcher(Arc::new(fetcher));

        let huge_url = format!("{}/huge-photo.jpg", media_origin.uri());
        let entry = sample_entry(
            "oversized-photo",
            vec![MediaAttachment {
                url: huge_url.clone(),
                mime_type: Some("image/jpeg".to_owned()),
                kind: MediaAttachmentKind::Image,
                length_bytes: None,
                caption: None,
            }],
        );

        let cfg = ChannelConfig::Telegram(TelegramChannelConfig {
            bot_token: "TOK_CAP".to_owned(),
            chat_id: "556677".to_owned(),
            message_thread_id: None,
            api_base_url: Some(tg_server.uri()),
        });

        let ctx = DeliveryContext {
            task: &task,
            entry: &entry,
            feed: &feed,
            route: &route,
            route_options: media_opts.clone(),
            channel: &channel,
            channel_config: &cfg,
            checkpoint: None,
            checkpoint_sink: None,
        };

        let outcome = sender
            .send(&ctx)
            .await
            .expect("oversized photo degradation");
        match outcome {
            DeliveryOutcome::Delivered {
                checkpoint:
                    Some(DeliveryCheckpoint::Telegram {
                        sent_message_ids, ..
                    }),
                ..
            } => {
                assert_eq!(sent_message_ids, vec![909]);
            }
            other => panic!("expected graceful Delivered fallback, got {other:?}"),
        }

        let tg_reqs = tg_server.received_requests().await.expect("tg reqs");
        assert_eq!(
            tg_reqs.len(),
            2,
            "1 sendPhoto URL attempt + 1 degraded sendMessage"
        );
        let degraded_msg: Value = serde_json::from_slice(&tg_reqs[1].body).expect("degraded json");
        let degraded_text = degraded_msg["text"].as_str().expect("text");
        assert!(
            degraded_text.contains(&huge_url),
            "degraded message must include plain link to the oversized photo:\n{degraded_text}"
        );
    }

    // 3) DefaultMediaStreamFetcher SSRF guard blocks non-http(s), loopback, private, and cloud metadata IPs
    {
        let strict_fetcher = DefaultMediaStreamFetcher::new();
        assert!(matches!(
            strict_fetcher
                .fetch_media("file:///etc/passwd", MAX_PHOTO_BYTES, false)
                .await,
            Err(AppError::SsrfBlocked(_))
        ));
        assert!(matches!(
            strict_fetcher
                .fetch_media("http://127.0.0.1:8080/secret.jpg", MAX_PHOTO_BYTES, false)
                .await,
            Err(AppError::SsrfBlocked(_))
        ));
        assert!(matches!(
            strict_fetcher
                .fetch_media(
                    "http://169.254.169.254/latest/meta-data/",
                    MAX_PHOTO_BYTES,
                    false
                )
                .await,
            Err(AppError::SsrfBlocked(_))
        ));

        // Even when loopback is enabled for tests, cloud metadata 169.254.169.254 stays blocked!
        let test_fetcher = DefaultMediaStreamFetcher::new().with_allow_private_for_tests(true);
        assert!(matches!(
            test_fetcher
                .fetch_media(
                    "http://169.254.169.254/latest/meta-data/",
                    MAX_PHOTO_BYTES,
                    false
                )
                .await,
            Err(AppError::SsrfBlocked(_))
        ));
    }

    // 4) 403 Forbidden (bot kicked) -> PermanentFailure
    {
        let kicked_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botTOK_KICK/sendMessage"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "ok": false,
                "error_code": 403,
                "description": "Forbidden: bot was kicked from the channel chat"
            })))
            .mount(&kicked_server)
            .await;

        let sender = TelegramSender::new();
        let entry = sample_entry("kicked-entry", Vec::new());
        let cfg = ChannelConfig::Telegram(TelegramChannelConfig {
            bot_token: "TOK_KICK".to_owned(),
            chat_id: "@kicked_channel".to_owned(),
            message_thread_id: None,
            api_base_url: Some(kicked_server.uri()),
        });
        let ctx = DeliveryContext {
            task: &task,
            entry: &entry,
            feed: &feed,
            route: &route,
            route_options: RouteOptions {
                telegram_mode: TelegramDeliveryMode::CompactCard,
                link_preview: true,
            },
            channel: &channel,
            channel_config: &cfg,
            checkpoint: None,
            checkpoint_sink: None,
        };
        let outcome = sender.send(&ctx).await.expect("403 outcome");
        assert!(matches!(
            outcome,
            DeliveryOutcome::PermanentFailure {
                status_code: Some(403),
                ..
            }
        ));
    }
}
