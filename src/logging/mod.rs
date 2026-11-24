//! Structured, redacted logging subsystem with dual output:
//! 1. Stdout (`docker logs`) in `text` or `json` format (`LOG_FORMAT`, filtered by `RUST_LOG`).
//! 2. Asynchronous, non-blocking persistent log storage backed by an isolated SQLite database
//!    (`<DATA_DIR>/logs.db`, default) or ClickHouse HTTP (`LOG_STORE=clickhouse`), or disabled
//!    (`LOG_STORE=off`).

pub mod clickhouse;
pub mod layer;
pub mod redact;
pub mod sqlite;

use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::Level;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use crate::config::{AppConfig, LogStoreKind};
use crate::error::AppResult;

use self::clickhouse::ClickHouseLogStore;
use self::layer::RedactingLayer;
use self::sqlite::SqliteLogStore;

/// Bounded in-memory log ingestion channel capacity (events beyond this are dropped & counted).
pub const LOG_CHANNEL_CAPACITY: usize = 10_000;

/// Maximum batch size flushed in a single SQLite transaction or ClickHouse HTTP request.
pub const LOG_BATCH_SIZE: usize = 500;

/// Maximum interval between background log flushes.
pub const LOG_FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// Default log query page size.
pub const DEFAULT_LOG_QUERY_LIMIT: usize = 100;

/// Hard upper bound on log records returned in a single query.
pub const MAX_LOG_QUERY_LIMIT: usize = 500;

/// Redacted, structured log entry persisted in `logs.db` or ClickHouse.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRecord {
    pub ts_ms: i64,
    pub level: String,
    pub level_num: i32,
    pub target: String,
    pub message: String,
    pub fields_json: String,
    pub feed_id: Option<i64>,
    pub task_id: Option<i64>,
    pub route_id: Option<i64>,
    pub channel_id: Option<i64>,
    pub destination_id: Option<i64>,
    pub plugin_id: Option<String>,
}

/// Filter parameters for querying stored logs.
#[derive(Debug, Clone, Default)]
pub struct LogQuery {
    pub min_level: Option<String>,
    pub target_prefix: Option<String>,
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
    pub contains: Option<String>,
    pub task_id: Option<i64>,
    pub feed_id: Option<i64>,
    pub route_id: Option<i64>,
    pub channel_id: Option<i64>,
    pub destination_id: Option<i64>,
    pub plugin_id: Option<String>,
    pub limit: Option<usize>,
    pub ascending: bool,
}

impl LogQuery {
    pub fn effective_limit(&self) -> usize {
        self.limit
            .unwrap_or(DEFAULT_LOG_QUERY_LIMIT)
            .clamp(1, MAX_LOG_QUERY_LIMIT)
    }
}

/// Health and metadata summary for the active log store backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogStoreStatus {
    pub backend: String,
    pub healthy: bool,
    pub dropped: u64,
    pub oldest_ts_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Combined response returned when querying the log store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogQueryResult {
    pub store: LogStoreStatus,
    pub logs: Vec<LogRecord>,
}

/// Storage backend trait implemented by [`SqliteLogStore`] and [`ClickHouseLogStore`].
#[async_trait]
pub trait LogStoreBackend: Send + Sync {
    fn backend_name(&self) -> &'static str;
    async fn write_batch(&self, batch: &[LogRecord]) -> AppResult<()>;
    async fn query(&self, query: &LogQuery) -> AppResult<Vec<LogRecord>>;
    async fn oldest_ts_ms(&self) -> AppResult<Option<i64>>;
    async fn prune(&self) -> AppResult<u64>;
    fn file_size_bytes(&self) -> Option<u64> {
        None
    }
    async fn close(&self) {}
}

enum LogWorkerCmd {
    Flush(oneshot::Sender<()>),
    Shutdown(oneshot::Sender<()>),
}

