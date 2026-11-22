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
    /// Path to a true pre-v0.2.0 DDL backup captured via `VACUUM INTO` before applying
    /// `0002_channel_destinations.sql` on an existing v0.1.x database.
    pre_v2_backup_path: Option<PathBuf>,
}

/// Choose a non-overwriting backup path beside `db_path` for pre-v0.2.0 upgrades.
///
/// Uses `pre-v0.2.0-backup.db` when absent; if a file or symlink already exists at that path,
/// falls back to a UTC timestamped filename so an earlier clean backup is never clobbered.
pub(crate) fn choose_pre_v2_backup_path(db_path: &Path) -> PathBuf {
    let backup_dir = db_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let base = backup_dir.join("pre-v0.2.0-backup.db");
    if fs::symlink_metadata(&base).is_err() {
        return base;
    }

    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let dated = backup_dir.join(format!("pre-v0.2.0-backup-{stamp}.db"));
    if fs::symlink_metadata(&dated).is_err() {
        return dated;
    }

    for idx in 1u32..=1000 {
        let candidate = backup_dir.join(format!("pre-v0.2.0-backup-{stamp}-{idx}.db"));
        if fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
    }

    backup_dir.join(format!(
        "pre-v0.2.0-backup-{stamp}-{:08x}.db",
        rand::random::<u32>()
    ))
}

async fn needs_pre_v2_ddl_backup(write_pool: &SqlitePool) -> AppResult<bool> {
    let (has_migrations_table,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_one(write_pool)
    .await?;
    if has_migrations_table == 0 {
        return Ok(false);
    }

    let (has_v1,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 1")
            .fetch_one(write_pool)
            .await?;
    let (has_v2,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 2")
            .fetch_one(write_pool)
            .await?;
    if has_v1 == 0 || has_v2 > 0 {
        return Ok(false);
    }

    let (has_channels_table,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'channels'",
    )
    .fetch_one(write_pool)
    .await?;
    if has_channels_table == 0 {
        return Ok(false);
    }

    let (channel_count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM channels")
        .fetch_one(write_pool)
        .await?;
    Ok(channel_count > 0)
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

        // Snapshot a genuine pre-v0.2.0 database BEFORE applying `0002_channel_destinations.sql`
        // so downgrading back to v0.1.6 can restore the backup directly without schema mismatch.
        let pre_v2_backup_path = if needs_pre_v2_ddl_backup(&write_pool).await? {
            let backup_path = choose_pre_v2_backup_path(db_path);
            let backup_str = backup_path.to_str().ok_or_else(|| {
                crate::error::AppError::Validation(
                    "pre-v0.2.0 backup path must be valid UTF-8".to_string(),
                )
            })?;
            sqlx::query("VACUUM INTO ?1")
                .bind(backup_str)
                .execute(&write_pool)
                .await?;
            tracing::info!(
                backup_path = %backup_path.display(),
                "captured pre-v0.2.0 SQLite backup prior to applying 0002 schema migration"
            );
            Some(backup_path)
        } else {
            None
        };

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
            pre_v2_backup_path,
        })
    }

    /// Filesystem path of the underlying SQLite database file.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Path to the pre-DDL v0.1.x backup created during [`Database::connect`], if any.
    pub fn pre_v2_backup_path(&self) -> Option<&Path> {
        self.pre_v2_backup_path.as_deref()
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
