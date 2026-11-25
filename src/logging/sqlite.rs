//! Independent SQLite log store (`/data/logs.db`) isolated from the main business database.
//!
//! Uses WAL mode with a dedicated single writer connection and a bounded read-only pool so
//! log ingestion and MCP debug queries never contend with `feedcourier.db`'s write lock.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};

use crate::error::{AppError, AppResult};

use super::{
    DEFAULT_KEYWORD_SEARCH_WINDOW_MS, LOG_QUERY_TIMEOUT, LogQuery, LogRecord, LogStoreBackend,
    parse_level_num, validate_log_keyword,
};

#[derive(Debug)]
pub struct SqliteLogStore {
    path: PathBuf,
    write_pool: SqlitePool,
    read_pool: SqlitePool,
    retention_days: u32,
    max_rows: u64,
}

impl SqliteLogStore {
    /// Open or create `<path>` (`logs.db`) in WAL mode and ensure the `logs` table & indexes exist.
    pub async fn open(path: &Path, retention_days: u32, max_rows: u64) -> AppResult<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let write_opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(5));

        let write_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(write_opts)
            .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts_ms INTEGER NOT NULL,
                level TEXT NOT NULL,
                level_num INTEGER NOT NULL,
                target TEXT NOT NULL,
                message TEXT NOT NULL,
                fields_json TEXT NOT NULL,
                feed_id INTEGER,
                task_id INTEGER,
                route_id INTEGER,
                channel_id INTEGER,
                destination_id INTEGER,
                plugin_id TEXT
            )
            "#,
        )
        .execute(&write_pool)
        .await?;

        for idx_sql in [
            "CREATE INDEX IF NOT EXISTS idx_logs_ts ON logs(ts_ms DESC)",
            "CREATE INDEX IF NOT EXISTS idx_logs_level_ts ON logs(level_num, ts_ms DESC)",
            "CREATE INDEX IF NOT EXISTS idx_logs_task_id ON logs(task_id, ts_ms ASC) WHERE task_id IS NOT NULL",
            "CREATE INDEX IF NOT EXISTS idx_logs_feed_id ON logs(feed_id, ts_ms DESC) WHERE feed_id IS NOT NULL",
        ] {
            sqlx::query(idx_sql).execute(&write_pool).await?;
        }

        let read_opts = SqliteConnectOptions::new()
            .filename(path)
            .read_only(true)
            .busy_timeout(Duration::from_secs(5));
        let read_pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(read_opts)
            .await?;

        Ok(Self {
            path: path.to_path_buf(),
            write_pool,
            read_pool,
            retention_days: retention_days.max(1),
            max_rows: max_rows.max(100),
        })
    }
}