struct LogStoreInner {
    backend_name: &'static str,
    backend: Option<Arc<dyn LogStoreBackend>>,
    dropped: Arc<AtomicU64>,
    healthy: Arc<AtomicBool>,
    last_error: Arc<RwLock<Option<String>>>,
    tx: Option<mpsc::Sender<LogRecord>>,
    cmd_tx: Option<mpsc::UnboundedSender<LogWorkerCmd>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

/// Cheaply cloneable runtime handle to the active log store (SQLite, ClickHouse, or disabled).
#[derive(Clone)]
pub struct LogStoreHandle {
    inner: Arc<LogStoreInner>,
}

impl fmt::Debug for LogStoreHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LogStoreHandle")
            .field("backend", &self.inner.backend_name)
            .field("healthy", &self.inner.healthy.load(Ordering::Relaxed))
            .field("dropped", &self.inner.dropped.load(Ordering::Relaxed))
            .finish()
    }
}

impl LogStoreHandle {
    /// Construct a disabled (`"off"`) handle (used in tests or when `LOG_STORE=off`).
    pub fn disabled() -> Self {
        Self {
            inner: Arc::new(LogStoreInner {
                backend_name: "off",
                backend: None,
                dropped: Arc::new(AtomicU64::new(0)),
                healthy: Arc::new(AtomicBool::new(true)),
                last_error: Arc::new(RwLock::new(None)),
                tx: None,
                cmd_tx: None,
                worker: Mutex::new(None),
            }),
        }
    }

    /// Construct a handle backed by a concrete [`LogStoreBackend`] and spawn its async batch writer.
    pub fn from_backend(
        backend: Arc<dyn LogStoreBackend>,
        initial_healthy: bool,
        initial_error: Option<String>,
    ) -> Self {
        let (tx, rx) = mpsc::channel(LOG_CHANNEL_CAPACITY);
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let dropped = Arc::new(AtomicU64::new(0));
        let healthy = Arc::new(AtomicBool::new(initial_healthy));
        let last_error = Arc::new(RwLock::new(initial_error));
        let backend_name = backend.backend_name();

        let worker_handle = spawn_batch_writer(
            Arc::clone(&backend),
            rx,
            cmd_rx,
            Arc::clone(&dropped),
            Arc::clone(&healthy),
            Arc::clone(&last_error),
        );

        Self {
            inner: Arc::new(LogStoreInner {
                backend_name,
                backend: Some(backend),
                dropped,
                healthy,
                last_error,
                tx: Some(tx),
                cmd_tx: Some(cmd_tx),
                worker: Mutex::new(Some(worker_handle)),
            }),
        }
    }

