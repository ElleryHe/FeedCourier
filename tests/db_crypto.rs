//! Integration tests for Wave 1 Rust, Crypto, and SQLite WAL foundation.

use base64::Engine as _;
use tempfile::TempDir;
use tokio_test as _;
use wiremock as _;

use feedcourier::channels::{
    ChannelConfig, ChannelKind, CheckpointSink, DeliveryCheckpoint, NotionChannelConfig,
    NotionColumnMapping, RouteOptions, SmtpChannelConfig, SmtpSecurity, TelegramChannelConfig,
    TelegramDeliveryMode,
};
use feedcourier::config::AppConfig;
use feedcourier::crypto::{
    MasterKey, NONCE_LEN, TAG_LEN, generate_mcp_token, generate_session_token, hash_password,
    hash_token, sha256_hex, verify_password,
};
use feedcourier::db::Database;
use feedcourier::db::models::{
    DeliveryTaskStatus, EntryInsertOutcome, FeedHealthStatus, McpTokenScope, MediaAttachment,
    MediaAttachmentKind, NewChannel, NewEntry, NewFeed, NewRoute, SETTING_SSRF_ALLOWLIST,
};

fn sample_fingerprint(tag: &str) -> String {
    sha256_hex(tag.as_bytes())
}

#[test]
fn master_key_bootstrap_sets_0600_and_reloads_identically() {
    let tmp = TempDir::new().expect("tempdir");
    let nested_dir = tmp.path().join("nested").join("data");
    let config = AppConfig::with_data_dir(&nested_dir);
    let key_path = config.master_key_path();

    assert!(!key_path.exists());

    let key1 = MasterKey::from_config(&config).expect("auto-generate master.key");
    assert!(key_path.exists(), "master.key should be written to disk");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key_path)
            .expect("stat master.key")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "master.key must have strict 0600 permissions");
    }

    let ciphertext = key1.encrypt(b"secret-channel-payload").expect("encrypt");

    // Second load from disk must read the same key without overwriting it.
    let key2 = MasterKey::from_config(&config).expect("reload master.key");
    assert_eq!(key1.to_hex(), key2.to_hex());
    let decrypted = key2
        .decrypt(&ciphertext)
        .expect("decrypt with reloaded key");
    assert_eq!(decrypted, b"secret-channel-payload");

    // Explicit env/string override (both hex and base64) must work without touching disk.
    let raw_bytes = [0x42u8; 32];
    let hex_key = hex::encode(raw_bytes);
    let b64_key = base64::engine::general_purpose::STANDARD.encode(raw_bytes);
    let unused_path = tmp.path().join("should_not_exist.key");

    let from_hex = MasterKey::load_or_init(Some(&hex_key), &unused_path).expect("hex key");
    let from_b64 = MasterKey::load_or_init(Some(&b64_key), &unused_path).expect("b64 key");
    assert!(!unused_path.exists());
    assert_eq!(from_hex.to_hex(), from_b64.to_hex());

    // Invalid key lengths/formats must fail cleanly.
    assert!(MasterKey::from_encoded("deadbeef").is_err());
    assert!(MasterKey::from_encoded("").is_err());
}

