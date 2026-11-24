//! Optional ClickHouse HTTP log store (`LOG_STORE=clickhouse`).
//!
//! Configured strictly via environment variables (`LOG_CLICKHOUSE_*`) and treated as an
//! operator-trusted endpoint (bypasses SSRF blocking to allow internal Docker/LAN hosts, while
//! keeping redirects disabled via `reqwest::redirect::Policy::none()`).
//!
//! Writes via HTTP `INSERT ... FORMAT JSONEachRow` and queries via parameterized ClickHouse SQL
//! (`param_*` query parameters + `FORMAT JSON`). Outages never block startup or delivery: failed
//! batches increment the dropped counter and emit a rate-limited stderr warning (at most once/60s).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

use super::{LogQuery, LogRecord, LogStoreBackend, parse_level_num};

#[derive(Debug)]
pub struct ClickHouseLogStore {
    client: Client,
    base_url: String,
    database: String,
    table: String,
    user: Option<String>,
    password: Option<String>,
    retention_days: u32,
    schema_ready: AtomicBool,
}

#[derive(Debug, Serialize)]
struct ClickHouseInsertRow<'a> {
    ts: i64,
    ts_ms: i64,
    level: &'a str,
    level_num: i32,
    target: &'a str,
    message: &'a str,
    fields_json: &'a str,
    feed_id: Option<i64>,
    task_id: Option<i64>,
    route_id: Option<i64>,
    channel_id: Option<i64>,
    destination_id: Option<i64>,
    plugin_id: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
struct ClickHouseJsonResponse<T> {
    #[serde(default = "Vec::new")]
    data: Vec<T>,
}

