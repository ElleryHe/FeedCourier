//! SQLite WAL dual-pool database layer (`read_pool` + serial `write_pool`) and embedded migrator.

pub mod models;
pub mod repo;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

use crate::channels::{CheckpointSink, DeliveryCheckpoint, DestinationConfig};
use crate::config::AppConfig;
use crate::crypto::MasterKey;
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
    /// Path to a true pre-v0.3.0 DDL backup captured via `VACUUM INTO` before applying
    /// `0003_plugins_and_smtp_compliance.sql` on an existing v0.2.x database.
    pre_v3_backup_path: Option<PathBuf>,
    /// Path to a pre-v0.3.1 DDL backup captured via `VACUUM INTO` before applying
    /// `0004_js_plugins.sql` on an existing v0.3.0 database.
    pre_v4_backup_path: Option<PathBuf>,
}

/// Return `true` only when `candidate` AND its SQLite WAL/SHM sidecar paths (`-wal`, `-shm`)
/// do not exist on disk, preventing `VACUUM INTO` from picking up stale sidecars.
pub(crate) fn path_and_sidecars_absent(candidate: &Path) -> bool {
    if fs::symlink_metadata(candidate).is_ok() {
        return false;
    }
    for suffix in ["-wal", "-shm"] {
        let mut os = candidate.as_os_str().to_os_string();
        os.push(suffix);
        if fs::symlink_metadata(Path::new(&os)).is_ok() {
            return false;
        }
    }
    true
}

/// Best-effort cleanup of companion `-wal` and `-shm` sidecar files beside a backup database file.
pub(crate) fn remove_sqlite_sidecars(path: &Path) {
    for suffix in ["-wal", "-shm"] {
        let mut os = path.as_os_str().to_os_string();
        os.push(suffix);
        let sidecar = PathBuf::from(os);
        if fs::symlink_metadata(&sidecar).is_ok() {
            let _ = fs::remove_file(&sidecar);
        }
    }
}

/// Choose a non-overwriting backup path beside `db_path` whose main file and `-wal`/`-shm`
/// sidecars are all absent.
pub(crate) fn choose_pre_ddl_backup_path(db_path: &Path, stem: &str) -> PathBuf {
    let backup_dir = db_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let base = backup_dir.join(format!("{stem}.db"));
    if path_and_sidecars_absent(&base) {
        return base;
    }

    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let dated = backup_dir.join(format!("{stem}-{stamp}.db"));
    if path_and_sidecars_absent(&dated) {
        return dated;
    }

    for idx in 1u32..=1000 {
        let candidate = backup_dir.join(format!("{stem}-{stamp}-{idx}.db"));
        if path_and_sidecars_absent(&candidate) {
            return candidate;
        }
    }

    backup_dir.join(format!("{stem}-{stamp}-{:08x}.db", rand::random::<u32>()))
}