#[test]
fn chacha20poly1305_roundtrip_channel_configs_and_tamper_rejection() {
    let key = MasterKey::generate();
    let other_key = MasterKey::generate();

    // Encrypting identical plaintext twice must yield distinct ciphertexts due to random 12B nonce.
    let c1 = key.encrypt(b"same-plaintext").expect("encrypt 1");
    let c2 = key.encrypt(b"same-plaintext").expect("encrypt 2");
    assert_ne!(c1, c2);
    assert_eq!(c1.len(), NONCE_LEN + b"same-plaintext".len() + TAG_LEN);

    // Tamper with nonce, body, and tag -> all must fail AEAD verification.
    for idx in [0, NONCE_LEN, c1.len() - 1] {
        let mut tampered = c1.clone();
        tampered[idx] ^= 0x01;
        assert!(
            key.decrypt(&tampered).is_err(),
            "flipping bit at byte {idx} must be rejected"
        );
    }

    // Wrong key and truncated payloads must fail.
    assert!(other_key.decrypt(&c1).is_err());
    assert!(key.decrypt(&c1[..NONCE_LEN + TAG_LEN - 1]).is_err());

    // JSON roundtrip for Notion channel config + secret redaction + shared credential group key.
    let notion_cfg = ChannelConfig::Notion(NotionChannelConfig {
        integration_secret: "ntn_super_secret_integration_token_1234".to_string(),
        data_source_id: "ds-2026-03-11-abc".to_string(),
        database_id: Some("db-xyz".to_string()),
        parent_page_id: None,
        column_mapping: NotionColumnMapping::standard(),
    });
    notion_cfg.validate().expect("valid notion config");

    let encrypted = key.encrypt_json(&notion_cfg).expect("encrypt_json");
    let roundtripped: ChannelConfig = key.decrypt_json(&encrypted).expect("decrypt_json");
    assert_eq!(roundtripped.kind(), ChannelKind::Notion);
    assert_eq!(
        notion_cfg.credential_group_key(),
        roundtripped.credential_group_key()
    );

    let redacted = roundtripped.redacted_view();
    let redacted_str = serde_json::to_string(&redacted).expect("serialize redacted");
    assert!(
        !redacted_str.contains("super_secret_integration"),
        "redacted_view must never leak raw secret"
    );
    assert_eq!(redacted["column_mapping"]["fc_id"], "FC-ID");
}

#[test]
fn argon2id_password_and_sha256_token_helpers() {
    assert!(hash_password("").is_err(), "empty password must fail");

    let h1 = hash_password("correct-horse-battery-staple").expect("hash 1");
    let h2 = hash_password("correct-horse-battery-staple").expect("hash 2");
    assert!(h1.starts_with("$argon2id$v=19$"));
    assert_ne!(h1, h2, "random salt must produce distinct PHC strings");

    assert!(verify_password("correct-horse-battery-staple", &h1).expect("verify right"));
    assert!(!verify_password("wrong-password", &h1).expect("verify wrong"));

    let (sess_raw, sess_hash) = generate_session_token();
    assert!(sess_raw.starts_with("fcs_"));
    assert_eq!(sess_hash.len(), 64);
    assert_eq!(hash_token(&sess_raw), sess_hash);

    let (mcp_raw, mcp_hash, mcp_prefix) = generate_mcp_token("admin");
    assert!(mcp_raw.starts_with("fcm_adm_"));
    assert_eq!(mcp_hash, hash_token(&mcp_raw));
    assert_eq!(mcp_prefix, &mcp_raw[..12]);
}