#[async_trait]
impl LogStoreBackend for SqliteLogStore {
    fn backend_name(&self) -> &'static str {
        "sqlite"
    }

    async fn write_batch(&self, batch: &[LogRecord]) -> AppResult<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let mut tx = self.write_pool.begin().await?;
        for rec in batch {
            sqlx::query(
                r#"
                INSERT INTO logs (
                    ts_ms, level, level_num, target, message, fields_json,
                    feed_id, task_id, route_id, channel_id, destination_id, plugin_id
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                "#,
            )
            .bind(rec.ts_ms)
            .bind(&rec.level)
            .bind(rec.level_num)
            .bind(&rec.target)
            .bind(&rec.message)
            .bind(&rec.fields_json)
            .bind(rec.feed_id)
            .bind(rec.task_id)
            .bind(rec.route_id)
            .bind(rec.channel_id)
            .bind(rec.destination_id)
            .bind(&rec.plugin_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn query(&self, q: &LogQuery) -> AppResult<Vec<LogRecord>> {
        let keyword = validate_log_keyword(q.contains.as_deref())?;
        // When keyword search is requested without an explicit `since_ms` bound, default to the
        // last 24 hours so SQLite uses `idx_logs_ts` to bound the scan before evaluating `LIKE`.
        let effective_since_ms = match (q.since_ms, keyword.is_some()) {
            (Some(ts), _) => Some(ts),
            (None, true) => Some(
                chrono::Utc::now()
                    .timestamp_millis()
                    .saturating_sub(DEFAULT_KEYWORD_SEARCH_WINDOW_MS),
            ),
            (None, false) => None,
        };

        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
            "SELECT ts_ms, level, level_num, target, message, fields_json, \
             feed_id, task_id, route_id, channel_id, destination_id, plugin_id \
             FROM logs WHERE 1 = 1",
        );

        if let Some(min_lvl) = q.min_level.as_deref().and_then(parse_level_num) {
            qb.push(" AND level_num >= ");
            qb.push_bind(min_lvl);
        }
        if let Some(prefix) = q
            .target_prefix
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            qb.push(" AND target LIKE ");
            qb.push_bind(format!("{}%", escape_like(prefix)));
            qb.push(" ESCAPE '\\'");
        }
        if let Some(since_ms) = effective_since_ms {
            qb.push(" AND ts_ms >= ");
            qb.push_bind(since_ms);
        }
        if let Some(until_ms) = q.until_ms {
            qb.push(" AND ts_ms <= ");
            qb.push_bind(until_ms);
        }
        if let Some(ref needle) = keyword {
            let pat = format!("%{}%", escape_like(needle));
            qb.push(" AND (message LIKE ");
            qb.push_bind(pat.clone());
            qb.push(" ESCAPE '\\' OR fields_json LIKE ");
            qb.push_bind(pat);
            qb.push(" ESCAPE '\\')");
        }
        if let Some(task_id) = q.task_id {
            qb.push(" AND task_id = ");
            qb.push_bind(task_id);
        }
        if let Some(feed_id) = q.feed_id {
            qb.push(" AND feed_id = ");
            qb.push_bind(feed_id);
        }
        if let Some(route_id) = q.route_id {
            qb.push(" AND route_id = ");
            qb.push_bind(route_id);
        }
        if let Some(channel_id) = q.channel_id {
            qb.push(" AND channel_id = ");
            qb.push_bind(channel_id);
        }
        if let Some(destination_id) = q.destination_id {
            qb.push(" AND destination_id = ");
            qb.push_bind(destination_id);
        }
        if let Some(plugin_id) = q
            .plugin_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            qb.push(" AND plugin_id = ");
            qb.push_bind(plugin_id.to_string());
        }

        if q.ascending {
            qb.push(" ORDER BY ts_ms ASC, id ASC LIMIT ");
        } else {
            qb.push(" ORDER BY ts_ms DESC, id DESC LIMIT ");
        }
        qb.push_bind(q.effective_limit() as i64);

        let rows = tokio::time::timeout(LOG_QUERY_TIMEOUT, qb.build().fetch_all(&self.read_pool))
            .await
            .map_err(|_| {
                AppError::Upstream(format!(
                    "SQLite log query timed out after {}s; narrow the time range or filters",
                    LOG_QUERY_TIMEOUT.as_secs()
                ))
            })??;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(LogRecord {
                ts_ms: row.try_get("ts_ms")?,
                level: row.try_get("level")?,
                level_num: row.try_get("level_num")?,
                target: row.try_get("target")?,
                message: row.try_get("message")?,
                fields_json: row.try_get("fields_json")?,
                feed_id: row.try_get("feed_id")?,
                task_id: row.try_get("task_id")?,
                route_id: row.try_get("route_id")?,
                channel_id: row.try_get("channel_id")?,
                destination_id: row.try_get("destination_id")?,
                plugin_id: row.try_get("plugin_id")?,
            });
        }
        Ok(out)
    }

    async fn oldest_ts_ms(&self) -> AppResult<Option<i64>> {
        let val: Option<i64> = sqlx::query_scalar("SELECT MIN(ts_ms) FROM logs")
            .fetch_one(&self.read_pool)
            .await?;
        Ok(val)
    }

    async fn prune(&self) -> AppResult<u64> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let retention_ms = i64::from(self.retention_days).saturating_mul(86_400_000);
        let cutoff_ms = now_ms.saturating_sub(retention_ms);

        let by_age = sqlx::query("DELETE FROM logs WHERE ts_ms < ?1")
            .bind(cutoff_ms)
            .execute(&self.write_pool)
            .await?
            .rows_affected();

        let keep = i64::try_from(self.max_rows).unwrap_or(i64::MAX);
        let by_rows = sqlx::query(
            r#"
            DELETE FROM logs
            WHERE id <= COALESCE(
                (SELECT id FROM logs ORDER BY id DESC LIMIT 1 OFFSET ?1),
                0
            )
            "#,
        )
        .bind(keep)
        .execute(&self.write_pool)
        .await?
        .rows_affected();

        Ok(by_age.saturating_add(by_rows))
    }

    fn file_size_bytes(&self) -> Option<u64> {
        let mut total = 0u64;
        let base = self.path.to_string_lossy();
        for candidate in [
            self.path.clone(),
            PathBuf::from(format!("{base}-wal")),
            PathBuf::from(format!("{base}-shm")),
        ] {
            if let Ok(meta) = std::fs::metadata(&candidate) {
                total = total.saturating_add(meta.len());
            }
        }
        Some(total)
    }

    async fn close(&self) {
        self.read_pool.close().await;
        self.write_pool.close().await;
    }
}