#[derive(Debug, Deserialize)]
struct ClickHouseQueryRow {
    #[serde(deserialize_with = "deserialize_i64_or_string")]
    ts_ms: i64,
    level: String,
    #[serde(deserialize_with = "deserialize_i32_or_string")]
    level_num: i32,
    target: String,
    message: String,
    fields_json: String,
    #[serde(default, deserialize_with = "deserialize_opt_i64_or_string")]
    feed_id: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_opt_i64_or_string")]
    task_id: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_opt_i64_or_string")]
    route_id: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_opt_i64_or_string")]
    channel_id: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_opt_i64_or_string")]
    destination_id: Option<i64>,
    #[serde(default)]
    plugin_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClickHouseMinTsRow {
    #[serde(default, deserialize_with = "deserialize_opt_i64_or_string")]
    oldest_ts_ms: Option<i64>,
}

impl ClickHouseLogStore {
    pub fn new(
        base_url: String,
        database: String,
        table: String,
        user: Option<String>,
        password: Option<String>,
        retention_days: u32,
    ) -> AppResult<Self> {
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|err| AppError::Config(format!("failed to build ClickHouse client: {err}")))?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            database,
            table,
            user,
            password,
            retention_days: retention_days.max(1),
            schema_ready: AtomicBool::new(false),
        })
    }

    fn apply_auth(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(user) = self.user.as_deref() {
            req = req.header("X-ClickHouse-User", user);
        }
        if let Some(pass) = self.password.as_deref() {
            req = req.header("X-ClickHouse-Key", pass);
        }
        req
    }

    /// Idempotently create the target database and `MergeTree` table with TTL.
    pub async fn ensure_schema(&self) -> AppResult<()> {
        let create_db = format!("CREATE DATABASE IF NOT EXISTS {}", self.database);
        self.exec_ddl(&create_db).await?;

        let create_table = format!(
            "CREATE TABLE IF NOT EXISTS {}.{} (\
                ts DateTime64(3, 'UTC'), \
                ts_ms Int64, \
                level LowCardinality(String), \
                level_num Int32, \
                target String, \
                message String, \
                fields_json String, \
                feed_id Nullable(Int64), \
                task_id Nullable(Int64), \
                route_id Nullable(Int64), \
                channel_id Nullable(Int64), \
                destination_id Nullable(Int64), \
                plugin_id Nullable(String)\
            ) ENGINE = MergeTree \
            ORDER BY (ts_ms, level_num) \
            TTL ts + INTERVAL {} DAY",
            self.database, self.table, self.retention_days
        );
        self.exec_ddl(&create_table).await?;
        self.schema_ready.store(true, Ordering::Release);
        Ok(())
    }

    async fn exec_ddl(&self, sql: &str) -> AppResult<()> {
        let resp = self
            .apply_auth(self.client.post(&self.base_url))
            .body(sql.to_string())
            .send()
            .await
            .map_err(|err| AppError::Upstream(format!("ClickHouse DDL failed: {err}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::Upstream(format!(
                "ClickHouse DDL returned HTTP {status}: {}",
                body.chars().take(240).collect::<String>()
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl LogStoreBackend for ClickHouseLogStore {
    fn backend_name(&self) -> &'static str {
        "clickhouse"
    }

    async fn write_batch(&self, batch: &[LogRecord]) -> AppResult<()> {
        if batch.is_empty() {
            return Ok(());
        }
        if !self.schema_ready.load(Ordering::Acquire) {
            self.ensure_schema().await?;
        }

        let mut payload = String::with_capacity(batch.len() * 256);
        for rec in batch {
            let row = ClickHouseInsertRow {
                ts: rec.ts_ms,
                ts_ms: rec.ts_ms,
                level: &rec.level,
                level_num: rec.level_num,
                target: &rec.target,
                message: &rec.message,
                fields_json: &rec.fields_json,
                feed_id: rec.feed_id,
                task_id: rec.task_id,
                route_id: rec.route_id,
                channel_id: rec.channel_id,
                destination_id: rec.destination_id,
                plugin_id: rec.plugin_id.as_deref(),
            };
            let line = serde_json::to_string(&row).map_err(|err| {
                AppError::Internal(anyhow::anyhow!(
                    "ClickHouse JSONEachRow encode error: {err}"
                ))
            })?;
            payload.push_str(&line);
            payload.push('\n');
        }

        let query = format!(
            "INSERT INTO {}.{} FORMAT JSONEachRow",
            self.database, self.table
        );
        let resp = self
            .apply_auth(
                self.client
                    .post(&self.base_url)
                    .query(&[
                        ("database", self.database.as_str()),
                        ("query", query.as_str()),
                    ])
                    .header("Content-Type", "application/x-ndjson"),
            )
            .body(payload)
            .send()
            .await
            .map_err(|err| AppError::Upstream(format!("ClickHouse insert failed: {err}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::Upstream(format!(
                "ClickHouse insert returned HTTP {status}: {}",
                body.chars().take(240).collect::<String>()
            )));
        }
        Ok(())
    }

    async fn query(&self, q: &LogQuery) -> AppResult<Vec<LogRecord>> {
        if !self.schema_ready.load(Ordering::Acquire) {
            self.ensure_schema().await?;
        }

        let mut sql = format!(
            "SELECT ts_ms, level, level_num, target, message, fields_json, \
             feed_id, task_id, route_id, channel_id, destination_id, plugin_id \
             FROM {}.{} WHERE 1 = 1",
            self.database, self.table
        );
        let mut params: Vec<(String, String)> =
            vec![("database".to_string(), self.database.clone())];

        if let Some(min_lvl) = q.min_level.as_deref().and_then(parse_level_num) {
            sql.push_str(" AND level_num >= {min_level:Int32}");
            params.push(("param_min_level".to_string(), min_lvl.to_string()));
        }
        if let Some(prefix) = q
            .target_prefix
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            sql.push_str(" AND startsWith(target, {target_prefix:String})");
            params.push(("param_target_prefix".to_string(), prefix.to_string()));
        }
        if let Some(since_ms) = q.since_ms {
            sql.push_str(
                " AND ts_ms >= {since_ms:Int64} \
                 AND ts >= fromUnixTimestamp64Milli({since_ms:Int64}, 'UTC')",
            );
            params.push(("param_since_ms".to_string(), since_ms.to_string()));
        }
        if let Some(until_ms) = q.until_ms {
            sql.push_str(
                " AND ts_ms <= {until_ms:Int64} \
                 AND ts <= fromUnixTimestamp64Milli({until_ms:Int64}, 'UTC')",
            );
            params.push(("param_until_ms".to_string(), until_ms.to_string()));
        }
        if let Some(needle) = q
            .contains
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            sql.push_str(
                " AND (positionCaseInsensitiveUTF8(message, {contains:String}) > 0 \
                 OR positionCaseInsensitiveUTF8(fields_json, {contains:String}) > 0)",
            );
            params.push(("param_contains".to_string(), needle.to_string()));
        }
        if let Some(task_id) = q.task_id {
            sql.push_str(" AND task_id = {task_id:Int64}");
            params.push(("param_task_id".to_string(), task_id.to_string()));
        }
        if let Some(feed_id) = q.feed_id {
            sql.push_str(" AND feed_id = {feed_id:Int64}");
            params.push(("param_feed_id".to_string(), feed_id.to_string()));
        }
        if let Some(route_id) = q.route_id {
            sql.push_str(" AND route_id = {route_id:Int64}");
            params.push(("param_route_id".to_string(), route_id.to_string()));
        }
        if let Some(channel_id) = q.channel_id {
            sql.push_str(" AND channel_id = {channel_id:Int64}");
            params.push(("param_channel_id".to_string(), channel_id.to_string()));
        }
        if let Some(destination_id) = q.destination_id {
            sql.push_str(" AND destination_id = {destination_id:Int64}");
            params.push((
                "param_destination_id".to_string(),
                destination_id.to_string(),
            ));
        }
        if let Some(plugin_id) = q
            .plugin_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            sql.push_str(" AND plugin_id = {plugin_id:String}");
            params.push(("param_plugin_id".to_string(), plugin_id.to_string()));
        }

        if q.ascending {
            sql.push_str(" ORDER BY ts_ms ASC LIMIT {limit:UInt32} FORMAT JSON");
        } else {
            sql.push_str(" ORDER BY ts_ms DESC LIMIT {limit:UInt32} FORMAT JSON");
        }
        params.push(("param_limit".to_string(), q.effective_limit().to_string()));

        let resp = self
            .apply_auth(self.client.post(&self.base_url).query(&params))
            .body(sql)
            .send()
            .await
            .map_err(|err| AppError::Upstream(format!("ClickHouse query failed: {err}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::Upstream(format!(
                "ClickHouse query returned HTTP {status}: {}",
                body.chars().take(240).collect::<String>()
            )));
        }

        let parsed: ClickHouseJsonResponse<ClickHouseQueryRow> = resp
            .json()
            .await
            .map_err(|err| AppError::Upstream(format!("ClickHouse JSON decode failed: {err}")))?;

        Ok(parsed
            .data
            .into_iter()
            .map(|r| LogRecord {
                ts_ms: r.ts_ms,
                level: r.level,
                level_num: r.level_num,
                target: r.target,
                message: r.message,
                fields_json: r.fields_json,
                feed_id: r.feed_id,
                task_id: r.task_id,
                route_id: r.route_id,
                channel_id: r.channel_id,
                destination_id: r.destination_id,
                plugin_id: r.plugin_id,
            })
            .collect())
    }

    async fn oldest_ts_ms(&self) -> AppResult<Option<i64>> {
        if !self.schema_ready.load(Ordering::Acquire) {
            return Ok(None);
        }
        let sql = format!(
            "SELECT minOrNull(ts_ms) AS oldest_ts_ms FROM {}.{} FORMAT JSON",
            self.database, self.table
        );
        let resp = self
            .apply_auth(
                self.client
                    .post(&self.base_url)
                    .query(&[("database", self.database.as_str())]),
            )
            .body(sql)
            .send()
            .await
            .map_err(|err| {
                AppError::Upstream(format!("ClickHouse oldest_ts query failed: {err}"))
            })?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let parsed: ClickHouseJsonResponse<ClickHouseMinTsRow> = resp
            .json()
            .await
            .map_err(|err| AppError::Upstream(format!("ClickHouse JSON decode failed: {err}")))?;
        Ok(parsed
            .data
            .into_iter()
            .next()
            .and_then(|row| row.oldest_ts_ms))
    }

    async fn prune(&self) -> AppResult<u64> {
        // ClickHouse enforces retention automatically via table TTL; max_rows is SQLite-only.
        Ok(0)
    }
}