#[tokio::test]
async fn sqlite_wal_dual_pool_pragmas_and_readonly_enforcement() {
    let tmp = TempDir::new().expect("tempdir");
    let db_path = tmp.path().join("feedcourier.db");
    let db = Database::connect(&db_path, 4).await.expect("connect db");

    assert_eq!(db.write_pool().options().get_max_connections(), 1);
    assert_eq!(db.read_pool().options().get_max_connections(), 4);

    for (label, pool) in [("write", db.write_pool()), ("read", db.read_pool())] {
        let (journal_mode,): (String,) = sqlx::query_as("PRAGMA journal_mode")
            .fetch_one(pool)
            .await
            .expect("journal_mode");
        let (synchronous,): (i64,) = sqlx::query_as("PRAGMA synchronous")
            .fetch_one(pool)
            .await
            .expect("synchronous");
        let (foreign_keys,): (i64,) = sqlx::query_as("PRAGMA foreign_keys")
            .fetch_one(pool)
            .await
            .expect("foreign_keys");
        let (busy_timeout,): (i64,) = sqlx::query_as("PRAGMA busy_timeout")
            .fetch_one(pool)
            .await
            .expect("busy_timeout");

        assert_eq!(
            journal_mode.to_ascii_lowercase(),
            "wal",
            "{label} pool must run in WAL mode"
        );
        // SQLite PRAGMA synchronous: 1 == NORMAL
        assert_eq!(synchronous, 1, "{label} pool must use synchronous = NORMAL");
        assert_eq!(
            foreign_keys, 1,
            "{label} pool must enforce foreign_keys = ON"
        );
        assert_eq!(
            busy_timeout, 5000,
            "{label} pool must configure busy_timeout = 5000"
        );
    }

    // Accidental write via `read_pool` must be rejected by `PRAGMA query_only = ON`.
    let rogue_write = sqlx::query(
        "INSERT INTO system_settings (key, value_json, updated_at) VALUES ('rogue', '1', 1)",
    )
    .execute(db.read_pool())
    .await;
    assert!(
        rogue_write.is_err(),
        "read_pool must reject mutations under query_only = ON"
    );

    db.close().await;
}

