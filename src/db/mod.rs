//! SQLite WAL dual-pool database layer (`read_pool` + serial `write_pool`) and embedded migrator.

pub mod models;
pub mod repo;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

use crate::channels::{CheckpointSink, DeliveryCheckpoint};
use crate::config::AppConfig;
use crate::error::AppResult;

/// Embedded SQLx migrations compiled directly into the single static binary.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Default SQLite busy timeout (`5000ms` per `feedcourier.md` Section 三.1).
pub const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// Dual-pool SQLite handle separating concurrent WAL readers from the single serial writer.
#[derive(Debug, Clone)]
pub struct Database {
    db_path: PathBuf,
    read_pool: SqlitePool,
    write_pool: SqlitePool,
}

impl Database {
    /// Open and migrate the SQLite database configured by `AppConfig`.
    pub async fn from_config(config: &AppConfig) -> AppResult<Self> {
        Self::connect(&config.db_path(), config.read_pool_max_connections).await
    }

    /// Initialize dual SQLite WAL pools (`write_pool` with `max_connections = 1` and concurrent
    /// `read_pool` with `query_only = ON`) and apply all embedded migrations.
    pub async fn connect(db_path: &Path, read_max_connections: u32) -> AppResult<Self> {
        if let Some(parent) = db_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }

        let base_opts = SqliteConnectOptions::new()
            .filename(db_path)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(SQLITE_BUSY_TIMEOUT)
            .foreign_keys(true);

        // 1. Open the serial writer first so the DB file, WAL header, and migrations exist.
        let write_opts = base_opts.clone().create_if_missing(true);
        let write_pool = SqlitePoolOptions::new()
            .min_connections(1)
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(15))
            .connect_with(write_opts)
            .await?;

        MIGRATOR.run(&write_pool).await?;

        // 2. Open the concurrent read pool with `PRAGMA query_only = ON` so WAL `-shm` read-marks
        // work natively while SQLite strictly rejects any accidental write query on `read_pool`.
        let read_opts = base_opts
            .create_if_missing(false)
            .pragma("query_only", "ON");
        let read_pool = SqlitePoolOptions::new()
            .max_connections(read_max_connections.max(1))
            .acquire_timeout(Duration::from_secs(15))
            .connect_with(read_opts)
            .await?;

        Ok(Self {
            db_path: db_path.to_path_buf(),
            read_pool,
            write_pool,
        })
    }

    /// Filesystem path of the underlying SQLite database file.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Concurrent read-only pool (`PRAGMA query_only = ON`).
    pub fn read_pool(&self) -> &SqlitePool {
        &self.read_pool
    }

    /// Serial writer pool (`max_connections = 1`).
    pub fn write_pool(&self) -> &SqlitePool {
        &self.write_pool
    }

    /// Create a `CheckpointSink` bound to `task_id` for mid-flight checkpoint persistence.
    pub fn checkpoint_sink(&self, task_id: i64) -> DbCheckpointSink {
        DbCheckpointSink {
            db: self.clone(),
            task_id,
        }
    }

    /// Gracefully close both read and write pools.
    pub async fn close(&self) {
        self.read_pool.close().await;
        self.write_pool.close().await;
    }
}

/// SQLite-backed implementation of [`CheckpointSink`] persisting intermediate checkpoints to `write_pool`.
#[derive(Debug, Clone)]
pub struct DbCheckpointSink {
    db: Database,
    task_id: i64,
}

#[async_trait]
impl CheckpointSink for DbCheckpointSink {
    async fn save_checkpoint(&self, checkpoint: &DeliveryCheckpoint) -> AppResult<()> {
        self.db
            .save_task_checkpoint_typed(self.task_id, checkpoint)
            .await
    }
}
