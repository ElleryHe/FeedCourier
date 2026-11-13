//! Background feed polling loop, lease-based outbox delivery worker (pacing + jitter + quotas),
//! and periodic payload GC / `PRAGMA optimize` maintenance loop (`feedcourier.md` Sections 三.3 & 四.3).

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{self, StreamExt};
use reqwest::header::{ACCEPT, HeaderValue, REFERER};
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use url::Url;

use crate::channels::notion::NotionSender;
use crate::channels::smtp::SmtpSender;
use crate::channels::telegram::{FetchedMedia, MediaStreamFetcher, TelegramSender};
use crate::channels::{
    ChannelConfig, ChannelKind, ChannelSender, DeliveryCheckpoint, DeliveryContext, DeliveryOutcome,
};
use crate::config::AppConfig;
use crate::crypto::MasterKey;
use crate::db::Database;
use crate::db::models::{
    Channel, DeliveryTask, SETTING_OUTBOUND_PROXY_URL, SETTING_SSRF_ALLOWLIST, now_unix,
};
use crate::error::{AppError, AppResult};
use crate::ingest::IngestEngine;
use crate::net::{NetSettings, OutboundRequest, ProtectedHttpClient};

/// Default lease duration in seconds granted when a worker claims a `delivery_tasks` row.
pub const DEFAULT_TASK_LEASE_SECS: i64 = 120;

/// Default maximum delivery attempts before moving a task to `dead_letter`.
pub const DEFAULT_MAX_DELIVERY_ATTEMPTS: i64 = 6;

/// Default maximum concurrent feed polls per poller tick.
pub const DEFAULT_FEED_POLL_CONCURRENCY: usize = 4;

/// Default maximum due feeds claimed per poller tick.
pub const DEFAULT_DUE_FEED_BATCH_LIMIT: i64 = 64;

/// Default maximum outbox tasks drained per worker wakeup cycle.
pub const DEFAULT_OUTBOX_DRAIN_LIMIT: usize = 64;

/// Maximum task age (`20 minutes`) during which Notion `202 Accepted` async import polls
/// are deferred without consuming the 6-attempt transient error retry budget.
pub const DEFAULT_MAX_ASYNC_POLL_AGE_SECS: i64 = 20 * 60;

/// Default cadence for checking due feeds when idle (`15s`).
pub const DEFAULT_POLLER_TICK: Duration = Duration::from_secs(15);

/// Default cadence for checking due/expired-lease outbox tasks when idle (`2s`).
pub const DEFAULT_OUTBOX_TICK: Duration = Duration::from_secs(2);

/// Default cadence for running `NOT EXISTS` payload GC, session cleanup, and `PRAGMA optimize` (`1h`).
pub const DEFAULT_MAINTENANCE_TICK: Duration = Duration::from_secs(3600);

const MEDIA_ACCEPT_HEADER: &str = "image/*,video/*,audio/*,*/*;q=0.8";

/// SSRF-protected [`MediaStreamFetcher`] backed by the live runtime [`ProtectedHttpClient`].
///
/// Ensures Telegram `media_mode` anti-hotlink fallback uploads automatically honor hot-reloaded
/// `ssrf_allowlist` entries and `outbound_proxy_url` settings without recreating the fetcher.
#[derive(Debug, Clone)]
pub struct ProtectedMediaFetcher {
    http: ProtectedHttpClient,
}

impl ProtectedMediaFetcher {
    /// Wrap a shared [`ProtectedHttpClient`].
    pub fn new(http: ProtectedHttpClient) -> Self {
        Self { http }
    }

    /// Underlying [`ProtectedHttpClient`].
    pub fn http(&self) -> &ProtectedHttpClient {
        &self.http
    }
}

#[async_trait]
impl MediaStreamFetcher for ProtectedMediaFetcher {
    async fn fetch_media(
        &self,
        url: &str,
        max_bytes: usize,
        use_proxy: bool,
    ) -> AppResult<FetchedMedia> {
        let trimmed = url.trim();
        let parsed = Url::parse(trimmed)?;

        let mut req = OutboundRequest::new(trimmed)
            .max_bytes(max_bytes)
            .use_proxy(use_proxy)
            .header(ACCEPT, HeaderValue::from_static(MEDIA_ACCEPT_HEADER));

        if let Some(host) = parsed.host_str() {
            let origin = match parsed.port() {
                Some(port) => format!("{}://{host}:{port}/", parsed.scheme()),
                None => format!("{}://{host}/", parsed.scheme()),
            };
            if let Ok(referer) = HeaderValue::from_str(&origin) {
                req = req.header(REFERER, referer);
            }
        }

        let resp = self.http.get_bytes(req).await?;
        if !resp.status.is_success() {
            return Err(AppError::Upstream(format!(
                "media origin '{}' returned HTTP {}",
                resp.final_url, resp.status
            )));
        }
        if resp.bytes.is_empty() {
            return Err(AppError::Upstream(format!(
                "media origin '{}' returned an empty body",
                resp.final_url
            )));
        }

        let normalized_ct = resp
            .content_type
            .as_deref()
            .and_then(|ct| ct.split(';').next())
            .map(|ct| ct.trim().to_ascii_lowercase())
            .filter(|ct| !ct.is_empty());

        let filename = derive_media_filename(&resp.final_url, normalized_ct.as_deref());

        Ok(FetchedMedia {
            bytes: resp.bytes,
            content_type: normalized_ct,
            filename,
        })
    }
}