    pub fn backend_name(&self) -> &'static str {
        self.inner.backend_name
    }

    pub fn dropped_count(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    pub fn is_healthy(&self) -> bool {
        self.inner.healthy.load(Ordering::Relaxed)
    }

    pub fn file_size_bytes(&self) -> Option<u64> {
        self.inner.backend.as_ref()?.file_size_bytes()
    }

    /// Directly push a redacted record into the store channel (primarily for tests).
    pub fn push_record(&self, rec: LogRecord) -> bool {
        match &self.inner.tx {
            Some(tx) => {
                if tx.try_send(rec).is_ok() {
                    true
                } else {
                    self.inner.dropped.fetch_add(1, Ordering::Relaxed);
                    false
                }
            }
            None => false,
        }
    }

    /// Drain all currently buffered log records to the backend before returning.
    pub async fn flush_now(&self) {
        if let Some(ref cmd_tx) = self.inner.cmd_tx {
            let (ack_tx, ack_rx) = oneshot::channel();
            if cmd_tx.send(LogWorkerCmd::Flush(ack_tx)).is_ok() {
                let _ = tokio::time::timeout(Duration::from_secs(5), ack_rx).await;
            }
        }
    }

    /// Retrieve current backend health, drop counter, and earliest timestamp.
    pub async fn status(&self) -> LogStoreStatus {
        let dropped = self.dropped_count();
        let Some(ref backend) = self.inner.backend else {
            return LogStoreStatus {
                backend: self.inner.backend_name.to_string(),
                healthy: self.inner.backend_name == "off",
                dropped,
                oldest_ts_ms: None,
                error: self.last_error_string(),
            };
        };

        let oldest_ts_ms = backend.oldest_ts_ms().await.ok().flatten();
        LogStoreStatus {
            backend: self.inner.backend_name.to_string(),
            healthy: self.is_healthy(),
            dropped,
            oldest_ts_ms,
            error: self.last_error_string(),
        }
    }

    /// Query stored logs without failing hard when the backend is unavailable.
    pub async fn query_logs(&self, q: &LogQuery) -> LogQueryResult {
        let Some(ref backend) = self.inner.backend else {
            return LogQueryResult {
                store: self.status().await,
                logs: Vec::new(),
            };
        };

        match backend.query(q).await {
            Ok(logs) => {
                self.inner.healthy.store(true, Ordering::Relaxed);
                if let Ok(mut guard) = self.inner.last_error.write() {
                    *guard = None;
                }
                let oldest_ts_ms = backend.oldest_ts_ms().await.ok().flatten();
                LogQueryResult {
                    store: LogStoreStatus {
                        backend: self.inner.backend_name.to_string(),
                        healthy: true,
                        dropped: self.dropped_count(),
                        oldest_ts_ms,
                        error: None,
                    },
                    logs,
                }
            }
            Err(err) => {
                let err_msg = redact::redact_text(&err.to_string());
                self.inner.healthy.store(false, Ordering::Relaxed);
                if let Ok(mut guard) = self.inner.last_error.write() {
                    *guard = Some(err_msg.clone());
                }
                LogQueryResult {
                    store: LogStoreStatus {
                        backend: self.inner.backend_name.to_string(),
                        healthy: false,
                        dropped: self.dropped_count(),
                        oldest_ts_ms: None,
                        error: Some(err_msg),
                    },
                    logs: Vec::new(),
                }
            }
        }
    }

    /// Prune expired/excess log rows (called by the hourly maintenance loop).
    pub async fn prune(&self) -> AppResult<u64> {
        let Some(ref backend) = self.inner.backend else {
            return Ok(0);
        };
        backend.prune().await
    }

    /// Flush all remaining buffered logs, stop the background writer gracefully, and close backend connections.
    pub async fn shutdown(&self) {
        if let Some(ref cmd_tx) = self.inner.cmd_tx {
            let (ack_tx, ack_rx) = oneshot::channel();
            if cmd_tx.send(LogWorkerCmd::Shutdown(ack_tx)).is_ok() {
                let _ = tokio::time::timeout(Duration::from_secs(5), ack_rx).await;
            }
        }
        if let Some(handle) = self.inner.worker.lock().ok().and_then(|mut g| g.take()) {
            let abort_handle = handle.abort_handle();
            if tokio::time::timeout(Duration::from_secs(2), handle)
                .await
                .is_err()
            {
                abort_handle.abort();
            }
        }
        if let Some(ref backend) = self.inner.backend {
            backend.close().await;
        }
    }

    fn last_error_string(&self) -> Option<String> {
        self.inner.last_error.read().ok().and_then(|g| g.clone())
    }
}

/// Initialize the global tracing subscriber (`RedactingLayer` + stdout + configured log store).
///
/// Storage initialization errors (e.g. unreachable ClickHouse at boot) NEVER abort startup:
/// stdout logging remains active and the returned [`LogStoreHandle`] reports unhealthy status.
pub async fn init_logging(config: &AppConfig) -> LogStoreHandle {
    let handle = match config.logging.store {
        LogStoreKind::Off => LogStoreHandle::disabled(),
        LogStoreKind::Sqlite => {
            let path = config.logs_db_path();
            match SqliteLogStore::open(
                &path,
                config.logging.retention_days,
                config.logging.max_rows,
            )
            .await
            {
                Ok(store) => LogStoreHandle::from_backend(Arc::new(store), true, None),
                Err(err) => {
                    let msg = format!("failed to open SQLite log store {}: {err}", path.display());
                    write_stderr_notice(&msg);
                    LogStoreHandle {
                        inner: Arc::new(LogStoreInner {
                            backend_name: "sqlite",
                            backend: None,
                            dropped: Arc::new(AtomicU64::new(0)),
                            healthy: Arc::new(AtomicBool::new(false)),
                            last_error: Arc::new(RwLock::new(Some(msg))),
                            tx: None,
                            cmd_tx: None,
                            worker: Mutex::new(None),
                        }),
                    }
                }
            }
        }
        LogStoreKind::ClickHouse => {
            let url = config.logging.clickhouse_url.clone().unwrap_or_default();
            match ClickHouseLogStore::new(
                url,
                config.logging.clickhouse_database.clone(),
                config.logging.clickhouse_table.clone(),
                config.logging.clickhouse_user.clone(),
                config.logging.clickhouse_password.clone(),
                config.logging.retention_days,
            ) {
                Ok(store) => {
                    let (healthy, init_err) = match store.ensure_schema().await {
                        Ok(()) => (true, None),
                        Err(err) => {
                            let msg = redact::redact_text(&format!(
                                "ClickHouse log store unreachable at startup (continuing without blocking): {err}"
                            ));
                            write_stderr_notice(&msg);
                            (false, Some(msg))
                        }
                    };
                    LogStoreHandle::from_backend(Arc::new(store), healthy, init_err)
                }
                Err(err) => {
                    let msg = redact::redact_text(&err.to_string());
                    write_stderr_notice(&msg);
                    LogStoreHandle {
                        inner: Arc::new(LogStoreInner {
                            backend_name: "clickhouse",
                            backend: None,
                            dropped: Arc::new(AtomicU64::new(0)),
                            healthy: Arc::new(AtomicBool::new(false)),
                            last_error: Arc::new(RwLock::new(Some(msg))),
                            tx: None,
                            cmd_tx: None,
                            worker: Mutex::new(None),
                        }),
                    }
                }
            }
        }
    };

    let stdout_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("feedcourier=info,tower_http=info"));
    let store_min_level_num = parse_level_num(&config.logging.store_level).unwrap_or(30);

    let layer = RedactingLayer::new(
        stdout_filter,
        config.logging.format,
        store_min_level_num,
        handle.inner.tx.clone(),
        Arc::clone(&handle.inner.dropped),
    );

    let _ = tracing_subscriber::registry().with(layer).try_init();
    handle
}