/// Choose a non-overwriting backup path beside `db_path` for pre-v0.2.0 upgrades.
pub(crate) fn choose_pre_v2_backup_path(db_path: &Path) -> PathBuf {
    choose_pre_ddl_backup_path(db_path, "pre-v0.2.0-backup")
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

async fn needs_pre_v3_ddl_backup(write_pool: &SqlitePool) -> AppResult<bool> {
    let (has_migrations_table,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_one(write_pool)
    .await?;
    if has_migrations_table == 0 {
        return Ok(false);
    }

    let (has_v2,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 2")
            .fetch_one(write_pool)
            .await?;
    let (has_v3,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 3")
            .fetch_one(write_pool)
            .await?;
    if has_v2 == 0 || has_v3 > 0 {
        return Ok(false);
    }

    let (has_dests,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'channel_destinations'",
    )
    .fetch_one(write_pool)
    .await?;
    if has_dests == 0 {
        return Ok(false);
    }

    let (dest_count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM channel_destinations")
        .fetch_one(write_pool)
        .await?;
    if dest_count > 0 {
        return Ok(true);
    }

    let (feed_count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM feeds")
        .fetch_one(write_pool)
        .await
        .unwrap_or((0,));
    let (channel_count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM channels")
        .fetch_one(write_pool)
        .await
        .unwrap_or((0,));
    Ok(feed_count > 0 || channel_count > 0)
}

async fn needs_pre_v4_ddl_backup(write_pool: &SqlitePool) -> AppResult<bool> {
    let (has_migrations_table,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_one(write_pool)
    .await?;
    if has_migrations_table == 0 {
        return Ok(false);
    }

    let (has_v3,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 3")
            .fetch_one(write_pool)
            .await?;
    let (has_v4,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 4")
            .fetch_one(write_pool)
            .await?;
    if has_v3 == 0 || has_v4 > 0 {
        return Ok(false);
    }

    let (channel_count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM channels")
        .fetch_one(write_pool)
        .await
        .unwrap_or((0,));
    let (feed_count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM feeds")
        .fetch_one(write_pool)
        .await
        .unwrap_or((0,));
    Ok(channel_count > 0 || feed_count > 0)
}

async fn vacuum_into_backup_cleanly(write_pool: &SqlitePool, backup_path: &Path) -> AppResult<()> {
    let backup_str = backup_path.to_str().ok_or_else(|| {
        crate::error::AppError::Validation("backup path must be valid UTF-8".to_string())
    })?;
    if let Err(err) = sqlx::query("VACUUM INTO ?1")
        .bind(backup_str)
        .execute(write_pool)
        .await
    {
        // Clean up any partial/corrupt backup file and sidecars left behind by a failed VACUUM INTO.
        let _ = fs::remove_file(backup_path);
        remove_sqlite_sidecars(backup_path);
        return Err(err.into());
    }
    remove_sqlite_sidecars(backup_path);
    Ok(())
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
            vacuum_into_backup_cleanly(&write_pool, &backup_path).await?;
            tracing::info!(
                backup_path = %backup_path.display(),
                "captured pre-v0.2.0 SQLite backup prior to applying 0002 schema migration"
            );
            Some(backup_path)
        } else {
            None
        };

        // Snapshot a genuine pre-v0.3.0 database BEFORE applying `0003_plugins_and_smtp_compliance.sql`.
        let pre_v3_backup_path =
            if pre_v2_backup_path.is_none() && needs_pre_v3_ddl_backup(&write_pool).await? {
                let backup_path = choose_pre_ddl_backup_path(db_path, "pre-v0.3.0-backup");
                vacuum_into_backup_cleanly(&write_pool, &backup_path).await?;
                tracing::info!(
                    backup_path = %backup_path.display(),
                    "captured pre-v0.3.0 SQLite backup prior to applying 0003 schema migration"
                );
                Some(backup_path)
            } else {
                None
            };

        // Snapshot a genuine pre-v0.3.1 database BEFORE applying `0004_js_plugins.sql`.
        let pre_v4_backup_path = if pre_v2_backup_path.is_none()
            && pre_v3_backup_path.is_none()
            && needs_pre_v4_ddl_backup(&write_pool).await?
        {
            let backup_path = choose_pre_ddl_backup_path(db_path, "pre-v0.3.1-backup");
            vacuum_into_backup_cleanly(&write_pool, &backup_path).await?;
            tracing::info!(
                backup_path = %backup_path.display(),
                "captured pre-v0.3.1 SQLite backup prior to applying 0004 schema migration"
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
            pre_v3_backup_path,
            pre_v4_backup_path,
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

    /// Path to the pre-DDL v0.2.x backup created during [`Database::connect`], if any.
    pub fn pre_v3_backup_path(&self) -> Option<&Path> {
        self.pre_v3_backup_path.as_deref()
    }

    /// Path to the pre-DDL v0.3.0 backup created during [`Database::connect`], if any.
    pub fn pre_v4_backup_path(&self) -> Option<&Path> {
        self.pre_v4_backup_path.as_deref()
    }

    /// Concurrent read-only pool (`PRAGMA query_only = ON`).
    pub fn read_pool(&self) -> &SqlitePool {
        &self.read_pool
    }

    /// Serial writer pool (`max_connections = 1`).
    pub fn write_pool(&self) -> &SqlitePool {
        &self.write_pool
    }

    /// Create a basic `CheckpointSink` bound to `task_id` for mid-flight checkpoint persistence.
    pub fn checkpoint_sink(&self, task_id: i64) -> DbCheckpointSink {
        DbCheckpointSink {
            db: self.clone(),
            task_id,
            destination_id: None,
            master_key: None,
            lease_duration_secs: 120,
        }
    }

    /// Create a full-featured `CheckpointSink` supporting checkpoint flushes, destination config
    /// writeback (e.g. Notion `data_source_id`), lease renewal, and SMTP hard-bounce recording.
    pub fn rich_checkpoint_sink(
        &self,
        task_id: i64,
        destination_id: i64,
        master_key: MasterKey,
        lease_duration_secs: i64,
    ) -> DbCheckpointSink {
        DbCheckpointSink {
            db: self.clone(),
            task_id,
            destination_id: Some(destination_id),
            master_key: Some(master_key),
            lease_duration_secs,
        }
    }

    /// Gracefully close both read and write pools.
    pub async fn close(&self) {
        self.read_pool.close().await;
        self.write_pool.close().await;
    }
}

/// SQLite-backed implementation of [`CheckpointSink`] persisting intermediate checkpoints,
/// destination config writebacks, lease renewals, and SMTP hard bounces to `write_pool`.
#[derive(Debug, Clone)]
pub struct DbCheckpointSink {
    db: Database,
    task_id: i64,
    destination_id: Option<i64>,
    master_key: Option<MasterKey>,
    lease_duration_secs: i64,
}

#[async_trait]
impl CheckpointSink for DbCheckpointSink {
    async fn save_checkpoint(&self, checkpoint: &DeliveryCheckpoint) -> AppResult<()> {
        self.db
            .save_task_checkpoint_typed(self.task_id, checkpoint)
            .await
    }

    async fn update_destination_config(&self, config: &DestinationConfig) -> AppResult<()> {
        if let (Some(dest_id), Some(key)) = (self.destination_id, self.master_key.as_ref()) {
            match config {
                DestinationConfig::Notion(n) => {
                    // Transactional read-modify-write on `write_pool` so concurrent WebUI
                    // `column_mapping` edits are NEVER clobbered by background 404 writebacks!
                    self.db
                        .update_notion_destination_ids_atomic(
                            dest_id,
                            key,
                            &n.data_source_id,
                            n.database_id.as_deref(),
                        )
                        .await?;
                }
                other => {
                    let encrypted = key.encrypt_json(other)?;
                    self.db
                        .update_destination_encrypted_config(dest_id, &encrypted)
                        .await?;
                }
            }
        }
        Ok(())
    }

    async fn renew_lease(&self) -> AppResult<()> {
        let new_until = models::now_unix().saturating_add(self.lease_duration_secs.max(30));
        let _ = self.db.renew_task_lease(self.task_id, new_until).await?;
        Ok(())
    }

    async fn record_smtp_hard_bounce(&self, recipient_id: i64, diagnostic: &str) -> AppResult<()> {
        let _ = self
            .db
            .mark_smtp_recipient_bounced(recipient_id, diagnostic, models::now_unix())
            .await?;
        Ok(())
    }
}