/// Resolve persisted network settings from SQLite `system_settings`, falling back to `AppConfig`
/// defaults when a setting key has not yet been written via `PUT /api/v1/settings`.
pub async fn load_persisted_net_settings(
    config: &AppConfig,
    db: &Database,
) -> AppResult<NetSettings> {
    let outbound_proxy_url = match db
        .get_setting_typed::<Option<String>>(SETTING_OUTBOUND_PROXY_URL)
        .await?
    {
        Some(stored) => stored
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        None => config
            .outbound_proxy
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned),
    };

    let raw_allowlist = match db
        .get_setting_typed::<Vec<String>>(SETTING_SSRF_ALLOWLIST)
        .await?
    {
        Some(stored) => stored,
        None => config.ssrf_allowlist.clone(),
    };
    let ssrf_allowlist = normalize_allowlist(raw_allowlist);

    Ok(NetSettings {
        outbound_proxy_url,
        ssrf_allowlist,
    })
}

/// Normalize a `NetSettings` payload (trimming whitespace and deduplicating allowlist entries).
pub fn normalize_net_settings(settings: NetSettings) -> NetSettings {
    let outbound_proxy_url = settings
        .outbound_proxy_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    let ssrf_allowlist = normalize_allowlist(settings.ssrf_allowlist);
    NetSettings {
        outbound_proxy_url,
        ssrf_allowlist,
    }
}

fn normalize_allowlist(entries: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(entries.len());
    for item in entries {
        let trimmed = item.trim();
        if !trimmed.is_empty() && !out.iter().any(|existing| existing == trimmed) {
            out.push(trimmed.to_string());
        }
    }
    out
}

struct DispatcherInner {
    config: AppConfig,
    db: Database,
    master_key: MasterKey,
    http: ProtectedHttpClient,
    ingest: IngestEngine,
    smtp_sender: SmtpSender,
    telegram_sender: RwLock<TelegramSender>,
    notion_sender: Arc<NotionSender>,
    media_fetcher: Arc<ProtectedMediaFetcher>,
    /// In-memory per-`credential_group_key` next allowed send timestamp (`min_interval_secs + jitter`).
    pacing_next_allowed_at: Mutex<HashMap<String, i64>>,
    poller_wakeup: Arc<Notify>,
    outbox_wakeup: Arc<Notify>,
    maintenance_wakeup: Arc<Notify>,
    task_lease_secs: i64,
    max_delivery_attempts: i64,
    poll_concurrency: usize,
    poller_tick: Duration,
    outbox_tick: Duration,
    maintenance_tick: Duration,
}

impl Clone for DispatcherInner {
    fn clone(&self) -> Self {
        let tg = self
            .telegram_sender
            .read()
            .expect("TelegramSender RwLock poisoned")
            .clone();
        let pacing = self
            .pacing_next_allowed_at
            .lock()
            .expect("pacing_next_allowed_at mutex poisoned")
            .clone();

        Self {
            config: self.config.clone(),
            db: self.db.clone(),
            master_key: self.master_key.clone(),
            http: self.http.clone(),
            ingest: self.ingest.clone(),
            smtp_sender: self.smtp_sender.clone(),
            telegram_sender: RwLock::new(tg),
            notion_sender: Arc::clone(&self.notion_sender),
            media_fetcher: Arc::clone(&self.media_fetcher),
            pacing_next_allowed_at: Mutex::new(pacing),
            poller_wakeup: Arc::clone(&self.poller_wakeup),
            outbox_wakeup: Arc::clone(&self.outbox_wakeup),
            maintenance_wakeup: Arc::clone(&self.maintenance_wakeup),
            task_lease_secs: self.task_lease_secs,
            max_delivery_attempts: self.max_delivery_attempts,
            poll_concurrency: self.poll_concurrency,
            poller_tick: self.poller_tick,
            outbox_tick: self.outbox_tick,
            maintenance_tick: self.maintenance_tick,
        }
    }
}

/// Shared application runtime and background scheduler/dispatcher (`Clone` via `Arc`).
///
/// Coordinates:
/// - Bounded concurrent feed polling (`poll_due_feeds_once`)
/// - Lease-based effective-once outbox task dispatching (`process_next_outbox_task`)
/// - Per-channel hourly/daily quotas and shared `credential_group_key` pacing + random jitter
/// - Hot-reloading outbound proxy & SSRF allowlists across HTTP, Telegram, and Notion adapters
/// - Periodic `NOT EXISTS` payload GC, expired session cleanup, and `PRAGMA optimize`
#[derive(Clone)]
pub struct DeliveryDispatcher {
    inner: Arc<DispatcherInner>,
}