fn spawn_batch_writer(
    backend: Arc<dyn LogStoreBackend>,
    mut rx: mpsc::Receiver<LogRecord>,
    mut cmd_rx: mpsc::UnboundedReceiver<LogWorkerCmd>,
    dropped: Arc<AtomicU64>,
    healthy: Arc<AtomicBool>,
    last_error: Arc<RwLock<Option<String>>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut buf: Vec<LogRecord> = Vec::with_capacity(LOG_BATCH_SIZE);
        let mut tick = tokio::time::interval(LOG_FLUSH_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_warn_at: Option<Instant> = None;

        loop {
            tokio::select! {
                biased;
                maybe_cmd = cmd_rx.recv() => {
                    match maybe_cmd {
                        Some(LogWorkerCmd::Flush(ack)) => {
                            drain_all(&mut rx, &mut buf, &backend, &dropped, &healthy, &last_error, &mut last_warn_at).await;
                            let _ = ack.send(());
                        }
                        Some(LogWorkerCmd::Shutdown(ack)) => {
                            drain_all(&mut rx, &mut buf, &backend, &dropped, &healthy, &last_error, &mut last_warn_at).await;
                            let _ = ack.send(());
                            break;
                        }
                        None => {
                            drain_all(&mut rx, &mut buf, &backend, &dropped, &healthy, &last_error, &mut last_warn_at).await;
                            break;
                        }
                    }
                }
                maybe_rec = rx.recv() => {
                    match maybe_rec {
                        Some(rec) => {
                            buf.push(rec);
                            while buf.len() < LOG_BATCH_SIZE {
                                match rx.try_recv() {
                                    Ok(next) => buf.push(next),
                                    Err(_) => break,
                                }
                            }
                            if buf.len() >= LOG_BATCH_SIZE {
                                flush_batch(&backend, &mut buf, &dropped, &healthy, &last_error, &mut last_warn_at).await;
                            }
                        }
                        None => {
                            if !buf.is_empty() {
                                flush_batch(&backend, &mut buf, &dropped, &healthy, &last_error, &mut last_warn_at).await;
                            }
                            break;
                        }
                    }
                }
                _ = tick.tick() => {
                    if !buf.is_empty() {
                        flush_batch(&backend, &mut buf, &dropped, &healthy, &last_error, &mut last_warn_at).await;
                    }
                }
            }
        }
    })
}

async fn drain_all(
    rx: &mut mpsc::Receiver<LogRecord>,
    buf: &mut Vec<LogRecord>,
    backend: &Arc<dyn LogStoreBackend>,
    dropped: &Arc<AtomicU64>,
    healthy: &Arc<AtomicBool>,
    last_error: &Arc<RwLock<Option<String>>>,
    last_warn_at: &mut Option<Instant>,
) {
    loop {
        while buf.len() < LOG_BATCH_SIZE {
            match rx.try_recv() {
                Ok(next) => buf.push(next),
                Err(_) => break,
            }
        }
        if buf.is_empty() {
            break;
        }
        flush_batch(backend, buf, dropped, healthy, last_error, last_warn_at).await;
    }
}

