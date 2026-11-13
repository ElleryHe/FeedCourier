//! Full CRUD, outbox lease state machine, quota counters, `NOT EXISTS` payload GC,
//! `PRAGMA optimize`, and `VACUUM INTO` online snapshot backup operations.

use std::fs;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::channels::DeliveryCheckpoint;
use crate::db::Database;
use crate::db::models::{
    Channel, DeliveryTask, DeliveryTaskDetail, DeliveryTaskStatus, Entry, EntryInsertOutcome, Feed,
    FeedHealthStatus, McpToken, McpTokenScope, NewChannel, NewEntry, NewFeed, NewRoute,
    OverviewSnapshot, QuotaUsage, Route, Session, SystemSetting, UpdateChannel, UpdateFeed,
    UpdateRoute, now_unix,
};
use crate::error::{AppError, AppResult};

impl Database {
    // =========================================================================
    // Feeds CRUD & Polling State Machine
    // =========================================================================

    /// Insert a new feed subscription and return the persisted row.
    pub async fn create_feed(&self, input: &NewFeed) -> AppResult<Feed> {
        if input.poll_interval_secs <= 0 {
            return Err(AppError::Validation(
                "poll_interval_secs must be greater than 0".to_string(),
            ));
        }
        let now = now_unix();
        let tags_json = serde_json::to_string(&input.tags)?;
        let next_poll_at = input.next_poll_at.unwrap_or(now);

        let feed = sqlx::query_as::<_, Feed>(
            r#"
            INSERT INTO feeds (
                url, title, site_url, favicon_url, tags_json,
                poll_interval_secs, fetch_full_content, use_proxy,
                health_status, consecutive_failures, next_poll_at,
                silent_init, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'active', 0, ?9, ?10, ?11, ?11)
            RETURNING *
            "#,
        )
        .bind(input.url.trim())
        .bind(&input.title)
        .bind(&input.site_url)
        .bind(&input.favicon_url)
        .bind(tags_json)
        .bind(input.poll_interval_secs)
        .bind(input.fetch_full_content)
        .bind(input.use_proxy)
        .bind(next_poll_at)
        .bind(input.silent_init)
        .bind(now)
        .fetch_one(&self.write_pool)
        .await?;

        Ok(feed)
    }

    /// Fetch a feed by ID from `read_pool`.
    pub async fn get_feed(&self, feed_id: i64) -> AppResult<Option<Feed>> {
        let row = sqlx::query_as::<_, Feed>("SELECT * FROM feeds WHERE id = ?1")
            .bind(feed_id)
            .fetch_optional(&self.read_pool)
            .await?;
        Ok(row)
    }

    /// Fetch a feed by exact URL from `read_pool`.
    pub async fn get_feed_by_url(&self, url: &str) -> AppResult<Option<Feed>> {
        let row = sqlx::query_as::<_, Feed>("SELECT * FROM feeds WHERE url = ?1")
            .bind(url.trim())
            .fetch_optional(&self.read_pool)
            .await?;
        Ok(row)
    }

    /// List all feeds ordered by `id ASC`.
    pub async fn list_feeds(&self) -> AppResult<Vec<Feed>> {
        let rows = sqlx::query_as::<_, Feed>("SELECT * FROM feeds ORDER BY id ASC")
            .fetch_all(&self.read_pool)
            .await?;
        Ok(rows)
    }

    /// List feeds eligible for polling at `now` (`active` or `backoff` with `next_poll_at <= now`).
    pub async fn list_due_feeds(&self, now: i64, limit: i64) -> AppResult<Vec<Feed>> {
        let rows = sqlx::query_as::<_, Feed>(
            r#"
            SELECT * FROM feeds
            WHERE health_status IN ('active', 'backoff')
              AND (next_poll_at IS NULL OR next_poll_at <= ?1)
            ORDER BY COALESCE(next_poll_at, 0) ASC, id ASC
            LIMIT ?2
            "#,
        )
        .bind(now)
        .bind(limit)
        .fetch_all(&self.read_pool)
        .await?;
        Ok(rows)
    }