/// Alias for [`DeliveryDispatcher`] when referenced as the shared application runtime.
pub type AppRuntime = DeliveryDispatcher;

impl fmt::Debug for DeliveryDispatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeliveryDispatcher")
            .field("data_dir", &self.inner.config.data_dir)
            .field("task_lease_secs", &self.inner.task_lease_secs)
            .field("max_delivery_attempts", &self.inner.max_delivery_attempts)
            .field("poll_concurrency", &self.inner.poll_concurrency)
            .finish()
    }
}

impl DeliveryDispatcher {
    /// Construct a [`DeliveryDispatcher`] from pre-built components.
    pub fn new(
        config: AppConfig,
        db: Database,
        master_key: MasterKey,
        http: ProtectedHttpClient,
    ) -> AppResult<Self> {
        let net_settings = http.settings();
        let media_fetcher = Arc::new(ProtectedMediaFetcher::new(http.clone()));
        let ingest = IngestEngine::new(db.clone(), http.clone());
        let smtp_sender = SmtpSender::new();
        let telegram_sender = TelegramSender::with_proxy_and_fetcher(
            net_settings.outbound_proxy_url.clone(),
            Arc::clone(&media_fetcher) as Arc<dyn MediaStreamFetcher>,
        );
        let notion_sender = Arc::new(NotionSender::new(net_settings.outbound_proxy_url)?);

        Ok(Self {
            inner: Arc::new(DispatcherInner {
                config,
                db,
                master_key,
                http,
                ingest,
                smtp_sender,
                telegram_sender: RwLock::new(telegram_sender),
                notion_sender,
                media_fetcher,
                pacing_next_allowed_at: Mutex::new(HashMap::new()),
                poller_wakeup: Arc::new(Notify::new()),
                outbox_wakeup: Arc::new(Notify::new()),
                maintenance_wakeup: Arc::new(Notify::new()),
                task_lease_secs: DEFAULT_TASK_LEASE_SECS,
                max_delivery_attempts: DEFAULT_MAX_DELIVERY_ATTEMPTS,
                poll_concurrency: DEFAULT_FEED_POLL_CONCURRENCY,
                poller_tick: DEFAULT_POLLER_TICK,
                outbox_tick: DEFAULT_OUTBOX_TICK,
                maintenance_tick: DEFAULT_MAINTENANCE_TICK,
            }),
        })
    }

    /// Construct a [`DeliveryDispatcher`] resolving initial [`NetSettings`] from SQLite
    /// `system_settings` (with fallback to `AppConfig` environment defaults).
    pub async fn from_db_settings(
        config: AppConfig,
        db: Database,
        master_key: MasterKey,
    ) -> AppResult<Self> {
        let net_settings = load_persisted_net_settings(&config, &db).await?;
        let http = ProtectedHttpClient::new(net_settings)?;
        Self::new(config, db, master_key, http)
    }

    /// Replace the `NotionSender` instance (e.g. pointing at a `wiremock` base URL in tests).
    pub fn with_notion_sender(mut self, notion_sender: NotionSender) -> Self {
        let inner = Arc::make_mut(&mut self.inner);
        inner.notion_sender = Arc::new(notion_sender);
        self
    }

    /// Replace the `SmtpSender` instance.
    pub fn with_smtp_sender(mut self, smtp_sender: SmtpSender) -> Self {
        let inner = Arc::make_mut(&mut self.inner);
        inner.smtp_sender = smtp_sender;
        self
    }

    /// Replace the `TelegramSender` instance.
    pub fn with_telegram_sender(mut self, telegram_sender: TelegramSender) -> Self {
        let inner = Arc::make_mut(&mut self.inner);
        inner.telegram_sender = RwLock::new(telegram_sender);
        self
    }

    /// Replace the `IngestEngine` instance.
    pub fn with_ingest_engine(mut self, ingest: IngestEngine) -> Self {
        let inner = Arc::make_mut(&mut self.inner);
        inner.ingest = ingest;
        self
    }

    /// Override lease duration granted on `claim_next_delivery_task`.
    pub fn with_task_lease_secs(mut self, lease_secs: i64) -> Self {
        let inner = Arc::make_mut(&mut self.inner);
        inner.task_lease_secs = lease_secs.max(1);
        self
    }

    /// Override maximum transient delivery attempts before `dead_letter`.
    pub fn with_max_delivery_attempts(mut self, max_attempts: i64) -> Self {
        let inner = Arc::make_mut(&mut self.inner);
        inner.max_delivery_attempts = max_attempts.max(1);
        self
    }

    /// Override maximum concurrent feed polls per poller tick.
    pub fn with_poll_concurrency(mut self, concurrency: usize) -> Self {
        let inner = Arc::make_mut(&mut self.inner);
        inner.poll_concurrency = concurrency.max(1);
        self
    }