async fn flush_batch(
    backend: &Arc<dyn LogStoreBackend>,
    buf: &mut Vec<LogRecord>,
    dropped: &Arc<AtomicU64>,
    healthy: &Arc<AtomicBool>,
    last_error: &Arc<RwLock<Option<String>>>,
    last_warn_at: &mut Option<Instant>,
) {
    if buf.is_empty() {
        return;
    }
    match backend.write_batch(buf).await {
        Ok(()) => {
            healthy.store(true, Ordering::Relaxed);
            if let Ok(mut guard) = last_error.write() {
                *guard = None;
            }
        }
        Err(err) => {
            let count = buf.len() as u64;
            dropped.fetch_add(count, Ordering::Relaxed);
            healthy.store(false, Ordering::Relaxed);
            let msg = redact::redact_text(&err.to_string());
            if let Ok(mut guard) = last_error.write() {
                *guard = Some(msg.clone());
            }
            let should_warn =
                last_warn_at.is_none_or(|prev| prev.elapsed() >= Duration::from_secs(60));
            if should_warn {
                *last_warn_at = Some(Instant::now());
                write_stderr_notice(&format!(
                    "log store ({}) write failed (dropped {count} records): {msg}",
                    backend.backend_name()
                ));
            }
        }
    }
    buf.clear();
}

fn write_stderr_notice(msg: &str) {
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut err = io::stderr().lock();
    let _ = writeln!(err, "{ts}  WARN feedcourier::logging: {msg}");
}

pub const fn level_to_num(level: Level) -> i32 {
    match level {
        Level::TRACE => 10,
        Level::DEBUG => 20,
        Level::INFO => 30,
        Level::WARN => 40,
        Level::ERROR => 50,
    }
}

pub fn parse_level_num(raw: &str) -> Option<i32> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "trace" => Some(10),
        "debug" => Some(20),
        "info" => Some(30),
        "warn" | "warning" => Some(40),
        "error" => Some(50),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn flush_now_and_shutdown_drain_more_than_one_batch_without_loss() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("logs.db");
        let store = Arc::new(SqliteLogStore::open(&db_path, 7, 10_000).await.unwrap());
        let handle = LogStoreHandle::from_backend(
            Arc::clone(&store) as Arc<dyn LogStoreBackend>,
            true,
            None,
        );

        let make_rec = |i: i64| LogRecord {
            ts_ms: 1_700_000_000_000 + i,
            level: "INFO".to_string(),
            level_num: 30,
            target: "feedcourier::test".to_string(),
            message: format!("msg-{i}"),
            fields_json: "{}".to_string(),
            feed_id: None,
            task_id: None,
            route_id: None,
            channel_id: None,
            destination_id: None,
            plugin_id: None,
        };

        // Push 1,250 records (> 2 * LOG_BATCH_SIZE = 1000) and verify flush_now drains all of them.
        for i in 0..1_250 {
            assert!(handle.push_record(make_rec(i)));
        }
        handle.flush_now().await;
        assert_eq!(handle.dropped_count(), 0);

        let first_batch = store
            .query(&LogQuery {
                limit: Some(MAX_LOG_QUERY_LIMIT),
                ..LogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(first_batch.len(), MAX_LOG_QUERY_LIMIT);
        assert_eq!(first_batch[0].message, "msg-1249");

        // Push 600 more records and call shutdown(); every single record must reach SQLite before close.
        for i in 1_250..1_850 {
            assert!(handle.push_record(make_rec(i)));
        }
        handle.shutdown().await;

        let reopened = SqliteLogStore::open(&db_path, 7, 10_000).await.unwrap();
        let newest = reopened
            .query(&LogQuery {
                limit: Some(5),
                ..LogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(newest[0].message, "msg-1849");
        let oldest = reopened
            .query(&LogQuery {
                limit: Some(5),
                ascending: true,
                ..LogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(oldest[0].message, "msg-0");
        reopened.close().await;
    }
}