fn deserialize_i64_or_string<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let val = serde_json::Value::deserialize(deserializer)?;
    match val {
        serde_json::Value::Number(n) => n
            .as_i64()
            .ok_or_else(|| serde::de::Error::custom("expected i64 number")),
        serde_json::Value::String(s) => s.parse::<i64>().map_err(serde::de::Error::custom),
        _ => Err(serde::de::Error::custom("expected i64 or string")),
    }
}

fn deserialize_i32_or_string<'de, D>(deserializer: D) -> Result<i32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let val = serde_json::Value::deserialize(deserializer)?;
    match val {
        serde_json::Value::Number(n) => n
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| serde::de::Error::custom("expected i32 number")),
        serde_json::Value::String(s) => s.parse::<i32>().map_err(serde::de::Error::custom),
        _ => Err(serde::de::Error::custom("expected i32 or string")),
    }
}

fn deserialize_opt_i64_or_string<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let val = Option::<serde_json::Value>::deserialize(deserializer)?;
    match val {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(n)) => n
            .as_i64()
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("expected optional i64")),
        Some(serde_json::Value::String(s)) => {
            if s.is_empty() {
                Ok(None)
            } else {
                s.parse::<i64>().map(Some).map_err(serde::de::Error::custom)
            }
        }
        _ => Err(serde::de::Error::custom("expected optional i64 or string")),
    }
}