#[tokio::test]
async fn idempotent_entries_routes_atomic_lease_takeover_and_quotas() {
    let tmp = TempDir::new().expect("tempdir");
    let key = MasterKey::generate();
    let db = Database::connect(&tmp.path().join("feedcourier.db"), 4)
        .await
        .expect("connect db");

    // 1. Create two feeds.
    let mut nf1 = NewFeed::new("https://blog.rust-lang.org/feed.xml");
    nf1.title = "Rust Blog".to_string();
    nf1.tags = vec!["rust".to_string(), "programming".to_string()];
    let feed1 = db.create_feed(&nf1).await.expect("create feed1");
    assert_eq!(feed1.tags(), vec!["rust", "programming"]);

    let feed2 = db
        .create_feed(&NewFeed::new("https://tokio.rs/blog/index.xml"))
        .await
        .expect("create feed2");

    // 2. Entry fingerprint deduplication per feed.
    let fp = sample_fingerprint("guid:rust-1.100");
    let new_entry = NewEntry {
        feed_id: feed1.id,
        fingerprint_sha256: fp.clone(),
        published_at: Some(1_700_000_000),
        discovered_at: 1_700_000_010,
        title: Some("Announcing Rust 1.100".to_string()),
        canonical_url: Some("https://blog.rust-lang.org/2026/11/rust-1.100/".to_string()),
        author: Some("Rust Release Team".to_string()),
        content_html: Some("<p>Rust 1.100 is here.</p>".to_string()),
        first_sentence: Some("Rust 1.100 is here".to_string()),
        summary_text: Some("Rust 1.100 release highlights.".to_string()),
        media: vec![MediaAttachment {
            url: "https://blog.rust-lang.org/banner.png".to_string(),
            mime_type: Some("image/png".to_string()),
            kind: MediaAttachmentKind::Image,
            length_bytes: Some(4096),
            caption: None,
        }],
    };

    let entry1 = match db.insert_entry_if_new(&new_entry).await.expect("insert 1") {
        EntryInsertOutcome::Inserted(e) => *e,
        EntryInsertOutcome::Duplicate { .. } => panic!("expected fresh insert"),
    };
    // Re-inserting same fingerprint on feed1 -> silent Duplicate hit.
    match db
        .insert_entry_if_new(&new_entry)
        .await
        .expect("insert dup")
    {
        EntryInsertOutcome::Duplicate { existing_id } => assert_eq!(existing_id, entry1.id),
        EntryInsertOutcome::Inserted(_) => panic!("expected duplicate hit"),
    }
    // Same fingerprint on feed2 -> distinct entry allowed.
    let mut entry_on_feed2 = new_entry.clone();
    entry_on_feed2.feed_id = feed2.id;
    assert!(matches!(
        db.insert_entry_if_new(&entry_on_feed2)
            .await
            .expect("insert on feed2"),
        EntryInsertOutcome::Inserted(_)
    ));

    // 3. Create two Telegram channels sharing the SAME bot token (shared credential_group_key).
    let tg_cfg_a = ChannelConfig::Telegram(TelegramChannelConfig {
        bot_token: "123456:ABCDEF-shared-bot-token".to_string(),
        chat_id: "-1001111111".to_string(),
        message_thread_id: None,
        api_base_url: None,
    });
    let tg_cfg_b = ChannelConfig::Telegram(TelegramChannelConfig {
        bot_token: "123456:ABCDEF-shared-bot-token".to_string(),
        chat_id: "-1002222222".to_string(),
        message_thread_id: Some(42),
        api_base_url: None,
    });
    assert_eq!(
        tg_cfg_a.credential_group_key(),
        tg_cfg_b.credential_group_key()
    );

    let chan_a = db
        .create_channel(&NewChannel {
            name: "TG Main".to_string(),
            kind: ChannelKind::Telegram,
            encrypted_config: key.encrypt_json(&tg_cfg_a).expect("enc a"),
            credential_group_key: tg_cfg_a.credential_group_key(),
            capabilities_json: r#"{"supports_rich_message":true}"#.to_string(),
            min_interval_secs: 5,
            jitter_max_secs: 3,
            hourly_quota: Some(30),
            daily_quota: Some(200),
            use_proxy: false,
            enabled: true,
        })
        .await
        .expect("create chan_a");

    let chan_b = db
        .create_channel(&NewChannel {
            name: "TG Topic".to_string(),
            kind: ChannelKind::Telegram,
            encrypted_config: key.encrypt_json(&tg_cfg_b).expect("enc b"),
            credential_group_key: tg_cfg_b.credential_group_key(),
            capabilities_json: "{}".to_string(),
            min_interval_secs: 5,
            jitter_max_secs: 3,
            hourly_quota: None,
            daily_quota: None,
            use_proxy: false,
            enabled: true,
        })
        .await
        .expect("create chan_b");

    let route_a = db
        .create_route(&NewRoute {
            name: "Rust -> TG Main".to_string(),
            feed_ids: vec![feed1.id],
            match_tags: vec![],
            channel_id: chan_a.id,
            include_keywords: vec![],
            exclude_keywords: vec![],
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions {
                telegram_mode: TelegramDeliveryMode::MediaMode,
                link_preview: true,
            },
            enabled: true,
        })
        .await
        .expect("create route_a");

    let route_b = db
        .create_route(&NewRoute {
            name: "Rust -> TG Topic".to_string(),
            feed_ids: vec![],
            match_tags: vec!["rust".to_string()],
            channel_id: chan_b.id,
            include_keywords: vec![],
            exclude_keywords: vec![],
            include_regex: None,
            exclude_regex: None,
            options: RouteOptions::default(),
            enabled: true,
        })
        .await
        .expect("create route_b");

    // 4. `(route_id, entry_id)` unique outbox constraint.
    let t0 = 1_700_000_100;
    let task_a = db
        .enqueue_delivery_task(route_a.id, chan_a.id, entry1.id, t0)
        .await
        .expect("enqueue route_a")
        .expect("must insert first time");
    let dup_a = db
        .enqueue_delivery_task(route_a.id, chan_a.id, entry1.id, t0)
        .await
        .expect("enqueue duplicate route_a");
    assert!(
        dup_a.is_none(),
        "duplicate (route_id, entry_id) must be ignored idempotently"
    );

    // Enqueueing the same entry for a DIFFERENT route succeeds.
    let task_b = db
        .enqueue_delivery_task(route_b.id, chan_b.id, entry1.id, t0 + 50)
        .await
        .expect("enqueue route_b")
        .expect("must insert for route_b");

    // 5. Atomic lease claim & mid-flight checkpoint persistence & expired lease takeover!
    let claimed_1 = db
        .claim_next_delivery_task(t0, 60)
        .await
        .expect("claim 1")
        .expect("task_a is due at t0");
    assert_eq!(claimed_1.id, task_a.id);
    assert_eq!(claimed_1.status, DeliveryTaskStatus::Delivering);
    assert_eq!(claimed_1.lease_until, Some(t0 + 60));
    assert_eq!(claimed_1.attempt_count, 1);

    // While lease is active (`t0 + 30 < t0 + 60`) and `task_b` isn't due until `t0 + 50`, nothing can be claimed.
    assert!(
        db.claim_next_delivery_task(t0 + 30, 60)
            .await
            .expect("claim while leased")
            .is_none()
    );

    // Persist a mid-flight checkpoint via CheckpointSink (simulating batch 1 sent before worker SIGKILL).
    let sink = db.checkpoint_sink(task_a.id);
    let cp = DeliveryCheckpoint::Telegram {
        next_batch_offset: 1,
        sent_message_ids: vec![9001, 9002],
        degraded_to_compact: false,
    };
    sink.save_checkpoint(&cp)
        .await
        .expect("persist mid-flight checkpoint");

    // Advance clock to `t0 + 61`: `task_a`'s lease (`t0 + 60`) has expired AND `task_b` (`t0 + 50`) is due.
    // Expired `task_a` must be atomically reclaimed with its checkpoint preserved!
    let reclaimed = db
        .claim_next_delivery_task(t0 + 61, 60)
        .await
        .expect("reclaim expired")
        .expect("must reclaim expired lease");
    assert_eq!(reclaimed.id, task_a.id);
    assert_eq!(reclaimed.status, DeliveryTaskStatus::Delivering);
    assert_eq!(reclaimed.attempt_count, 2);
    assert_eq!(reclaimed.lease_until, Some(t0 + 121));
    assert_eq!(reclaimed.checkpoint().expect("parse cp"), Some(cp.clone()));

    // Complete `task_a` at `t0 + 65`.
    let delivered_a = db
        .mark_task_delivered(task_a.id, t0 + 65, Some(200), Some(&cp), Some("ok"))
        .await
        .expect("deliver task_a");
    assert_eq!(delivered_a.status, DeliveryTaskStatus::Delivered);
    assert!(delivered_a.lease_until.is_none());

    // 6. Shared credential group cooldown blocks `task_b` on `chan_b` until cooldown expires!
    let affected = db
        .set_credential_group_cooldown(&chan_a.credential_group_key, t0 + 200)
        .await
        .expect("cooldown group");
    assert_eq!(affected, 2, "both TG channels share the bot token cooldown");

    assert!(
        db.claim_next_delivery_task(t0 + 100, 60)
            .await
            .expect("blocked by cooldown")
            .is_none(),
        "task_b must not be claimed while shared bot token is in cooldown"
    );

    // Once past `t0 + 200`, `task_b` is claimed; test `deferred` -> `dead_letter` -> `requeue` -> `delivered`.
    let claimed_b = db
        .claim_next_delivery_task(t0 + 201, 60)
        .await
        .expect("claim task_b")
        .expect("cooldown expired");
    assert_eq!(claimed_b.id, task_b.id);
    assert_eq!(claimed_b.attempt_count, 1);

    // Deferring due to quota must restore `attempt_count` back to 0.
    let deferred_b = db
        .mark_task_deferred(task_b.id, t0 + 300, None, "hourly quota reached")
        .await
        .expect("defer task_b");
    assert_eq!(deferred_b.status, DeliveryTaskStatus::Deferred);
    assert_eq!(deferred_b.attempt_count, 0);

    let claimed_b2 = db
        .claim_next_delivery_task(t0 + 300, 60)
        .await
        .expect("claim after defer")
        .expect("due at t0 + 300");
    assert_eq!(claimed_b2.attempt_count, 1);

    db.mark_task_dead_letter(task_b.id, t0 + 305, Some(403), None, "bot kicked")
        .await
        .expect("dead letter");
    let requeued = db
        .requeue_all_dead_letter_tasks(t0 + 310)
        .await
        .expect("requeue dead letters");
    assert_eq!(requeued, 1);

    let claimed_b3 = db
        .claim_next_delivery_task(t0 + 310, 60)
        .await
        .expect("claim requeued")
        .expect("requeued task");
    assert_eq!(claimed_b3.id, task_b.id);
    assert_eq!(claimed_b3.attempt_count, 1);

    db.mark_task_delivered(task_b.id, t0 + 315, Some(200), None, Some("sent"))
        .await
        .expect("deliver task_b");

    // 7. Rolling hourly & daily quota usage reflects both individual channel and shared credential group!
    let usage_a = db
        .get_channel_quota_usage(chan_a.id, &chan_a.credential_group_key, t0 + 400)
        .await
        .expect("quota usage");
    assert_eq!(usage_a.channel_last_hour, 1);
    assert_eq!(usage_a.channel_last_day, 1);
    assert_eq!(usage_a.credential_group_last_hour, 2);
    assert_eq!(usage_a.credential_group_last_day, 2);
    assert_eq!(usage_a.last_completed_at, Some(t0 + 315));

    db.close().await;
}