    /// Override background loop idle tick intervals.
    pub fn with_loop_intervals(
        mut self,
        poller_tick: Duration,
        outbox_tick: Duration,
        maintenance_tick: Duration,
    ) -> Self {
        let inner = Arc::make_mut(&mut self.inner);
        inner.poller_tick = poller_tick;
        inner.outbox_tick = outbox_tick;
        inner.maintenance_tick = maintenance_tick;
        self
    }

    pub fn config(&self) -> &AppConfig {
        &self.inner.config
    }

    pub fn db(&self) -> &Database {
        &self.inner.db
    }

    pub fn master_key(&self) -> &MasterKey {
        &self.inner.master_key
    }

    pub fn http(&self) -> &ProtectedHttpClient {
        &self.inner.http
    }

    pub fn ingest(&self) -> &IngestEngine {
        &self.inner.ingest
    }

    pub fn smtp_sender(&self) -> &SmtpSender {
        &self.inner.smtp_sender
    }

    /// Snapshot the current proxy-aware [`TelegramSender`].
    pub fn telegram_sender(&self) -> TelegramSender {
        self.inner
            .telegram_sender
            .read()
            .expect("TelegramSender RwLock poisoned")
            .clone()
    }

    pub fn notion_sender(&self) -> &NotionSender {
        &self.inner.notion_sender
    }

    pub fn media_fetcher(&self) -> Arc<ProtectedMediaFetcher> {
        Arc::clone(&self.inner.media_fetcher)
    }