fn escape_like(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(ch);
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(ts_ms: i64, message: &str) -> LogRecord {
        LogRecord {
            ts_ms,
            level: "INFO".to_string(),
            level_num: 30,
            target: "feedcourier::test".to_string(),
            message: message.to_string(),
            fields_json: "{}".to_string(),
            feed_id: None,
            task_id: None,
            route_id: None,
            channel_id: None,
            destination_id: None,
            plugin_id: None,
        }
    }

    #[tokio::test]
    async fn opens_relative_and_special_char_paths_and_enforces_keyword_window() {
        let dir = tempfile::TempDir::new().unwrap();
        // Path containing `#` and `?` would break `sqlite://...` URI parsing unless `.filename()` is used.
        let special_path = dir.path().join("logs#1?test.db");
        let store = SqliteLogStore::open(&special_path, 7, 10_000)
            .await
            .expect("SqliteConnectOptions::new().filename() must accept special characters");

        let now_ms = chrono::Utc::now().timestamp_millis();
        let old_ms = now_ms - (48 * 60 * 60 * 1_000);
        store
            .write_batch(&[
                sample_record(old_ms, "needle-ancient-event"),
                sample_record(now_ms, "needle-recent-event"),
            ])
            .await
            .unwrap();

        // 1-char keyword is rejected.
        let short_err = store
            .query(&LogQuery {
                contains: Some("a".to_string()),
                ..LogQuery::default()
            })
            .await
            .unwrap_err();
        assert!(matches!(short_err, AppError::Validation(_)));

        // > 128-char keyword is rejected.
        let long_err = store
            .query(&LogQuery {
                contains: Some("x".repeat(129)),
                ..LogQuery::default()
            })
            .await
            .unwrap_err();
        assert!(matches!(long_err, AppError::Validation(_)));

        // Keyword without `since_ms` defaults to last 24h (finds recent, skips 48h-old row).
        let default_window = store
            .query(&LogQuery {
                contains: Some("needle".to_string()),
                ..LogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(default_window.len(), 1);
        assert_eq!(default_window[0].message, "needle-recent-event");

        // Explicit `since_ms` still searches older history.
        let wide_window = store
            .query(&LogQuery {
                contains: Some("needle".to_string()),
                since_ms: Some(old_ms - 1_000),
                ..LogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(wide_window.len(), 2);

        store.close().await;
    }
}