#[tokio::test]
async fn not_exists_payload_gc_covers_delivered_unrouted_deleted_routes_and_protects_retries() {
    let tmp = TempDir::new().expect("tempdir");
    let key = MasterKey::generate();
    let db = Database::connect(&tmp.path().join("feedcourier.db"), 4)
        .await
        .expect("connect db");

    let feed = db
        .create_feed(&NewFeed::new("https://example.com/rss.xml"))
        .await
        .expect("feed");

    let smtp_cfg = ChannelConfig::Smtp(SmtpChannelConfig {
        host: "smtp.example.com".to_string(),
        port: 587,
        username: Some("mailer@example.com".to_string()),
        password: Some("pw".to_string()),
        security: SmtpSecurity::StartTls,
        from_address: "mailer@example.com".to_string(),
        to_addresses: vec!["reader@example.com".to_string()],
        reply_to: None,
    });
    let channel = db
        .create_channel(&NewChannel {
            name: "Email".to_string(),
            kind: ChannelKind::Smtp,
            encrypted_config: key.encrypt_json(&smtp_cfg).expect("enc"),
            credential_group_key: smtp_cfg.credential_group_key(),
            capabilities_json: "{}".to_string(),
            min_interval_secs: 15,
            jitter_max_secs: 5,
            hourly_quota: None,
            daily_quota: None,
            use_proxy: false,
            enabled: true,
        })
        .await
        .expect("channel");

    let keep_route = db
        .create_route(&NewRoute {
            name: "Primary Route".to_string(),
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
        .expect("keep_route");

    let ephemeral_route = db
        .create_route(&NewRoute {
            name: "Ephemeral Route".to_string(),
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
        .expect("ephemeral_route");

    let retention_secs = 24 * 3600;
    let old_ts = 1_700_000_000;
    let now = old_ts + retention_secs + 600; // 24h10m later

    let make_entry = |slug: &str, discovered_at: i64| NewEntry {
        feed_id: feed.id,
        fingerprint_sha256: sample_fingerprint(slug),
        published_at: Some(discovered_at),
        discovered_at,
        title: Some(format!("Title {slug}")),
        canonical_url: Some(format!("https://example.com/{slug}")),
        author: Some("Author".to_string()),
        content_html: Some(format!("<article>Body for {slug}</article>")),
        first_sentence: Some(format!("Body for {slug}")),
        summary_text: Some(format!("Summary {slug}")),
        media: vec![MediaAttachment {
            url: format!("https://example.com/{slug}.jpg"),
            mime_type: Some("image/jpeg".to_string()),
            kind: MediaAttachmentKind::Image,
            length_bytes: None,
            caption: None,
        }],
    };

    let db_ref = &db;
    let insert = |ne: NewEntry| async move {
        match db_ref.insert_entry_if_new(&ne).await.expect("insert entry") {
            EntryInsertOutcome::Inserted(e) => *e,
            EntryInsertOutcome::Duplicate { .. } => panic!("unexpected dup"),
        }
    };

    // Case 1: Old entry with a `delivered` task -> MUST BE PURGED.
    let e_delivered = insert(make_entry("delivered", old_ts)).await;
    let t_delivered = db
        .enqueue_delivery_task(keep_route.id, channel.id, e_delivered.id, old_ts)
        .await
        .unwrap()
        .unwrap();
    db.mark_task_delivered(t_delivered.id, old_ts + 30, Some(250), None, None)
        .await
        .unwrap();

    // Case 2: Old entry that matched ZERO routes (unrouted) -> MUST BE PURGED.
    let e_unrouted = insert(make_entry("unrouted", old_ts)).await;

    // Case 3: Old entry queued on `ephemeral_route`, then `ephemeral_route` is deleted -> MUST BE PURGED.
    let e_deleted_route = insert(make_entry("deleted-route", old_ts)).await;
    db.enqueue_delivery_task(
        ephemeral_route.id,
        channel.id,
        e_deleted_route.id,
        old_ts + 60,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(db.delete_route(ephemeral_route.id).await.unwrap());

    // Case 4: Old entry with an active `retrying` task -> MUST BE PROTECTED (NOT purged).
    let e_retrying = insert(make_entry("active-retry", old_ts)).await;
    let t_retrying = db
        .enqueue_delivery_task(keep_route.id, channel.id, e_retrying.id, old_ts)
        .await
        .unwrap()
        .unwrap();
    db.mark_task_retrying(
        t_retrying.id,
        now + 300,
        Some(421),
        None,
        "421 temp fail, retry later",
    )
    .await
    .unwrap();

    // Case 5: Fresh entry (`now - 600`, inside 24h retention window) -> MUST BE PROTECTED.
    let e_recent = insert(make_entry("recent-unrouted", now - 600)).await;

    // Run GC! Exactly Cases 1, 2, and 3 (3 entries) must be purged.
    let purged_count = db
        .purge_expired_entry_payloads(now, retention_secs)
        .await
        .expect("run GC");
    assert_eq!(purged_count, 3);

    for purged_id in [e_delivered.id, e_unrouted.id, e_deleted_route.id] {
        let row = db.get_entry(purged_id).await.unwrap().unwrap();
        assert_eq!(row.payload_purged_at, Some(now));
        assert!(row.title.is_none());
        assert!(row.canonical_url.is_none());
        assert!(row.content_html.is_none());
        assert!(row.media_json.is_none());
        assert_eq!(
            row.fingerprint_sha256.len(),
            64,
            "fingerprint stays forever"
        );
    }

    // Re-polling a purged entry must STILL hit the permanent fingerprint and never re-enqueue!
    assert!(matches!(
        db.insert_entry_if_new(&make_entry("delivered", now))
            .await
            .unwrap(),
        EntryInsertOutcome::Duplicate { existing_id } if existing_id == e_delivered.id
    ));

    // Active retrying & recent entries still retain full HTML and media payloads.
    for kept_id in [e_retrying.id, e_recent.id] {
        let row = db.get_entry(kept_id).await.unwrap().unwrap();
        assert!(row.payload_purged_at.is_none());
        assert!(row.content_html.is_some());
        assert!(!row.media().is_empty());
    }

    // Once `t_retrying` succeeds, a subsequent GC purges `e_retrying` cleanly!
    db.mark_task_delivered(t_retrying.id, now + 10, Some(250), None, None)
        .await
        .unwrap();
    let purged_after_retry = db
        .purge_expired_entry_payloads(now + 20, retention_secs)
        .await
        .unwrap();
    assert_eq!(purged_after_retry, 1);
    assert!(
        db.get_entry(e_retrying.id)
            .await
            .unwrap()
            .unwrap()
            .is_purged()
    );

    db.close().await;
}

#[tokio::test]
async fn vacuum_into_online_snapshot_backup_and_auth_settings_roundtrip() {
    let tmp = TempDir::new().expect("tempdir");
    let live_path = tmp.path().join("live").join("feedcourier.db");
    let backup_path = tmp.path().join("backups").join("snapshot-01.db");

    let db = Database::connect(&live_path, 2)
        .await
        .expect("connect live");

    // Populate feeds, settings, MCP tokens, and admin sessions.
    let mut nf = NewFeed::new("https://hnrss.org/frontpage");
    nf.title = "Hacker News".to_string();
    let feed = db.create_feed(&nf).await.expect("create feed");
    db.record_feed_poll_failure(
        feed.id,
        3,
        FeedHealthStatus::Backoff,
        Some(1_700_005_000),
        "HTTP 503",
    )
    .await
    .expect("poll failure");

    let allowlist = vec!["10.0.0.0/8".to_string(), "rsshub.internal".to_string()];
    db.set_setting_typed(SETTING_SSRF_ALLOWLIST, &allowlist)
        .await
        .expect("set allowlist");

    let (_raw_mcp, mcp_hash, mcp_prefix) = generate_mcp_token("readonly");
    let created_tok = db
        .create_mcp_token(
            "Claude Desktop",
            &mcp_hash,
            &mcp_prefix,
            McpTokenScope::Readonly,
        )
        .await
        .expect("create mcp token");

    let (_raw_sess, sess_hash) = generate_session_token();
    db.create_session(&sess_hash, 1_900_000_000)
        .await
        .expect("create session");

    db.run_pragma_optimize().await.expect("PRAGMA optimize");

    // Refuse backing up over the live database file itself.
    assert!(db.vacuum_into(&live_path).await.is_err());

    // Execute online hot snapshot while `db` pools remain open and active!
    db.vacuum_into(&backup_path)
        .await
        .expect("VACUUM INTO snapshot");
    assert!(backup_path.exists());

    // Open the snapshot independently and verify data fidelity.
    let snap_db = Database::connect(&backup_path, 1)
        .await
        .expect("open snapshot db");

    let snap_feed = snap_db
        .get_feed(feed.id)
        .await
        .expect("get feed")
        .expect("feed exists in backup");
    assert_eq!(snap_feed.title, "Hacker News");
    assert_eq!(snap_feed.health_status, FeedHealthStatus::Backoff);
    assert_eq!(snap_feed.consecutive_failures, 3);

    let snap_allowlist: Vec<String> = snap_db
        .get_setting_typed(SETTING_SSRF_ALLOWLIST)
        .await
        .expect("read setting")
        .expect("allowlist exists");
    assert_eq!(snap_allowlist, allowlist);

    let snap_tok = snap_db
        .get_mcp_token_by_hash(&mcp_hash)
        .await
        .expect("get mcp token")
        .expect("token exists");
    assert_eq!(snap_tok.id, created_tok.id);
    assert_eq!(snap_tok.scope, McpTokenScope::Readonly);

    assert!(
        snap_db
            .get_valid_session(&sess_hash, 1_800_000_000)
            .await
            .expect("valid session")
            .is_some()
    );
    assert!(
        snap_db
            .get_valid_session(&sess_hash, 1_950_000_000)
            .await
            .expect("expired session")
            .is_none()
    );

    let overview = snap_db.get_overview_snapshot().await.expect("overview");
    assert_eq!(overview.feeds_backoff, 1);

    snap_db.close().await;
    db.close().await;
}