    /// Apply partial updates to a feed serially on `write_pool`.
    pub async fn update_feed(&self, feed_id: i64, patch: &UpdateFeed) -> AppResult<Feed> {
        let current = sqlx::query_as::<_, Feed>("SELECT * FROM feeds WHERE id = ?1")
            .bind(feed_id)
            .fetch_optional(&self.write_pool)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("feed {feed_id} not found")))?;

        let url = patch
            .url
            .as_deref()
            .map(str::trim)
            .unwrap_or(&current.url)
            .to_string();
        let title = patch.title.clone().unwrap_or(current.title);
        let site_url = patch.site_url.clone().unwrap_or(current.site_url);
        let favicon_url = patch.favicon_url.clone().unwrap_or(current.favicon_url);
        let tags_json = match &patch.tags {
            Some(tags) => serde_json::to_string(tags)?,
            None => current.tags_json,
        };
        let poll_interval_secs = patch
            .poll_interval_secs
            .unwrap_or(current.poll_interval_secs);
        if poll_interval_secs <= 0 {
            return Err(AppError::Validation(
                "poll_interval_secs must be greater than 0".to_string(),
            ));
        }
        let fetch_full_content = patch
            .fetch_full_content
            .unwrap_or(current.fetch_full_content);
        let use_proxy = patch.use_proxy.unwrap_or(current.use_proxy);
        let silent_init = patch.silent_init.unwrap_or(current.silent_init);
        let health_status = patch.health_status.unwrap_or(current.health_status);
        let now = now_unix();

        let updated = sqlx::query_as::<_, Feed>(
            r#"
            UPDATE feeds
            SET url = ?1,
                title = ?2,
                site_url = ?3,
                favicon_url = ?4,
                tags_json = ?5,
                poll_interval_secs = ?6,
                fetch_full_content = ?7,
                use_proxy = ?8,
                silent_init = ?9,
                health_status = ?10,
                updated_at = ?11
            WHERE id = ?12
            RETURNING *
            "#,
        )
        .bind(url)
        .bind(title)
        .bind(site_url)
        .bind(favicon_url)
        .bind(tags_json)
        .bind(poll_interval_secs)
        .bind(fetch_full_content)
        .bind(use_proxy)
        .bind(silent_init)
        .bind(health_status)
        .bind(now)
        .bind(feed_id)
        .fetch_one(&self.write_pool)
        .await?;

        Ok(updated)
    }

    /// Record a successful feed poll (including `304 Not Modified`), resetting failure counters.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_feed_poll_success(
        &self,
        feed_id: i64,
        etag: Option<&str>,
        last_modified: Option<&str>,
        next_poll_at: i64,
        discovered_title: Option<&str>,
        site_url: Option<&str>,
        favicon_url: Option<&str>,
        clear_silent_init: bool,
    ) -> AppResult<Feed> {
        let now = now_unix();
        let row = sqlx::query_as::<_, Feed>(
            r#"
            UPDATE feeds
            SET etag = COALESCE(?1, etag),
                last_modified = COALESCE(?2, last_modified),
                health_status = CASE WHEN health_status = 'paused' THEN 'paused' ELSE 'active' END,
                consecutive_failures = 0,
                next_poll_at = ?3,
                last_error = NULL,
                title = CASE WHEN title = '' AND ?4 IS NOT NULL THEN ?4 ELSE title END,
                site_url = COALESCE(?5, site_url),
                favicon_url = COALESCE(?6, favicon_url),
                silent_init = CASE WHEN ?7 THEN 0 ELSE silent_init END,
                updated_at = ?8
            WHERE id = ?9
            RETURNING *
            "#,
        )
        .bind(etag)
        .bind(last_modified)
        .bind(next_poll_at)
        .bind(discovered_title)
        .bind(site_url)
        .bind(favicon_url)
        .bind(clear_silent_init)
        .bind(now)
        .bind(feed_id)
        .fetch_optional(&self.write_pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("feed {feed_id} not found")))?;

        Ok(row)
    }

    /// Record a failed feed poll, transitioning to `backoff` or `circuit_open`.
    pub async fn record_feed_poll_failure(
        &self,
        feed_id: i64,
        consecutive_failures: i64,
        health_status: FeedHealthStatus,
        next_poll_at: Option<i64>,
        last_error: &str,
    ) -> AppResult<Feed> {
        let now = now_unix();
        let row = sqlx::query_as::<_, Feed>(
            r#"
            UPDATE feeds
            SET consecutive_failures = ?1,
                health_status = ?2,
                next_poll_at = ?3,
                last_error = ?4,
                updated_at = ?5
            WHERE id = ?6
            RETURNING *
            "#,
        )
        .bind(consecutive_failures)
        .bind(health_status)
        .bind(next_poll_at)
        .bind(last_error)
        .bind(now)
        .bind(feed_id)
        .fetch_optional(&self.write_pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("feed {feed_id} not found")))?;

        Ok(row)
    }

    /// Clear circuit-breaker / backoff state and schedule an immediate poll (`now`).
    pub async fn reset_feed_for_immediate_poll(&self, feed_id: i64, now: i64) -> AppResult<Feed> {
        let row = sqlx::query_as::<_, Feed>(
            r#"
            UPDATE feeds
            SET health_status = 'active',
                consecutive_failures = 0,
                next_poll_at = ?1,
                last_error = NULL,
                updated_at = ?1
            WHERE id = ?2
            RETURNING *
            "#,
        )
        .bind(now)
        .bind(feed_id)
        .fetch_optional(&self.write_pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("feed {feed_id} not found")))?;

        Ok(row)
    }

    /// Delete a feed (cascading its `entries` and `delivery_tasks`).
    pub async fn delete_feed(&self, feed_id: i64) -> AppResult<bool> {
        let res = sqlx::query("DELETE FROM feeds WHERE id = ?1")
            .bind(feed_id)
            .execute(&self.write_pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    // =========================================================================
    // Entries & Fingerprint Deduplication
    // =========================================================================

    /// Idempotently insert an entry by `(feed_id, fingerprint_sha256)`.
    /// If the fingerprint already exists on this feed, returns `EntryInsertOutcome::Duplicate`
    /// without mutating or re-enqueueing historical edits (`feedcourier.md` Section 四.2).
    pub async fn insert_entry_if_new(&self, input: &NewEntry) -> AppResult<EntryInsertOutcome> {
        if input.fingerprint_sha256.len() != 64 {
            return Err(AppError::Validation(format!(
                "fingerprint_sha256 must be 64 hex characters, got {}",
                input.fingerprint_sha256.len()
            )));
        }

        let media_json = if input.media.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&input.media)?)
        };

        let inserted = sqlx::query_as::<_, Entry>(
            r#"
            INSERT INTO entries (
                feed_id, fingerprint_sha256, published_at, discovered_at,
                title, canonical_url, author, content_html,
                first_sentence, summary_text, media_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT (feed_id, fingerprint_sha256) DO NOTHING
            RETURNING *
            "#,
        )
        .bind(input.feed_id)
        .bind(&input.fingerprint_sha256)
        .bind(input.published_at)
        .bind(input.discovered_at)
        .bind(&input.title)
        .bind(&input.canonical_url)
        .bind(&input.author)
        .bind(&input.content_html)
        .bind(&input.first_sentence)
        .bind(&input.summary_text)
        .bind(media_json)
        .fetch_optional(&self.write_pool)
        .await?;

        if let Some(entry) = inserted {
            return Ok(EntryInsertOutcome::Inserted(Box::new(entry)));
        }

        let existing_id: (i64,) =
            sqlx::query_as("SELECT id FROM entries WHERE feed_id = ?1 AND fingerprint_sha256 = ?2")
                .bind(input.feed_id)
                .bind(&input.fingerprint_sha256)
                .fetch_one(&self.write_pool)
                .await?;

        Ok(EntryInsertOutcome::Duplicate {
            existing_id: existing_id.0,
        })
    }

    /// Fetch an entry by ID from `read_pool`.
    pub async fn get_entry(&self, entry_id: i64) -> AppResult<Option<Entry>> {
        let row = sqlx::query_as::<_, Entry>("SELECT * FROM entries WHERE id = ?1")
            .bind(entry_id)
            .fetch_optional(&self.read_pool)
            .await?;
        Ok(row)
    }

    /// List recent entries for a feed ordered by `discovered_at DESC`.
    pub async fn list_entries_for_feed(&self, feed_id: i64, limit: i64) -> AppResult<Vec<Entry>> {
        let rows = sqlx::query_as::<_, Entry>(
            "SELECT * FROM entries WHERE feed_id = ?1 ORDER BY discovered_at DESC, id DESC LIMIT ?2",
        )
        .bind(feed_id)
        .bind(limit)
        .fetch_all(&self.read_pool)
        .await?;
        Ok(rows)
    }

    // =========================================================================
    // Channels CRUD, Capabilities & Shared Rate-Limit Cooldowns
    // =========================================================================

    /// Create a trusted delivery channel with encrypted credentials.
    pub async fn create_channel(&self, input: &NewChannel) -> AppResult<Channel> {
        let now = now_unix();
        let row = sqlx::query_as::<_, Channel>(
            r#"
            INSERT INTO channels (
                name, kind, encrypted_config, credential_group_key,
                capabilities_json, min_interval_secs, jitter_max_secs,
                hourly_quota, daily_quota, use_proxy, enabled,
                created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
            RETURNING *
            "#,
        )
        .bind(input.name.trim())
        .bind(input.kind)
        .bind(&input.encrypted_config)
        .bind(&input.credential_group_key)
        .bind(&input.capabilities_json)
        .bind(input.min_interval_secs)
        .bind(input.jitter_max_secs)
        .bind(input.hourly_quota)
        .bind(input.daily_quota)
        .bind(input.use_proxy)
        .bind(input.enabled)
        .bind(now)
        .fetch_one(&self.write_pool)
        .await?;

        Ok(row)
    }

    /// Fetch a channel by ID from `read_pool`.
    pub async fn get_channel(&self, channel_id: i64) -> AppResult<Option<Channel>> {
        let row = sqlx::query_as::<_, Channel>("SELECT * FROM channels WHERE id = ?1")
            .bind(channel_id)
            .fetch_optional(&self.read_pool)
            .await?;
        Ok(row)
    }

    /// List all channels ordered by `id ASC`.
    pub async fn list_channels(&self) -> AppResult<Vec<Channel>> {
        let rows = sqlx::query_as::<_, Channel>("SELECT * FROM channels ORDER BY id ASC")
            .fetch_all(&self.read_pool)
            .await?;
        Ok(rows)
    }

    /// Apply partial updates to a channel serially on `write_pool`.
    pub async fn update_channel(
        &self,
        channel_id: i64,
        patch: &UpdateChannel,
    ) -> AppResult<Channel> {
        let current = sqlx::query_as::<_, Channel>("SELECT * FROM channels WHERE id = ?1")
            .bind(channel_id)
            .fetch_optional(&self.write_pool)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("channel {channel_id} not found")))?;

        let name = patch
            .name
            .as_deref()
            .map(str::trim)
            .unwrap_or(&current.name)
            .to_string();
        let encrypted_config = patch
            .encrypted_config
            .clone()
            .unwrap_or(current.encrypted_config);
        let credential_group_key = patch
            .credential_group_key
            .clone()
            .unwrap_or(current.credential_group_key);
        let capabilities_json = patch
            .capabilities_json
            .clone()
            .unwrap_or(current.capabilities_json);
        let min_interval_secs = patch.min_interval_secs.unwrap_or(current.min_interval_secs);
        let jitter_max_secs = patch.jitter_max_secs.unwrap_or(current.jitter_max_secs);
        let hourly_quota = patch.hourly_quota.unwrap_or(current.hourly_quota);
        let daily_quota = patch.daily_quota.unwrap_or(current.daily_quota);
        let use_proxy = patch.use_proxy.unwrap_or(current.use_proxy);
        let cooldown_until = patch.cooldown_until.unwrap_or(current.cooldown_until);
        let enabled = patch.enabled.unwrap_or(current.enabled);
        let now = now_unix();

        let updated = sqlx::query_as::<_, Channel>(
            r#"
            UPDATE channels
            SET name = ?1,
                encrypted_config = ?2,
                credential_group_key = ?3,
                capabilities_json = ?4,
                min_interval_secs = ?5,
                jitter_max_secs = ?6,
                hourly_quota = ?7,
                daily_quota = ?8,
                use_proxy = ?9,
                cooldown_until = ?10,
                enabled = ?11,
                updated_at = ?12
            WHERE id = ?13
            RETURNING *
            "#,
        )
        .bind(name)
        .bind(encrypted_config)
        .bind(credential_group_key)
        .bind(capabilities_json)
        .bind(min_interval_secs)
        .bind(jitter_max_secs)
        .bind(hourly_quota)
        .bind(daily_quota)
        .bind(use_proxy)
        .bind(cooldown_until)
        .bind(enabled)
        .bind(now)
        .bind(channel_id)
        .fetch_one(&self.write_pool)
        .await?;

        Ok(updated)
    }

    /// Update probed platform capabilities JSON on `channels`.
    pub async fn update_channel_capabilities(
        &self,
        channel_id: i64,
        capabilities: &serde_json::Value,
    ) -> AppResult<()> {
        let now = now_unix();
        let capabilities_json = serde_json::to_string(capabilities)?;
        sqlx::query("UPDATE channels SET capabilities_json = ?1, updated_at = ?2 WHERE id = ?3")
            .bind(capabilities_json)
            .bind(now)
            .bind(channel_id)
            .execute(&self.write_pool)
            .await?;
        Ok(())
    }

    /// Place a single channel into cooldown until at least `cooldown_until`.
    pub async fn set_channel_cooldown(
        &self,
        channel_id: i64,
        cooldown_until: i64,
    ) -> AppResult<()> {
        let now = now_unix();
        sqlx::query(
            r#"
            UPDATE channels
            SET cooldown_until = MAX(COALESCE(cooldown_until, 0), ?1),
                updated_at = ?2
            WHERE id = ?3
            "#,
        )
        .bind(cooldown_until)
        .bind(now)
        .bind(channel_id)
        .execute(&self.write_pool)
        .await?;
        Ok(())
    }

    /// Place all channels sharing `credential_group_key` (same SMTP account / Bot token / Notion
    /// secret) into cooldown until at least `cooldown_until`.
    pub async fn set_credential_group_cooldown(
        &self,
        credential_group_key: &str,
        cooldown_until: i64,
    ) -> AppResult<u64> {
        let now = now_unix();
        let res = sqlx::query(
            r#"
            UPDATE channels
            SET cooldown_until = MAX(COALESCE(cooldown_until, 0), ?1),
                updated_at = ?2
            WHERE credential_group_key = ?3
            "#,
        )
        .bind(cooldown_until)
        .bind(now)
        .bind(credential_group_key)
        .execute(&self.write_pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Delete a channel (cascading its `routes` and `delivery_tasks`).
    pub async fn delete_channel(&self, channel_id: i64) -> AppResult<bool> {
        let res = sqlx::query("DELETE FROM channels WHERE id = ?1")
            .bind(channel_id)
            .execute(&self.write_pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    // =========================================================================
    // Routes CRUD
    // =========================================================================

    /// Create a routing rule linking feeds/tags to a trusted channel.
    pub async fn create_route(&self, input: &NewRoute) -> AppResult<Route> {
        let now = now_unix();
        let feed_ids_json = serde_json::to_string(&input.feed_ids)?;
        let match_tags_json = serde_json::to_string(&input.match_tags)?;
        let include_keywords_json = serde_json::to_string(&input.include_keywords)?;
        let exclude_keywords_json = serde_json::to_string(&input.exclude_keywords)?;
        let options_json = serde_json::to_string(&input.options)?;

        let row = sqlx::query_as::<_, Route>(
            r#"
            INSERT INTO routes (
                name, feed_ids_json, match_tags_json, channel_id,
                include_keywords_json, exclude_keywords_json,
                include_regex, exclude_regex, options_json, enabled,
                created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
            RETURNING *
            "#,
        )
        .bind(input.name.trim())
        .bind(feed_ids_json)
        .bind(match_tags_json)
        .bind(input.channel_id)
        .bind(include_keywords_json)
        .bind(exclude_keywords_json)
        .bind(&input.include_regex)
        .bind(&input.exclude_regex)
        .bind(options_json)
        .bind(input.enabled)
        .bind(now)
        .fetch_one(&self.write_pool)
        .await?;

        Ok(row)
    }

    /// Fetch a route by ID from `read_pool`.
    pub async fn get_route(&self, route_id: i64) -> AppResult<Option<Route>> {
        let row = sqlx::query_as::<_, Route>("SELECT * FROM routes WHERE id = ?1")
            .bind(route_id)
            .fetch_optional(&self.read_pool)
            .await?;
        Ok(row)
    }

    /// List all routes ordered by `id ASC`.
    pub async fn list_routes(&self) -> AppResult<Vec<Route>> {
        let rows = sqlx::query_as::<_, Route>("SELECT * FROM routes ORDER BY id ASC")
            .fetch_all(&self.read_pool)
            .await?;
        Ok(rows)
    }

    /// List enabled routes whose target channel is also enabled.
    pub async fn list_enabled_routes(&self) -> AppResult<Vec<Route>> {
        let rows = sqlx::query_as::<_, Route>(
            r#"
            SELECT r.*
            FROM routes r
            JOIN channels c ON c.id = r.channel_id
            WHERE r.enabled = 1 AND c.enabled = 1
            ORDER BY r.id ASC
            "#,
        )
        .fetch_all(&self.read_pool)
        .await?;
        Ok(rows)
    }

    /// Apply partial updates to a route serially on `write_pool`.
    pub async fn update_route(&self, route_id: i64, patch: &UpdateRoute) -> AppResult<Route> {
        let current = sqlx::query_as::<_, Route>("SELECT * FROM routes WHERE id = ?1")
            .bind(route_id)
            .fetch_optional(&self.write_pool)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("route {route_id} not found")))?;

        let name = patch
            .name
            .as_deref()
            .map(str::trim)
            .unwrap_or(&current.name)
            .to_string();
        let feed_ids_json = match &patch.feed_ids {
            Some(ids) => serde_json::to_string(ids)?,
            None => current.feed_ids_json,
        };
        let match_tags_json = match &patch.match_tags {
            Some(tags) => serde_json::to_string(tags)?,
            None => current.match_tags_json,
        };
        let channel_id = patch.channel_id.unwrap_or(current.channel_id);
        let include_keywords_json = match &patch.include_keywords {
            Some(kws) => serde_json::to_string(kws)?,
            None => current.include_keywords_json,
        };
        let exclude_keywords_json = match &patch.exclude_keywords {
            Some(kws) => serde_json::to_string(kws)?,
            None => current.exclude_keywords_json,
        };
        let include_regex = patch.include_regex.clone().unwrap_or(current.include_regex);
        let exclude_regex = patch.exclude_regex.clone().unwrap_or(current.exclude_regex);
        let options_json = match &patch.options {
            Some(opts) => serde_json::to_string(opts)?,
            None => current.options_json,
        };
        let enabled = patch.enabled.unwrap_or(current.enabled);
        let now = now_unix();

        let updated = sqlx::query_as::<_, Route>(
            r#"
            UPDATE routes
            SET name = ?1,
                feed_ids_json = ?2,
                match_tags_json = ?3,
                channel_id = ?4,
                include_keywords_json = ?5,
                exclude_keywords_json = ?6,
                include_regex = ?7,
                exclude_regex = ?8,
                options_json = ?9,
                enabled = ?10,
                updated_at = ?11
            WHERE id = ?12
            RETURNING *
            "#,
        )
        .bind(name)
        .bind(feed_ids_json)
        .bind(match_tags_json)
        .bind(channel_id)
        .bind(include_keywords_json)
        .bind(exclude_keywords_json)
        .bind(include_regex)
        .bind(exclude_regex)
        .bind(options_json)
        .bind(enabled)
        .bind(now)
        .bind(route_id)
        .fetch_one(&self.write_pool)
        .await?;

        Ok(updated)
    }

    /// Delete a route (cascading its `delivery_tasks`).
    pub async fn delete_route(&self, route_id: i64) -> AppResult<bool> {
        let res = sqlx::query("DELETE FROM routes WHERE id = ?1")
            .bind(route_id)
            .execute(&self.write_pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    // =========================================================================
    // Outbox Delivery Tasks, Atomic Lease Claim, Checkpoints & Quotas
    // =========================================================================

    /// Idempotently enqueue a delivery task (`1 Task = 1 Entry × 1 Route`).
    /// Returns `Ok(Some(task))` when newly inserted or `Ok(None)` when `(route_id, entry_id)`
    /// already exists in `delivery_tasks`.
    pub async fn enqueue_delivery_task(
        &self,
        route_id: i64,
        channel_id: i64,
        entry_id: i64,
        next_attempt_at: i64,
    ) -> AppResult<Option<DeliveryTask>> {
        let now = now_unix();
        let row = sqlx::query_as::<_, DeliveryTask>(
            r#"
            INSERT INTO delivery_tasks (
                route_id, channel_id, entry_id, status, next_attempt_at, created_at
            )
            VALUES (?1, ?2, ?3, 'queued', ?4, ?5)
            ON CONFLICT (route_id, entry_id) DO NOTHING
            RETURNING *
            "#,
        )
        .bind(route_id)
        .bind(channel_id)
        .bind(entry_id)
        .bind(next_attempt_at)
        .bind(now)
        .fetch_optional(&self.write_pool)
        .await?;

        Ok(row)
    }

    /// Atomically claim the next eligible delivery task on the serial `write_pool`, covering:
    /// 1. Due tasks in `queued`, `retrying`, or `deferred` with `next_attempt_at <= now`, AND
    /// 2. Orphaned/crashed `delivering` tasks whose `lease_until <= now` (automatic crash recovery).
    ///
    /// Channels that are disabled or currently inside `cooldown_until > now` are skipped.
    /// Existing `checkpoint_json` is preserved intact so the worker reconciles state before sending.
    pub async fn claim_next_delivery_task(
        &self,
        now: i64,
        lease_duration_secs: i64,
    ) -> AppResult<Option<DeliveryTask>> {
        let lease_until = now.saturating_add(lease_duration_secs.max(1));

        let claimed = sqlx::query_as::<_, DeliveryTask>(
            r#"
            UPDATE delivery_tasks
            SET status = 'delivering',
                lease_until = ?1,
                attempt_count = attempt_count + 1
            WHERE id = (
                SELECT dt.id
                FROM delivery_tasks dt
                JOIN channels c ON c.id = dt.channel_id
                WHERE c.enabled = 1
                  AND (c.cooldown_until IS NULL OR c.cooldown_until <= ?2)
                  AND (
                      (dt.status IN ('queued', 'retrying', 'deferred') AND dt.next_attempt_at <= ?2)
                      OR (dt.status = 'delivering' AND dt.lease_until IS NOT NULL AND dt.lease_until <= ?2)
                  )
                ORDER BY
                    CASE WHEN dt.status = 'delivering' THEN 0 ELSE 1 END ASC,
                    CASE
                        WHEN dt.status = 'delivering' THEN COALESCE(dt.lease_until, dt.next_attempt_at)
                        ELSE dt.next_attempt_at
                    END ASC,
                    dt.id ASC
                LIMIT 1
            )
            RETURNING *
            "#,
        )
        .bind(lease_until)
        .bind(now)
        .fetch_optional(&self.write_pool)
        .await?;

        Ok(claimed)
    }

    /// Extend the active lease on an in-flight `delivering` task (e.g. while polling Notion async import).
    pub async fn renew_task_lease(&self, task_id: i64, new_lease_until: i64) -> AppResult<bool> {
        let res = sqlx::query(
            "UPDATE delivery_tasks SET lease_until = ?1 WHERE id = ?2 AND status = 'delivering'",
        )
        .bind(new_lease_until)
        .bind(task_id)
        .execute(&self.write_pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Persist raw `checkpoint_json` mid-flight without changing task status.
    pub async fn save_task_checkpoint(&self, task_id: i64, checkpoint_json: &str) -> AppResult<()> {
        sqlx::query("UPDATE delivery_tasks SET checkpoint_json = ?1 WHERE id = ?2")
            .bind(checkpoint_json)
            .bind(task_id)
            .execute(&self.write_pool)
            .await?;
        Ok(())
    }

    /// Persist a typed [`DeliveryCheckpoint`] mid-flight without changing task status.
    pub async fn save_task_checkpoint_typed(
        &self,
        task_id: i64,
        checkpoint: &DeliveryCheckpoint,
    ) -> AppResult<()> {
        let json = serde_json::to_string(checkpoint)?;
        self.save_task_checkpoint(task_id, &json).await
    }

    /// Transition an in-flight task to terminal `delivered` state and release its lease.
    pub async fn mark_task_delivered(
        &self,
        task_id: i64,
        completed_at: i64,
        status_code: Option<i32>,
        checkpoint: Option<&DeliveryCheckpoint>,
        diagnostic_log: Option<&str>,
    ) -> AppResult<DeliveryTask> {
        let checkpoint_json = checkpoint.map(serde_json::to_string).transpose()?;
        let row = sqlx::query_as::<_, DeliveryTask>(
            r#"
            UPDATE delivery_tasks
            SET status = 'delivered',
                lease_until = NULL,
                checkpoint_json = COALESCE(?1, checkpoint_json),
                last_status_code = COALESCE(?2, last_status_code),
                diagnostic_log = ?3,
                completed_at = ?4
            WHERE id = ?5
            RETURNING *
            "#,
        )
        .bind(checkpoint_json)
        .bind(status_code)
        .bind(diagnostic_log)
        .bind(completed_at)
        .bind(task_id)
        .fetch_optional(&self.write_pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("delivery task {task_id} not found")))?;

        Ok(row)
    }

    /// Transition an in-flight task to `retrying` with updated `next_attempt_at` and checkpoint.
    pub async fn mark_task_retrying(
        &self,
        task_id: i64,
        next_attempt_at: i64,
        status_code: Option<i32>,
        checkpoint: Option<&DeliveryCheckpoint>,
        diagnostic_log: &str,
    ) -> AppResult<DeliveryTask> {
        let checkpoint_json = checkpoint.map(serde_json::to_string).transpose()?;
        let row = sqlx::query_as::<_, DeliveryTask>(
            r#"
            UPDATE delivery_tasks
            SET status = 'retrying',
                lease_until = NULL,
                next_attempt_at = ?1,
                checkpoint_json = COALESCE(?2, checkpoint_json),
                last_status_code = ?3,
                diagnostic_log = ?4
            WHERE id = ?5
            RETURNING *
            "#,
        )
        .bind(next_attempt_at)
        .bind(checkpoint_json)
        .bind(status_code)
        .bind(diagnostic_log)
        .bind(task_id)
        .fetch_optional(&self.write_pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("delivery task {task_id} not found")))?;

        Ok(row)
    }

    /// Transition a task to `deferred` when hourly/daily channel quota is reached.
    /// Decrements `attempt_count` back by 1 so quota deferral never consumes retry attempts.
    pub async fn mark_task_deferred(
        &self,
        task_id: i64,
        next_window_at: i64,
        checkpoint: Option<&DeliveryCheckpoint>,
        diagnostic_log: &str,
    ) -> AppResult<DeliveryTask> {
        let checkpoint_json = checkpoint.map(serde_json::to_string).transpose()?;
        let row = sqlx::query_as::<_, DeliveryTask>(
            r#"
            UPDATE delivery_tasks
            SET status = 'deferred',
                lease_until = NULL,
                attempt_count = MAX(attempt_count - 1, 0),
                next_attempt_at = ?1,
                checkpoint_json = COALESCE(?2, checkpoint_json),
                diagnostic_log = ?3
            WHERE id = ?4
            RETURNING *
            "#,
        )
        .bind(next_window_at)
        .bind(checkpoint_json)
        .bind(diagnostic_log)
        .bind(task_id)
        .fetch_optional(&self.write_pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("delivery task {task_id} not found")))?;

        Ok(row)
    }

    /// Transition a task to `dead_letter` after exhausting retries or hitting a permanent error.
    pub async fn mark_task_dead_letter(
        &self,
        task_id: i64,
        completed_at: i64,
        status_code: Option<i32>,
        checkpoint: Option<&DeliveryCheckpoint>,
        diagnostic_log: &str,
    ) -> AppResult<DeliveryTask> {
        let checkpoint_json = checkpoint.map(serde_json::to_string).transpose()?;
        let row = sqlx::query_as::<_, DeliveryTask>(
            r#"
            UPDATE delivery_tasks
            SET status = 'dead_letter',
                lease_until = NULL,
                checkpoint_json = COALESCE(?1, checkpoint_json),
                last_status_code = ?2,
                diagnostic_log = ?3,
                completed_at = ?4
            WHERE id = ?5
            RETURNING *
            "#,
        )
        .bind(checkpoint_json)
        .bind(status_code)
        .bind(diagnostic_log)
        .bind(completed_at)
        .bind(task_id)
        .fetch_optional(&self.write_pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("delivery task {task_id} not found")))?;

        Ok(row)
    }

    /// Fetch a delivery task by ID from `read_pool`.
    pub async fn get_delivery_task(&self, task_id: i64) -> AppResult<Option<DeliveryTask>> {
        let row = sqlx::query_as::<_, DeliveryTask>("SELECT * FROM delivery_tasks WHERE id = ?1")
            .bind(task_id)
            .fetch_optional(&self.read_pool)
            .await?;
        Ok(row)
    }

    /// List delivery tasks joined with entry/route/channel metadata for WebUI and MCP inspection.
    pub async fn list_delivery_task_details(
        &self,
        status: Option<DeliveryTaskStatus>,
        limit: i64,
    ) -> AppResult<Vec<DeliveryTaskDetail>> {
        let rows = match status {
            Some(st) => {
                sqlx::query_as::<_, DeliveryTaskDetail>(
                    r#"
                    SELECT
                        dt.id,
                        dt.route_id,
                        r.name AS route_name,
                        dt.channel_id,
                        c.name AS channel_name,
                        c.kind AS channel_kind,
                        dt.entry_id,
                        e.title AS entry_title,
                        e.canonical_url AS entry_url,
                        dt.status,
                        dt.lease_until,
                        dt.checkpoint_json,
                        dt.attempt_count,
                        dt.next_attempt_at,
                        dt.last_status_code,
                        dt.diagnostic_log,
                        dt.completed_at,
                        dt.created_at
                    FROM delivery_tasks dt
                    JOIN routes r ON r.id = dt.route_id
                    JOIN channels c ON c.id = dt.channel_id
                    JOIN entries e ON e.id = dt.entry_id
                    WHERE dt.status = ?1
                    ORDER BY dt.id DESC
                    LIMIT ?2
                    "#,
                )
                .bind(st)
                .bind(limit)
                .fetch_all(&self.read_pool)
                .await?
            }
            None => {
                sqlx::query_as::<_, DeliveryTaskDetail>(
                    r#"
                    SELECT
                        dt.id,
                        dt.route_id,
                        r.name AS route_name,
                        dt.channel_id,
                        c.name AS channel_name,
                        c.kind AS channel_kind,
                        dt.entry_id,
                        e.title AS entry_title,
                        e.canonical_url AS entry_url,
                        dt.status,
                        dt.lease_until,
                        dt.checkpoint_json,
                        dt.attempt_count,
                        dt.next_attempt_at,
                        dt.last_status_code,
                        dt.diagnostic_log,
                        dt.completed_at,
                        dt.created_at
                    FROM delivery_tasks dt
                    JOIN routes r ON r.id = dt.route_id
                    JOIN channels c ON c.id = dt.channel_id
                    JOIN entries e ON e.id = dt.entry_id
                    ORDER BY dt.id DESC
                    LIMIT ?1
                    "#,
                )
                .bind(limit)
                .fetch_all(&self.read_pool)
                .await?
            }
        };
        Ok(rows)
    }

    /// Requeue specific dead-letter/failed tasks by ID (provided their entry payload has not been purged).
    pub async fn requeue_tasks_by_ids(&self, task_ids: &[i64], now: i64) -> AppResult<u64> {
        if task_ids.is_empty() {
            return Ok(0);
        }
        let mut total = 0u64;
        for &task_id in task_ids {
            let res = sqlx::query(
                r#"
                UPDATE delivery_tasks
                SET status = 'queued',
                    lease_until = NULL,
                    attempt_count = 0,
                    next_attempt_at = ?1,
                    completed_at = NULL
                WHERE id = ?2
                  AND status IN ('dead_letter', 'retrying', 'deferred')
                  AND EXISTS (
                      SELECT 1 FROM entries e
                      WHERE e.id = delivery_tasks.entry_id
                        AND e.payload_purged_at IS NULL
                  )
                "#,
            )
            .bind(now)
            .bind(task_id)
            .execute(&self.write_pool)
            .await?;
            total += res.rows_affected();
        }
        Ok(total)
    }

    /// Requeue all `dead_letter` tasks whose entry payloads are still within retention.
    pub async fn requeue_all_dead_letter_tasks(&self, now: i64) -> AppResult<u64> {
        let res = sqlx::query(
            r#"
            UPDATE delivery_tasks
            SET status = 'queued',
                lease_until = NULL,
                attempt_count = 0,
                next_attempt_at = ?1,
                completed_at = NULL
            WHERE status = 'dead_letter'
              AND EXISTS (
                  SELECT 1 FROM entries e
                  WHERE e.id = delivery_tasks.entry_id
                    AND e.payload_purged_at IS NULL
              )
            "#,
        )
        .bind(now)
        .execute(&self.write_pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Compute rolling 1-hour (`3600s`) and 24-hour (`86400s`) delivered counts for both the
    /// individual `channel_id` and its shared `credential_group_key`, plus the latest completion timestamp.
    pub async fn get_channel_quota_usage(
        &self,
        channel_id: i64,
        credential_group_key: &str,
        now: i64,
    ) -> AppResult<QuotaUsage> {
        let hour_cutoff = now.saturating_sub(3600);
        let day_cutoff = now.saturating_sub(86_400);

        let (channel_last_hour,): (i64,) = sqlx::query_as(
            r#"
            SELECT COUNT(*)
            FROM delivery_tasks
            WHERE channel_id = ?1
              AND status = 'delivered'
              AND completed_at >= ?2
            "#,
        )
        .bind(channel_id)
        .bind(hour_cutoff)
        .fetch_one(&self.read_pool)
        .await?;

        let (channel_last_day,): (i64,) = sqlx::query_as(
            r#"
            SELECT COUNT(*)
            FROM delivery_tasks
            WHERE channel_id = ?1
              AND status = 'delivered'
              AND completed_at >= ?2
            "#,
        )
        .bind(channel_id)
        .bind(day_cutoff)
        .fetch_one(&self.read_pool)
        .await?;

        let (credential_group_last_hour, credential_group_last_day, last_completed_at): (
            i64,
            i64,
            Option<i64>,
        ) = sqlx::query_as(
            r#"
            SELECT
                COALESCE(SUM(CASE WHEN dt.completed_at >= ?2 THEN 1 ELSE 0 END), 0),
                COUNT(*),
                MAX(dt.completed_at)
            FROM delivery_tasks dt
            JOIN channels c ON c.id = dt.channel_id
            WHERE c.credential_group_key = ?1
              AND dt.status = 'delivered'
              AND dt.completed_at >= ?3
            "#,
        )
        .bind(credential_group_key)
        .bind(hour_cutoff)
        .bind(day_cutoff)
        .fetch_one(&self.read_pool)
        .await?;

        Ok(QuotaUsage {
            channel_last_hour,
            channel_last_day,
            credential_group_last_hour,
            credential_group_last_day,
            last_completed_at,
        })
    }

    // =========================================================================
    // Payload Garbage Collection (`NOT EXISTS`), `PRAGMA optimize` & `VACUUM INTO`
    // =========================================================================

    /// Zero-refcount `NOT EXISTS` payload garbage collector (`feedcourier.md` Section 三.2.2).
    ///
    /// Purges transient article body & media fields (`title`, `canonical_url`, `author`,
    /// `content_html`, `first_sentence`, `summary_text`, `media_json`) while permanently keeping
    /// the lightweight `(feed_id, fingerprint_sha256)` row for entries where:
    /// 1. `payload_purged_at IS NULL`,
    /// 2. `discovered_at <= now - retention_secs`, AND
    /// 3. `NOT EXISTS` any associated `delivery_tasks` in non-terminal states
    ///    (`queued`, `delivering`, `retrying`, `deferred`).
    ///
    /// Naturally covers delivered tasks, unrouted entries, and entries whose routes were deleted.
    pub async fn purge_expired_entry_payloads(
        &self,
        now: i64,
        retention_secs: i64,
    ) -> AppResult<u64> {
        let cutoff = now.saturating_sub(retention_secs.max(0));

        let res = sqlx::query(
            r#"
            UPDATE entries
            SET title = NULL,
                canonical_url = NULL,
                author = NULL,
                content_html = NULL,
                first_sentence = NULL,
                summary_text = NULL,
                media_json = NULL,
                payload_purged_at = ?1
            WHERE payload_purged_at IS NULL
              AND discovered_at <= ?2
              AND NOT EXISTS (
                  SELECT 1
                  FROM delivery_tasks dt
                  WHERE dt.entry_id = entries.id
                    AND dt.status IN ('queued', 'delivering', 'retrying', 'deferred')
              )
            "#,
        )
        .bind(now)
        .bind(cutoff)
        .execute(&self.write_pool)
        .await?;

        Ok(res.rows_affected())
    }

    /// Run `PRAGMA optimize` on the writer connection to refresh query planner statistics.
    pub async fn run_pragma_optimize(&self) -> AppResult<()> {
        sqlx::query("PRAGMA optimize")
            .execute(&self.write_pool)
            .await?;
        Ok(())
    }

    /// Create a consistent, zero-downtime online SQLite snapshot at `dest_path` using `VACUUM INTO`.
    pub async fn vacuum_into(&self, dest_path: &Path) -> AppResult<()> {
        if dest_path == self.db_path() {
            return Err(AppError::Validation(
                "backup destination path must not equal the live database path".to_string(),
            ));
        }

        if let Some(parent) = dest_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }

        if dest_path.exists() {
            fs::remove_file(dest_path)?;
        }

        let dest_str = dest_path.to_str().ok_or_else(|| {
            AppError::Validation("backup destination path must be valid UTF-8".to_string())
        })?;

        sqlx::query("VACUUM INTO ?1")
            .bind(dest_str)
            .execute(&self.write_pool)
            .await?;
        Ok(())
    }

    // =========================================================================
    // Dashboard Overview Snapshot
    // =========================================================================

    /// Collect aggregate health, fingerprint, and outbox task counts for `GET /api/v1/overview`.
    pub async fn get_overview_snapshot(&self) -> AppResult<OverviewSnapshot> {
        let feed_rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT health_status, COUNT(*) FROM feeds GROUP BY health_status")
                .fetch_all(&self.read_pool)
                .await?;

        let (entries_total_fingerprints, entries_active_payloads): (i64, i64) = sqlx::query_as(
            r#"
            SELECT
                COUNT(*),
                COALESCE(SUM(CASE WHEN payload_purged_at IS NULL THEN 1 ELSE 0 END), 0)
            FROM entries
            "#,
        )
        .fetch_one(&self.read_pool)
        .await?;

        let task_rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT status, COUNT(*) FROM delivery_tasks GROUP BY status")
                .fetch_all(&self.read_pool)
                .await?;

        let mut snap = OverviewSnapshot {
            entries_total_fingerprints,
            entries_active_payloads,
            ..OverviewSnapshot::default()
        };

        for (status, count) in feed_rows {
            match status.as_str() {
                "active" => snap.feeds_active = count,
                "backoff" => snap.feeds_backoff = count,
                "circuit_open" => snap.feeds_circuit_open = count,
                "paused" => snap.feeds_paused = count,
                _ => {}
            }
        }

        for (status, count) in task_rows {
            match status.as_str() {
                "queued" => snap.tasks_queued = count,
                "delivering" => snap.tasks_delivering = count,
                "retrying" => snap.tasks_retrying = count,
                "deferred" => snap.tasks_deferred = count,
                "dead_letter" => snap.tasks_dead_letter = count,
                "delivered" => snap.tasks_delivered = count,
                _ => {}
            }
        }

        Ok(snap)
    }

    // =========================================================================
    // System Settings, MCP Bearer Tokens & Admin Sessions
    // =========================================================================

    /// Read raw JSON setting row by `key`.
    pub async fn get_setting(&self, key: &str) -> AppResult<Option<SystemSetting>> {
        let row =
            sqlx::query_as::<_, SystemSetting>("SELECT * FROM system_settings WHERE key = ?1")
                .bind(key)
                .fetch_optional(&self.read_pool)
                .await?;
        Ok(row)
    }

    /// Read and deserialize a typed JSON setting by `key`.
    pub async fn get_setting_typed<T: DeserializeOwned>(&self, key: &str) -> AppResult<Option<T>> {
        match self.get_setting(key).await? {
            Some(row) => Ok(Some(serde_json::from_str(&row.value_json)?)),
            None => Ok(None),
        }
    }

    /// Upsert a typed JSON setting into `system_settings`.
    pub async fn set_setting_typed<T: Serialize>(
        &self,
        key: &str,
        value: &T,
    ) -> AppResult<SystemSetting> {
        let now = now_unix();
        let value_json = serde_json::to_string(value)?;
        let row = sqlx::query_as::<_, SystemSetting>(
            r#"
            INSERT INTO system_settings (key, value_json, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT (key) DO UPDATE SET
                value_json = excluded.value_json,
                updated_at = excluded.updated_at
            RETURNING *
            "#,
        )
        .bind(key)
        .bind(value_json)
        .bind(now)
        .fetch_one(&self.write_pool)
        .await?;
        Ok(row)
    }

    /// Delete a setting by `key`.
    pub async fn delete_setting(&self, key: &str) -> AppResult<bool> {
        let res = sqlx::query("DELETE FROM system_settings WHERE key = ?1")
            .bind(key)
            .execute(&self.write_pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Insert a hashed MCP bearer token (`readonly` or `admin`).
    pub async fn create_mcp_token(
        &self,
        name: &str,
        token_hash: &str,
        token_prefix: &str,
        scope: McpTokenScope,
    ) -> AppResult<McpToken> {
        let now = now_unix();
        let row = sqlx::query_as::<_, McpToken>(
            r#"
            INSERT INTO mcp_tokens (name, token_hash, token_prefix, scope, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            RETURNING *
            "#,
        )
        .bind(name.trim())
        .bind(token_hash)
        .bind(token_prefix)
        .bind(scope)
        .bind(now)
        .fetch_one(&self.write_pool)
        .await?;
        Ok(row)
    }

    /// List all MCP token metadata ordered by `id DESC` (never exposes plaintext token).
    pub async fn list_mcp_tokens(&self) -> AppResult<Vec<McpToken>> {
        let rows = sqlx::query_as::<_, McpToken>("SELECT * FROM mcp_tokens ORDER BY id DESC")
            .fetch_all(&self.read_pool)
            .await?;
        Ok(rows)
    }

    /// Lookup an MCP token by its SHA-256 hex digest.
    pub async fn get_mcp_token_by_hash(&self, token_hash: &str) -> AppResult<Option<McpToken>> {
        let row = sqlx::query_as::<_, McpToken>("SELECT * FROM mcp_tokens WHERE token_hash = ?1")
            .bind(token_hash)
            .fetch_optional(&self.read_pool)
            .await?;
        Ok(row)
    }

    /// Record MCP token usage timestamp.
    pub async fn touch_mcp_token_last_used(&self, token_id: i64, now: i64) -> AppResult<()> {
        sqlx::query("UPDATE mcp_tokens SET last_used_at = ?1 WHERE id = ?2")
            .bind(now)
            .bind(token_id)
            .execute(&self.write_pool)
            .await?;
        Ok(())
    }

    /// Revoke/delete an MCP token by ID.
    pub async fn delete_mcp_token(&self, token_id: i64) -> AppResult<bool> {
        let res = sqlx::query("DELETE FROM mcp_tokens WHERE id = ?1")
            .bind(token_id)
            .execute(&self.write_pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Create an HttpOnly WebUI administrator session.
    pub async fn create_session(&self, token_hash: &str, expires_at: i64) -> AppResult<Session> {
        let now = now_unix();
        let row = sqlx::query_as::<_, Session>(
            r#"
            INSERT INTO sessions (token_hash, expires_at, created_at)
            VALUES (?1, ?2, ?3)
            RETURNING *
            "#,
        )
        .bind(token_hash)
        .bind(expires_at)
        .bind(now)
        .fetch_one(&self.write_pool)
        .await?;
        Ok(row)
    }

    /// Fetch an active (unexpired) session by its SHA-256 hex digest.
    pub async fn get_valid_session(
        &self,
        token_hash: &str,
        now: i64,
    ) -> AppResult<Option<Session>> {
        let row = sqlx::query_as::<_, Session>(
            "SELECT * FROM sessions WHERE token_hash = ?1 AND expires_at > ?2",
        )
        .bind(token_hash)
        .bind(now)
        .fetch_optional(&self.read_pool)
        .await?;
        Ok(row)
    }

    /// Delete a session on logout.
    pub async fn delete_session(&self, token_hash: &str) -> AppResult<bool> {
        let res = sqlx::query("DELETE FROM sessions WHERE token_hash = ?1")
            .bind(token_hash)
            .execute(&self.write_pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Purge expired WebUI sessions (`expires_at <= now`).
    pub async fn purge_expired_sessions(&self, now: i64) -> AppResult<u64> {
        let res = sqlx::query("DELETE FROM sessions WHERE expires_at <= ?1")
            .bind(now)
            .execute(&self.write_pool)
            .await?;
        Ok(res.rows_affected())
    }
}