    /// Shared `Notify` handle waking the feed poller loop (for cross-subsystem wiring).
    pub fn poller_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.inner.poller_wakeup)
    }

    /// Shared `Notify` handle waking the outbox dispatcher loop (e.g. for `McpState::with_outbox_notify`).
    pub fn outbox_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.inner.outbox_wakeup)
    }

    /// Wake the background feed poller loop immediately.
    pub fn notify_poller(&self) {
        self.inner.poller_wakeup.notify_one();
    }

    /// Wake the background outbox dispatcher loop immediately.
    pub fn notify_outbox(&self) {
        self.inner.outbox_wakeup.notify_one();
    }

    /// Wake the background maintenance/GC loop immediately.
    pub fn notify_maintenance(&self) {
        self.inner.maintenance_wakeup.notify_one();
    }

    /// Hot-reload `outbound_proxy_url` and `ssrf_allowlist` atomically across
    /// [`ProtectedHttpClient`], [`NotionSender`], and [`TelegramSender`] (plus its shared
    /// [`ProtectedMediaFetcher`]).
    pub fn reload_network_settings(&self, settings: NetSettings) -> AppResult<()> {
        let normalized = normalize_net_settings(settings);

        // Validate & apply on ProtectedHttpClient first (rejects bad CIDRs or proxy schemes).
        self.inner.http.update_settings(normalized.clone())?;

        // Update NotionSender's outbound proxy client.
        self.inner
            .notion_sender
            .set_outbound_proxy(normalized.outbound_proxy_url.clone())?;

        // Swap a fresh proxy-configured TelegramSender sharing `ProtectedMediaFetcher`.
        let next_telegram = TelegramSender::with_proxy_and_fetcher(
            normalized.outbound_proxy_url,
            Arc::clone(&self.inner.media_fetcher) as Arc<dyn MediaStreamFetcher>,
        );
        {
            let mut guard = self
                .inner
                .telegram_sender
                .write()
                .expect("TelegramSender RwLock poisoned");
            *guard = next_telegram;
        }

        Ok(())
    }

    /// Poll up to `limit` feeds whose `next_poll_at <= now` with bounded concurrency,
    /// waking the outbox worker if any new delivery tasks are enqueued.
    ///
    /// Returns the number of due feeds processed in this tick.
    pub async fn poll_due_feeds_once(&self, limit: i64) -> AppResult<usize> {
        if limit <= 0 {
            return Ok(0);
        }

        let due_feeds = self.inner.db.list_due_feeds(now_unix(), limit).await?;
        if due_feeds.is_empty() {
            return Ok(0);
        }

        let total_due = due_feeds.len();
        let concurrency = self.inner.poll_concurrency.max(1);
        let semaphore = Arc::new(Semaphore::new(concurrency));
        let ingest = self.inner.ingest.clone();

        let results: Vec<usize> = stream::iter(due_feeds)
            .map(|feed| {
                let sem = Arc::clone(&semaphore);
                let ingest = ingest.clone();
                async move {
                    let _permit = sem
                        .acquire_owned()
                        .await
                        .expect("feed poll semaphore is never closed");
                    match ingest.poll_feed(feed.id, false, false).await {
                        Ok(outcome) => outcome.enqueued_tasks,
                        Err(err) => {
                            warn!(
                                feed_id = feed.id,
                                url = %feed.url,
                                error = %err,
                                "scheduled feed poll failed"
                            );
                            0
                        }
                    }
                }
            })
            .buffer_unordered(concurrency)
            .collect()
            .await;

        let total_enqueued: usize = results.into_iter().sum();
        if total_enqueued > 0 {
            self.notify_outbox();
        }

        Ok(total_due)
    }

    /// Claim and process at most one due (or expired-lease `delivering`) outbox task.
    ///
    /// Returns `Ok(true)` if a task was claimed and transitioned (`delivered`, `deferred`,
    /// `retrying`, or `dead_letter`), or `Ok(false)` when no eligible task is currently due.
    pub async fn process_next_outbox_task(&self) -> AppResult<bool> {
        let now = now_unix();
        let Some(task) = self
            .inner
            .db
            .claim_next_delivery_task(now, self.inner.task_lease_secs)
            .await?
        else {
            return Ok(false);
        };

        let existing_checkpoint = match task.checkpoint() {
            Ok(cp) => cp,
            Err(err) => {
                warn!(
                    task_id = task.id,
                    error = %err,
                    "ignoring malformed checkpoint_json on claimed delivery task"
                );
                None
            }
        };

        // 1. Load entry, feed, route, and channel; guard against purged payloads or disabled targets.
        let Some(entry) = self.inner.db.get_entry(task.entry_id).await? else {
            self.inner
                .db
                .mark_task_dead_letter(
                    task.id,
                    now,
                    None,
                    existing_checkpoint.as_ref(),
                    "Entry payload already garbage-collected before delivery",
                )
                .await?;
            return Ok(true);
        };

        if entry.is_purged() {
            self.inner
                .db
                .mark_task_dead_letter(
                    task.id,
                    now,
                    None,
                    existing_checkpoint.as_ref(),
                    "Entry payload already garbage-collected before delivery",
                )
                .await?;
            return Ok(true);
        }

        let Some(feed) = self.inner.db.get_feed(entry.feed_id).await? else {
            self.inner
                .db
                .mark_task_dead_letter(
                    task.id,
                    now,
                    None,
                    existing_checkpoint.as_ref(),
                    "Parent feed no longer exists",
                )
                .await?;
            return Ok(true);
        };

        let Some(route) = self.inner.db.get_route(task.route_id).await? else {
            self.inner
                .db
                .mark_task_dead_letter(
                    task.id,
                    now,
                    None,
                    existing_checkpoint.as_ref(),
                    "Target route no longer exists",
                )
                .await?;
            return Ok(true);
        };

        let Some(channel) = self.inner.db.get_channel(task.channel_id).await? else {
            self.inner
                .db
                .mark_task_dead_letter(
                    task.id,
                    now,
                    None,
                    existing_checkpoint.as_ref(),
                    "Target channel no longer exists",
                )
                .await?;
            return Ok(true);
        };

        if !route.enabled || !channel.enabled {
            self.inner
                .db
                .mark_task_deferred(
                    task.id,
                    now.saturating_add(300),
                    existing_checkpoint.as_ref(),
                    "Route or channel currently disabled",
                )
                .await?;
            return Ok(true);
        }

        // 2. Quota & anti-spam pacing pre-check (`mark_task_deferred` restores `attempt_count`).
        let usage = self
            .inner
            .db
            .get_channel_quota_usage(channel.id, &channel.credential_group_key, now)
            .await?;

        if let Some(hourly_quota) = channel.hourly_quota
            && (hourly_quota <= 0 || usage.channel_last_hour >= hourly_quota)
        {
            self.inner
                .db
                .mark_task_deferred(
                    task.id,
                    now.saturating_add(300),
                    existing_checkpoint.as_ref(),
                    &format!(
                        "Channel hourly quota reached ({}/{hourly_quota}); deferred to next window",
                        usage.channel_last_hour
                    ),
                )
                .await?;
            return Ok(true);
        }

        if let Some(daily_quota) = channel.daily_quota
            && (daily_quota <= 0 || usage.channel_last_day >= daily_quota)
        {
            self.inner
                .db
                .mark_task_deferred(
                    task.id,
                    now.saturating_add(900),
                    existing_checkpoint.as_ref(),
                    &format!(
                        "Channel daily quota reached ({}/{daily_quota}); deferred to next window",
                        usage.channel_last_day
                    ),
                )
                .await?;
            return Ok(true);
        }

        if let Some(earliest_allowed) =
            self.earliest_pacing_allowed_at(&channel, usage.last_completed_at)
            && now < earliest_allowed
        {
            self.inner
                .db
                .mark_task_deferred(
                    task.id,
                    earliest_allowed,
                    existing_checkpoint.as_ref(),
                    "Channel anti-spam pacing window active",
                )
                .await?;
            return Ok(true);
        }

        // 3. Decrypt channel configuration & execute downstream delivery.
        let channel_config: ChannelConfig = match self
            .inner
            .master_key
            .decrypt_json(&channel.encrypted_config)
        {
            Ok(cfg) => cfg,
            Err(err) => {
                self.inner
                    .db
                    .mark_task_dead_letter(
                        task.id,
                        now,
                        None,
                        existing_checkpoint.as_ref(),
                        &format!("Failed to decrypt channel credentials: {err}"),
                    )
                    .await?;
                return Ok(true);
            }
        };

        let checkpoint_sink = self.inner.db.checkpoint_sink(task.id);
        let ctx = DeliveryContext {
            task: &task,
            entry: &entry,
            feed: &feed,
            route: &route,
            route_options: route.options(),
            channel: &channel,
            channel_config: &channel_config,
            checkpoint: existing_checkpoint.clone(),
            checkpoint_sink: Some(&checkpoint_sink),
        };

        let send_result = match channel.kind {
            ChannelKind::Smtp => self.inner.smtp_sender.send(&ctx).await,
            ChannelKind::Telegram => {
                let sender = self.telegram_sender();
                sender.send(&ctx).await
            }
            ChannelKind::Notion => self.inner.notion_sender.send(&ctx).await,
        };

        let completed_now = now_unix();
        match send_result {
            Ok(outcome) => {
                self.apply_delivery_outcome(&task, &channel, completed_now, outcome)
                    .await?;
            }
            Err(err) => {
                self.apply_unexpected_send_error(&task, &channel, completed_now, err)
                    .await?;
            }
        }

        Ok(true)
    }

    /// Drain up to `max_tasks` currently due outbox tasks sequentially.
    pub async fn drain_due_outbox_tasks(&self, max_tasks: usize) -> AppResult<usize> {
        let mut processed = 0usize;
        while processed < max_tasks {
            if !self.process_next_outbox_task().await? {
                break;
            }
            processed += 1;
        }
        Ok(processed)
    }

    /// Execute one maintenance cycle:
    /// 1. `NOT EXISTS` payload garbage collection for delivered/unrouted entries older than retention
    /// 2. Expired WebUI session cleanup
    /// 3. `PRAGMA optimize` on the writer connection
    ///
    /// Returns the number of entry payloads purged.
    pub async fn run_maintenance_once(&self) -> AppResult<u64> {
        let now = now_unix();
        let purged_entries = self
            .inner
            .db
            .purge_expired_entry_payloads(now, self.inner.config.retention_secs())
            .await?;
        let purged_sessions = self.inner.db.purge_expired_sessions(now).await?;
        self.inner.db.run_pragma_optimize().await?;

        {
            let mut guard = self
                .inner
                .pacing_next_allowed_at
                .lock()
                .expect("pacing_next_allowed_at mutex poisoned");
            guard.retain(|_, ts| *ts > now);
        }

        debug!(
            purged_entries,
            purged_sessions, "completed scheduled database maintenance cycle"
        );
        Ok(purged_entries)
    }

    /// Spawn the 3 background loops (`poller`, `outbox`, `maintenance`) controlled by a shared
    /// [`CancellationToken`] and immediate wakeup [`Notify`] handles.
    pub fn spawn(&self) -> SchedulerHandle {
        let cancel = CancellationToken::new();

        let poller_handle = {
            let dispatcher = self.clone();
            let token = cancel.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(dispatcher.inner.poller_tick);
                interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
                // Consume initial immediate tick so startup behaves deterministically.
                interval.tick().await;

                loop {
                    tokio::select! {
                        biased;
                        _ = token.cancelled() => break,
                        _ = dispatcher.inner.poller_wakeup.notified() => {}
                        _ = interval.tick() => {}
                    }
                    if token.is_cancelled() {
                        break;
                    }
                    if let Err(err) = dispatcher
                        .poll_due_feeds_once(DEFAULT_DUE_FEED_BATCH_LIMIT)
                        .await
                    {
                        warn!(error = %err, "poller loop tick failed");
                    }
                }
            })
        };

        let outbox_handle = {
            let dispatcher = self.clone();
            let token = cancel.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(dispatcher.inner.outbox_tick);
                interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
                interval.tick().await;

                loop {
                    tokio::select! {
                        biased;
                        _ = token.cancelled() => break,
                        _ = dispatcher.inner.outbox_wakeup.notified() => {}
                        _ = interval.tick() => {}
                    }
                    if token.is_cancelled() {
                        break;
                    }
                    while !token.is_cancelled() {
                        match dispatcher
                            .drain_due_outbox_tasks(DEFAULT_OUTBOX_DRAIN_LIMIT)
                            .await
                        {
                            Ok(n) if n == DEFAULT_OUTBOX_DRAIN_LIMIT => {
                                tokio::task::yield_now().await;
                            }
                            Ok(_) => break,
                            Err(err) => {
                                warn!(error = %err, "outbox dispatcher loop tick failed");
                                break;
                            }
                        }
                    }
                }
            })
        };

        let maintenance_handle = {
            let dispatcher = self.clone();
            let token = cancel.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(dispatcher.inner.maintenance_tick);
                interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
                interval.tick().await;

                loop {
                    tokio::select! {
                        biased;
                        _ = token.cancelled() => break,
                        _ = dispatcher.inner.maintenance_wakeup.notified() => {}
                        _ = interval.tick() => {}
                    }
                    if token.is_cancelled() {
                        break;
                    }
                    if let Err(err) = dispatcher.run_maintenance_once().await {
                        warn!(error = %err, "maintenance loop tick failed");
                    }
                }
            })
        };

        info!("spawned background poller, outbox worker, and maintenance loops");
        SchedulerHandle {
            cancel,
            poller_handle,
            outbox_handle,
            maintenance_handle,
        }
    }

    fn earliest_pacing_allowed_at(
        &self,
        channel: &Channel,
        last_completed_at: Option<i64>,
    ) -> Option<i64> {
        let min_iv = channel.min_interval_secs.max(0);
        let jitter_max = channel.jitter_max_secs.max(0);
        if min_iv == 0 && jitter_max == 0 {
            return None;
        }

        let db_earliest = if min_iv > 0 {
            last_completed_at.map(|ts| ts.saturating_add(min_iv))
        } else {
            None
        };

        let mem_earliest = {
            let guard = self
                .inner
                .pacing_next_allowed_at
                .lock()
                .expect("pacing_next_allowed_at mutex poisoned");
            guard.get(&channel.credential_group_key).copied()
        };

        match (db_earliest, mem_earliest) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    fn record_pacing_after_delivery(&self, channel: &Channel, completed_at: i64) {
        let min_iv = channel.min_interval_secs.max(0);
        let jitter_max = channel.jitter_max_secs.max(0);
        if min_iv == 0 && jitter_max == 0 {
            return;
        }

        let jitter = if jitter_max > 0 {
            rand::random_range(0..=jitter_max)
        } else {
            0
        };
        let next_allowed = completed_at.saturating_add(min_iv).saturating_add(jitter);

        let mut guard = self
            .inner
            .pacing_next_allowed_at
            .lock()
            .expect("pacing_next_allowed_at mutex poisoned");
        let slot = guard
            .entry(channel.credential_group_key.clone())
            .or_insert(0);
        *slot = (*slot).max(next_allowed);
    }

    async fn apply_delivery_outcome(
        &self,
        task: &DeliveryTask,
        channel: &Channel,
        completed_now: i64,
        outcome: DeliveryOutcome,
    ) -> AppResult<()> {
        match outcome {
            DeliveryOutcome::Delivered {
                status_code,
                checkpoint,
                diagnostic,
                updated_capabilities,
            } => {
                if let Some(caps) = updated_capabilities {
                    self.inner
                        .db
                        .update_channel_capabilities(channel.id, &caps)
                        .await?;
                }
                self.inner
                    .db
                    .mark_task_delivered(
                        task.id,
                        completed_now,
                        status_code,
                        checkpoint.as_ref(),
                        diagnostic.as_deref(),
                    )
                    .await?;
                self.record_pacing_after_delivery(channel, completed_now);
            }
            DeliveryOutcome::Deferred {
                next_window_at,
                checkpoint,
                diagnostic,
            } => {
                let target_window = next_window_at.max(completed_now.saturating_add(1));
                self.inner
                    .db
                    .mark_task_deferred(task.id, target_window, checkpoint.as_ref(), &diagnostic)
                    .await?;
            }
            DeliveryOutcome::PermanentFailure {
                status_code,
                checkpoint,
                diagnostic,
            } => {
                self.inner
                    .db
                    .mark_task_dead_letter(
                        task.id,
                        completed_now,
                        status_code,
                        checkpoint.as_ref(),
                        &diagnostic,
                    )
                    .await?;
            }
            DeliveryOutcome::Retry {
                retry_after_secs,
                status_code,
                checkpoint,
                diagnostic,
                cooldown_credential_group,
            } => {
                // Non-penalizing deferral for Notion `202 Accepted` async import polling so
                // `GET /v1/async_tasks/{id}` checks do not consume the 6-error retry budget.
                if status_code == Some(202)
                    && !cooldown_credential_group
                    && completed_now.saturating_sub(task.created_at)
                        < DEFAULT_MAX_ASYNC_POLL_AGE_SECS
                {
                    let poll_delay = i64::try_from(retry_after_secs.unwrap_or(4))
                        .unwrap_or(4)
                        .clamp(1, 60);
                    self.inner
                        .db
                        .mark_task_deferred(
                            task.id,
                            completed_now.saturating_add(poll_delay),
                            checkpoint.as_ref(),
                            &diagnostic,
                        )
                        .await?;
                    return Ok(());
                }

                self.handle_transient_retry(
                    task,
                    channel,
                    completed_now,
                    retry_after_secs,
                    status_code,
                    checkpoint.as_ref(),
                    &diagnostic,
                    cooldown_credential_group,
                )
                .await?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_transient_retry(
        &self,
        task: &DeliveryTask,
        channel: &Channel,
        completed_now: i64,
        retry_after_secs: Option<u64>,
        status_code: Option<i32>,
        checkpoint: Option<&DeliveryCheckpoint>,
        diagnostic: &str,
        cooldown_credential_group: bool,
    ) -> AppResult<()> {
        let delay = compute_delivery_retry_delay_secs(task.attempt_count, retry_after_secs);

        if cooldown_credential_group {
            let cooldown_until = completed_now.saturating_add(delay);
            self.inner
                .db
                .set_credential_group_cooldown(&channel.credential_group_key, cooldown_until)
                .await?;
        }

        if task.attempt_count >= self.inner.max_delivery_attempts {
            self.inner
                .db
                .mark_task_dead_letter(task.id, completed_now, status_code, checkpoint, diagnostic)
                .await?;
        } else {
            let next_attempt_at = completed_now.saturating_add(delay);
            self.inner
                .db
                .mark_task_retrying(
                    task.id,
                    next_attempt_at,
                    status_code,
                    checkpoint,
                    diagnostic,
                )
                .await?;
        }

        Ok(())
    }

    async fn apply_unexpected_send_error(
        &self,
        task: &DeliveryTask,
        channel: &Channel,
        completed_now: i64,
        err: AppError,
    ) -> AppResult<()> {
        let diagnostic = err.to_string();
        match err {
            AppError::Validation(_)
            | AppError::BadRequest(_)
            | AppError::Unauthorized(_)
            | AppError::Forbidden(_)
            | AppError::Crypto(_) => {
                self.inner
                    .db
                    .mark_task_dead_letter(task.id, completed_now, None, None, &diagnostic)
                    .await?;
            }
            AppError::RateLimited {
                retry_after_secs, ..
            } => {
                self.handle_transient_retry(
                    task,
                    channel,
                    completed_now,
                    retry_after_secs,
                    Some(429),
                    None,
                    &diagnostic,
                    true,
                )
                .await?;
            }
            _ => {
                self.handle_transient_retry(
                    task,
                    channel,
                    completed_now,
                    None,
                    None,
                    None,
                    &diagnostic,
                    false,
                )
                .await?;
            }
        }
        Ok(())
    }
}

/// Handle owning the spawned background scheduler tasks (`poller`, `outbox`, `maintenance`).
pub struct SchedulerHandle {
    cancel: CancellationToken,
    poller_handle: JoinHandle<()>,
    outbox_handle: JoinHandle<()>,
    maintenance_handle: JoinHandle<()>,
}

impl fmt::Debug for SchedulerHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SchedulerHandle")
            .field("is_cancelled", &self.cancel.is_cancelled())
            .finish()
    }
}

impl SchedulerHandle {
    /// Underlying [`CancellationToken`].
    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Signal cancellation without awaiting task completion.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Signal graceful cancellation and await all three background loops to exit cleanly.
    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = tokio::join!(
            self.poller_handle,
            self.outbox_handle,
            self.maintenance_handle
        );
    }
}

/// Compute jittered retry delay in seconds for transient outbox delivery failures:
/// - Honors explicit downstream `retry_after_secs` when supplied.
/// - Otherwise applies `min(15 * 2^(attempt - 1), 3600) + jitter`.
pub fn compute_delivery_retry_delay_secs(attempt_count: i64, retry_after_secs: Option<u64>) -> i64 {
    if let Some(ra) = retry_after_secs {
        let base = i64::try_from(ra).unwrap_or(3600).clamp(1, 86_400);
        let jitter = if base >= 10 {
            rand::random_range(0..=(base / 10).min(15))
        } else {
            0
        };
        return base.saturating_add(jitter);
    }

    let shift = u32::try_from(attempt_count.saturating_sub(1).max(0))
        .unwrap_or(16)
        .min(16);
    let exp = (15_i64 << shift).min(3600);
    let jitter_cap = (exp / 5).max(1);
    let jitter = rand::random_range(0..=jitter_cap);
    exp.saturating_add(jitter)
}

fn derive_media_filename(url: &Url, content_type: Option<&str>) -> String {
    let default_ext = match content_type {
        Some("image/jpeg" | "image/jpg") => "jpg",
        Some("image/png") => "png",
        Some("image/gif") => "gif",
        Some("image/webp") => "webp",
        Some("video/mp4") => "mp4",
        Some("video/webm") => "webm",
        Some("audio/mpeg" | "audio/mp3") => "mp3",
        Some("audio/ogg") => "ogg",
        Some("audio/mp4" | "audio/x-m4a") => "m4a",
        Some("application/pdf") => "pdf",
        _ => "bin",
    };

    if let Some(seg) = url
        .path_segments()
        .and_then(|mut s| s.rfind(|part| !part.is_empty()))
    {
        let sanitized: String = seg
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            .take(80)
            .collect();
        let trimmed = sanitized.trim_matches('.');
        if !trimmed.is_empty() {
            if trimmed.contains('.') {
                return trimmed.to_string();
            }
            return format!("{trimmed}.{default_ext}");
        }
    }

    format!("media.{default_ext}")
}
