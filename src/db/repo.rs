//! Full CRUD, outbox lease state machine, quota counters, `NOT EXISTS` payload GC,
//! `PRAGMA optimize`, and `VACUUM INTO` online snapshot backup operations.

use std::fs;
use std::path::{Path, PathBuf};

use chrono_tz::Tz;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::channels::{
    ChannelConfig, ChannelKind, DeliveryCheckpoint, DestinationConfig, LegacyChannelConfig,
    TelegramCapabilities,
};
use crate::crypto::MasterKey;
use crate::db::models::{
    CHANNEL_MODEL_VERSION_V2, Channel, ChannelDestination, DeliveryTask, DeliveryTaskDetail,
    DeliveryTaskStatus, Entry, EntryInsertOutcome, Feed, FeedHealthStatus, GroupEffectiveLimits,
    McpToken, McpTokenScope, NewChannel, NewDestination, NewEntry, NewFeed, NewRoute,
    OverviewSnapshot, QuotaUsage, Route, SETTING_CHANNEL_MODEL_VERSION, SETTING_OUTBOX_QUEUE,
    SETTING_TIMEZONE, Session, SystemSetting, UpdateChannel, UpdateDestination, UpdateFeed,
    UpdateRoute, now_unix,
};
use crate::db::{Database, choose_pre_v2_backup_path};
use crate::error::{AppError, AppResult};
use crate::schedule::{DEFAULT_TIMEZONE, OutboxQueueState};

/// Summary returned by [`Database::migrate_channels_to_v2`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelMigrationReport {
    pub migrated: bool,
    pub legacy_channels_scanned: usize,
    pub accounts_surviving: usize,
    pub accounts_merged: usize,
    pub destinations_created: usize,
    pub backup_path: Option<PathBuf>,
}

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

        let next_poll_at = if patch.health_status == Some(FeedHealthStatus::Active)
            && current.health_status != FeedHealthStatus::Active
        {
            Some(now)
        } else if patch.poll_interval_secs.is_some() {
            let max_next = now.saturating_add(poll_interval_secs);
            match current.next_poll_at {
                Some(existing) if existing > max_next => Some(max_next),
                other => other,
            }
        } else {
            current.next_poll_at
        };

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
                next_poll_at = ?11,
                updated_at = ?12
            WHERE id = ?13
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
        .bind(next_poll_at)
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

    /// Remove all cached state of a feed so the next poll re-ingests (and re-delivers) its full
    /// backlog: deletes every `entries` row (cascading `delivery_tasks`), clears HTTP validators
    /// and failure state, and disables `silent_init`. Returns the number of deleted entries.
    pub async fn clear_feed_cache(&self, feed_id: i64) -> AppResult<u64> {
        let mut tx = self.write_pool.begin().await?;
        let exists: Option<(i64,)> = sqlx::query_as("SELECT id FROM feeds WHERE id = ?1")
            .bind(feed_id)
            .fetch_optional(&mut *tx)
            .await?;
        if exists.is_none() {
            return Err(AppError::NotFound(format!("feed {feed_id} not found")));
        }
        let deleted = sqlx::query("DELETE FROM entries WHERE feed_id = ?1")
            .bind(feed_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        let now = now_unix();
        sqlx::query(
            r#"
            UPDATE feeds
            SET etag = NULL,
                last_modified = NULL,
                consecutive_failures = 0,
                last_error = NULL,
                silent_init = 0,
                health_status = CASE WHEN health_status = 'paused' THEN 'paused' ELSE 'active' END,
                next_poll_at = CASE WHEN health_status = 'paused' THEN next_poll_at ELSE ?1 END,
                updated_at = ?1
            WHERE id = ?2
            "#,
        )
        .bind(now)
        .bind(feed_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(deleted)
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

    /// Delete a channel (cascading its `channel_destinations`, `routes`, and `delivery_tasks`).
    pub async fn delete_channel(&self, channel_id: i64) -> AppResult<bool> {
        let res = sqlx::query("DELETE FROM channels WHERE id = ?1")
            .bind(channel_id)
            .execute(&self.write_pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    // =========================================================================
    // Channel Destinations CRUD & Target Resolution (v0.2.0 Two-Layer Model)
    // =========================================================================

    /// Create a destination attached to an existing parent `channel_id`.
    pub async fn create_destination(
        &self,
        input: &NewDestination,
    ) -> AppResult<ChannelDestination> {
        let now = now_unix();
        let row = sqlx::query_as::<_, ChannelDestination>(
            r#"
            INSERT INTO channel_destinations (
                channel_id, name, encrypted_config, enabled, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?5)
            RETURNING *
            "#,
        )
        .bind(input.channel_id)
        .bind(input.name.trim())
        .bind(&input.encrypted_config)
        .bind(input.enabled)
        .bind(now)
        .fetch_one(&self.write_pool)
        .await?;
        Ok(row)
    }

    /// Fetch a destination by ID from `read_pool`.
    pub async fn get_destination(
        &self,
        destination_id: i64,
    ) -> AppResult<Option<ChannelDestination>> {
        let row = sqlx::query_as::<_, ChannelDestination>(
            "SELECT * FROM channel_destinations WHERE id = ?1",
        )
        .bind(destination_id)
        .fetch_optional(&self.read_pool)
        .await?;
        Ok(row)
    }

    /// List all destinations ordered by `channel_id ASC, id ASC`.
    pub async fn list_destinations(&self) -> AppResult<Vec<ChannelDestination>> {
        let rows = sqlx::query_as::<_, ChannelDestination>(
            "SELECT * FROM channel_destinations ORDER BY channel_id ASC, id ASC",
        )
        .fetch_all(&self.read_pool)
        .await?;
        Ok(rows)
    }

    /// List all destinations attached to a single `channel_id` ordered by `id ASC`.
    pub async fn list_destinations_for_channel(
        &self,
        channel_id: i64,
    ) -> AppResult<Vec<ChannelDestination>> {
        let rows = sqlx::query_as::<_, ChannelDestination>(
            "SELECT * FROM channel_destinations WHERE channel_id = ?1 ORDER BY id ASC",
        )
        .bind(channel_id)
        .fetch_all(&self.read_pool)
        .await?;
        Ok(rows)
    }

    /// Apply partial updates to a destination serially on `write_pool`.
    pub async fn update_destination(
        &self,
        destination_id: i64,
        patch: &UpdateDestination,
    ) -> AppResult<ChannelDestination> {
        let current = sqlx::query_as::<_, ChannelDestination>(
            "SELECT * FROM channel_destinations WHERE id = ?1",
        )
        .bind(destination_id)
        .fetch_optional(&self.write_pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("destination {destination_id} not found")))?;

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
        let enabled = patch.enabled.unwrap_or(current.enabled);
        let now = now_unix();

        let updated = sqlx::query_as::<_, ChannelDestination>(
            r#"
            UPDATE channel_destinations
            SET name = ?1,
                encrypted_config = ?2,
                enabled = ?3,
                updated_at = ?4
            WHERE id = ?5
            RETURNING *
            "#,
        )
        .bind(name)
        .bind(encrypted_config)
        .bind(enabled)
        .bind(now)
        .bind(destination_id)
        .fetch_one(&self.write_pool)
        .await?;
        Ok(updated)
    }

    /// Delete a destination (cascading its `routes` and `delivery_tasks`).
    pub async fn delete_destination(&self, destination_id: i64) -> AppResult<bool> {
        let res = sqlx::query("DELETE FROM channel_destinations WHERE id = ?1")
            .bind(destination_id)
            .execute(&self.write_pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Resolve a target `(Channel, ChannelDestination)` from either `destination_id` (primary) or
    /// legacy `channel_id` (backward-compatible fallback when that channel owns exactly 1 destination).
    pub async fn resolve_destination_target(
        &self,
        destination_id: Option<i64>,
        channel_id: Option<i64>,
    ) -> AppResult<(Channel, ChannelDestination)> {
        if let Some(did) = destination_id {
            let dest = self
                .get_destination(did)
                .await?
                .ok_or_else(|| AppError::NotFound(format!("destination {did} not found")))?;
            if let Some(cid) = channel_id
                && cid != dest.channel_id
            {
                return Err(AppError::Validation(format!(
                    "destination {did} belongs to channel {}, not channel {cid}",
                    dest.channel_id
                )));
            }
            let channel = self.get_channel(dest.channel_id).await?.ok_or_else(|| {
                AppError::NotFound(format!("channel {} not found", dest.channel_id))
            })?;
            return Ok((channel, dest));
        }

        if let Some(cid) = channel_id {
            let channel = self
                .get_channel(cid)
                .await?
                .ok_or_else(|| AppError::NotFound(format!("channel {cid} not found")))?;
            let mut dests = self.list_destinations_for_channel(cid).await?;
            return match dests.len() {
                1 => Ok((channel, dests.remove(0))),
                0 => Err(AppError::Validation(format!(
                    "channel {cid} has no destinations configured; add a destination first"
                ))),
                n => Err(AppError::Validation(format!(
                    "channel {cid} has {n} destinations; specify destination_id explicitly"
                ))),
            };
        }

        Err(AppError::Validation(
            "either destination_id or channel_id is required".to_string(),
        ))
    }

    /// Automatically and atomically migrate pre-v0.2.0 single-layer `channels` rows into the
    /// v0.2.0 two-layer (`channels` Account + `channel_destinations`) model.
    ///
    /// - No-op when `system_settings.channel_model_version >= 2`.
    /// - Takes a consistent `VACUUM INTO` backup at `<db_dir>/pre-v0.2.0-backup.db` before
    ///   mutating legacy data.
    /// - Splits each legacy channel into account credentials + destination config, merges rows
    ///   sharing the same `credential_group_key` (keeping the strictest pacing/quotas and
    ///   unioning `use_proxy` / `enabled`), repoints all `routes` and `delivery_tasks` (including
    ///   `delivered` and `dead_letter`), deletes absorbed duplicate rows, and records
    ///   `channel_model_version = 2` inside a single SQLite transaction.
    pub async fn migrate_channels_to_v2(
        &self,
        master_key: &MasterKey,
    ) -> AppResult<ChannelMigrationReport> {
        if let Some(ver) = self
            .get_setting_typed::<i64>(SETTING_CHANNEL_MODEL_VERSION)
            .await?
            && ver >= CHANNEL_MODEL_VERSION_V2
        {
            return Ok(ChannelMigrationReport::default());
        }

        let existing_channels = self.list_channels().await?;
        if existing_channels.is_empty() {
            self.set_setting_typed(SETTING_CHANNEL_MODEL_VERSION, &CHANNEL_MODEL_VERSION_V2)
                .await?;
            return Ok(ChannelMigrationReport::default());
        }

        struct DecryptedLegacy {
            row: Channel,
            account_cfg: ChannelConfig,
            dest_cfg: DestinationConfig,
            group_key: String,
            merge_key: String,
        }

        let mut legacy_rows = Vec::with_capacity(existing_channels.len());
        for row in existing_channels {
            match master_key.decrypt_json::<LegacyChannelConfig>(&row.encrypted_config) {
                Ok(legacy_cfg) => {
                    let (account_cfg, dest_cfg) = legacy_cfg.into_split();
                    let group_key = account_cfg.credential_group_key();
                    let merge_key = account_cfg.migration_merge_key();
                    legacy_rows.push(DecryptedLegacy {
                        row,
                        account_cfg,
                        dest_cfg,
                        group_key,
                        merge_key,
                    });
                }
                Err(legacy_err) => {
                    // If this row is already stored in v2 account-only format, skip it cleanly;
                    // otherwise propagate the decryption failure so we never silently lose data.
                    if master_key
                        .decrypt_json::<ChannelConfig>(&row.encrypted_config)
                        .is_ok()
                    {
                        continue;
                    }
                    return Err(legacy_err);
                }
            }
        }

        if legacy_rows.is_empty() {
            self.set_setting_typed(SETTING_CHANNEL_MODEL_VERSION, &CHANNEL_MODEL_VERSION_V2)
                .await?;
            return Ok(ChannelMigrationReport::default());
        }

        // Reuse the pre-DDL snapshot captured in `Database::connect` when upgrading from v0.1.x;
        // otherwise take a non-overwriting snapshot right before mutating legacy rows.
        let backup_path = if let Some(pre_ddl) = self.pre_v2_backup_path() {
            pre_ddl.to_path_buf()
        } else {
            let path = choose_pre_v2_backup_path(self.db_path());
            self.vacuum_into(&path).await?;
            path
        };

        // Group legacy rows strictly by (kind, migration_merge_key) while preserving ID order so
        // SMTP channels with distinct passwords, security modes, or `from_address` never merge.
        let mut groups: Vec<((ChannelKind, String), Vec<DecryptedLegacy>)> = Vec::new();
        for item in legacy_rows {
            let key = (item.row.kind, item.merge_key.clone());
            if let Some((_, bucket)) = groups.iter_mut().find(|(k, _)| *k == key) {
                bucket.push(item);
            } else {
                groups.push((key, vec![item]));
            }
        }

        let now = now_unix();
        let day_cutoff = now.saturating_sub(86_400);
        let legacy_channels_scanned: usize = groups.iter().map(|(_, g)| g.len()).sum();
        let accounts_surviving = groups.len();
        let accounts_merged = legacy_channels_scanned.saturating_sub(accounts_surviving);
        let mut destinations_created = 0usize;

        let mut tx = self.write_pool.begin().await?;

        for (_, group) in groups {
            let surviving = &group[0];
            let surviving_id = surviving.row.id;

            // Aggregate quotas/pacing/proxy from enabled rows only (falling back to all rows only
            // when every legacy row in the group was disabled) so disabled channels never drag down
            // active quotas or unexpectedly flip `use_proxy`.
            let enabled_rows: Vec<&DecryptedLegacy> =
                group.iter().filter(|g| g.row.enabled).collect();
            let active_rows: Vec<&DecryptedLegacy> = if enabled_rows.is_empty() {
                group.iter().collect()
            } else {
                enabled_rows
            };

            let min_interval_secs = active_rows
                .iter()
                .map(|g| g.row.min_interval_secs)
                .max()
                .unwrap_or(15);
            let jitter_max_secs = active_rows
                .iter()
                .map(|g| g.row.jitter_max_secs)
                .max()
                .unwrap_or(10);
            let hourly_quota = active_rows.iter().filter_map(|g| g.row.hourly_quota).min();
            let daily_quota = active_rows.iter().filter_map(|g| g.row.daily_quota).min();
            let use_proxy = active_rows.iter().any(|g| g.row.use_proxy);
            let enabled = group.iter().any(|g| g.row.enabled);
            let cooldown_until = active_rows
                .iter()
                .filter_map(|g| g.row.cooldown_until)
                .max();

            let capabilities_json = group
                .iter()
                .find_map(|g| {
                    let trimmed = g.row.capabilities_json.trim();
                    if !trimmed.is_empty() && trimmed != "{}" {
                        Some(trimmed.to_string())
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| "{}".to_string());

            let account_name = match &surviving.account_cfg {
                ChannelConfig::Smtp(smtp) if !smtp.from_address.trim().is_empty() => {
                    smtp.from_address.trim().to_string()
                }
                ChannelConfig::Telegram(_) => {
                    serde_json::from_str::<TelegramCapabilities>(&capabilities_json)
                        .ok()
                        .and_then(|caps| {
                            caps.bot_username
                                .as_deref()
                                .map(str::trim)
                                .filter(|u| !u.is_empty())
                                .map(|u| format!("@{}", u.trim_start_matches('@')))
                                .or_else(|| {
                                    caps.bot_first_name
                                        .as_deref()
                                        .map(str::trim)
                                        .filter(|n| !n.is_empty())
                                        .map(ToOwned::to_owned)
                                })
                        })
                        .unwrap_or_else(|| surviving.row.name.clone())
                }
                _ => surviving.row.name.clone(),
            };

            let enc_account = master_key.encrypt_json(&surviving.account_cfg)?;
            sqlx::query(
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
                "#,
            )
            .bind(&account_name)
            .bind(&enc_account)
            .bind(&surviving.group_key)
            .bind(&capabilities_json)
            .bind(min_interval_secs)
            .bind(jitter_max_secs)
            .bind(hourly_quota)
            .bind(daily_quota)
            .bind(use_proxy)
            .bind(cooldown_until)
            .bind(enabled)
            .bind(now)
            .bind(surviving_id)
            .execute(&mut *tx)
            .await?;

            for item in &group {
                let enc_dest = master_key.encrypt_json(&item.dest_cfg)?;
                let (dest_id,): (i64,) = sqlx::query_as(
                    r#"
                    INSERT INTO channel_destinations (
                        channel_id, name, encrypted_config, enabled, created_at, updated_at
                    )
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                    RETURNING id
                    "#,
                )
                .bind(surviving_id)
                .bind(item.row.name.trim())
                .bind(&enc_dest)
                .bind(item.row.enabled)
                .bind(item.row.created_at)
                .bind(now)
                .fetch_one(&mut *tx)
                .await?;
                destinations_created += 1;

                // Repoint routes and historical/active delivery tasks from this legacy channel.
                // Filtering `destination_id IS NULL` prevents touching rows already repointed
                // onto `surviving_id` for an earlier sibling in the same merged group.
                sqlx::query(
                    r#"
                    UPDATE routes
                    SET channel_id = ?1,
                        destination_id = ?2,
                        updated_at = ?3
                    WHERE channel_id = ?4
                      AND destination_id IS NULL
                    "#,
                )
                .bind(surviving_id)
                .bind(dest_id)
                .bind(now)
                .bind(item.row.id)
                .execute(&mut *tx)
                .await?;

                sqlx::query(
                    r#"
                    UPDATE delivery_tasks
                    SET channel_id = ?1,
                        destination_id = ?2
                    WHERE channel_id = ?3
                      AND destination_id IS NULL
                    "#,
                )
                .bind(surviving_id)
                .bind(dest_id)
                .bind(item.row.id)
                .execute(&mut *tx)
                .await?;

                // Align any existing quota ledger entries to the surviving account and updated key.
                sqlx::query(
                    r#"
                    UPDATE delivery_quota_ledger
                    SET channel_id = ?1,
                        credential_group_key = ?2
                    WHERE channel_id = ?3
                    "#,
                )
                .bind(surviving_id)
                .bind(&surviving.group_key)
                .bind(item.row.id)
                .execute(&mut *tx)
                .await?;
            }

            // Backfill recent 24h delivered tasks into `delivery_quota_ledger` when not yet seeded.
            let (existing_ledger_rows,): (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM delivery_quota_ledger WHERE channel_id = ?1")
                    .bind(surviving_id)
                    .fetch_one(&mut *tx)
                    .await?;
            if existing_ledger_rows == 0 {
                sqlx::query(
                    r#"
                    INSERT INTO delivery_quota_ledger (credential_group_key, channel_id, completed_at)
                    SELECT ?1, dt.channel_id, dt.completed_at
                    FROM delivery_tasks dt
                    WHERE dt.channel_id = ?2
                      AND dt.status = 'delivered'
                      AND dt.completed_at IS NOT NULL
                      AND dt.completed_at >= ?3
                    "#,
                )
                .bind(&surviving.group_key)
                .bind(surviving_id)
                .bind(day_cutoff)
                .execute(&mut *tx)
                .await?;
            }

            // Delete absorbed duplicate channel rows after all references have been repointed.
            for item in group.iter().skip(1) {
                sqlx::query("DELETE FROM channels WHERE id = ?1")
                    .bind(item.row.id)
                    .execute(&mut *tx)
                    .await?;
            }
        }

        // Strict integrity assertion: no route or delivery task may remain with NULL destination_id.
        let (orphan_routes,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM routes WHERE destination_id IS NULL")
                .fetch_one(&mut *tx)
                .await?;
        let (orphan_tasks,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM delivery_tasks WHERE destination_id IS NULL")
                .fetch_one(&mut *tx)
                .await?;
        if orphan_routes > 0 || orphan_tasks > 0 {
            return Err(AppError::Internal(anyhow::anyhow!(
                "v0.2.0 channel migration aborted: {orphan_routes} route(s) and {orphan_tasks} delivery_task(s) still have NULL destination_id"
            )));
        }

        let ver_json = serde_json::to_string(&CHANNEL_MODEL_VERSION_V2)?;
        sqlx::query(
            r#"
            INSERT INTO system_settings (key, value_json, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT (key) DO UPDATE SET
                value_json = excluded.value_json,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(SETTING_CHANNEL_MODEL_VERSION)
        .bind(ver_json)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        tracing::info!(
            legacy_channels_scanned,
            accounts_surviving,
            accounts_merged,
            destinations_created,
            backup_path = %backup_path.display(),
            "migrated legacy delivery channels to v0.2.0 two-layer Account + Destinations model"
        );

        Ok(ChannelMigrationReport {
            migrated: true,
            legacy_channels_scanned,
            accounts_surviving,
            accounts_merged,
            destinations_created,
            backup_path: Some(backup_path),
        })
    }

    // =========================================================================
    // Routes CRUD
    // =========================================================================

    /// Create a routing rule linking feeds/tags to a trusted channel destination.
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
                name, feed_ids_json, match_tags_json, channel_id, destination_id,
                include_keywords_json, exclude_keywords_json,
                include_regex, exclude_regex, options_json, enabled,
                created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
            RETURNING *
            "#,
        )
        .bind(input.name.trim())
        .bind(feed_ids_json)
        .bind(match_tags_json)
        .bind(input.channel_id)
        .bind(input.destination_id)
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

    /// List enabled routes whose parent channel account AND target destination are both enabled.
    pub async fn list_enabled_routes(&self) -> AppResult<Vec<Route>> {
        let rows = sqlx::query_as::<_, Route>(
            r#"
            SELECT r.*
            FROM routes r
            JOIN channels c ON c.id = r.channel_id
            JOIN channel_destinations cd ON cd.id = r.destination_id
            WHERE r.enabled = 1 AND c.enabled = 1 AND cd.enabled = 1
            ORDER BY r.id ASC
            "#,
        )
        .fetch_all(&self.read_pool)
        .await?;
        Ok(rows)
    }

    /// Apply partial updates to a route serially on `write_pool`, atomically keeping in-flight
    /// non-delivering outbox tasks synchronized when the route's target destination or delivery
    /// schedule changes.
    pub async fn update_route(&self, route_id: i64, patch: &UpdateRoute) -> AppResult<Route> {
        let mut tx = self.write_pool.begin().await?;

        let current = sqlx::query_as::<_, Route>("SELECT * FROM routes WHERE id = ?1")
            .bind(route_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("route {route_id} not found")))?;

        let old_options = current.options();
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
        let destination_id = patch.destination_id.unwrap_or(current.destination_id);
        let target_changed =
            channel_id != current.channel_id || destination_id != current.destination_id;
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
        let schedule_changed = patch.options.as_ref().is_some_and(|new_opts| {
            new_opts.timezone != old_options.timezone
                || new_opts.active_windows != old_options.active_windows
                || new_opts.active_weekdays != old_options.active_weekdays
        });
        let options_json = match &patch.options {
            Some(opts) => serde_json::to_string(opts)?,
            None => current.options_json,
        };
        let enabled = patch.enabled.unwrap_or(current.enabled);
        let re_enabled = enabled && !current.enabled;
        let now = now_unix();

        let updated = sqlx::query_as::<_, Route>(
            r#"
            UPDATE routes
            SET name = ?1,
                feed_ids_json = ?2,
                match_tags_json = ?3,
                channel_id = ?4,
                destination_id = ?5,
                include_keywords_json = ?6,
                exclude_keywords_json = ?7,
                include_regex = ?8,
                exclude_regex = ?9,
                options_json = ?10,
                enabled = ?11,
                updated_at = ?12
            WHERE id = ?13
            RETURNING *
            "#,
        )
        .bind(name)
        .bind(feed_ids_json)
        .bind(match_tags_json)
        .bind(channel_id)
        .bind(destination_id)
        .bind(include_keywords_json)
        .bind(exclude_keywords_json)
        .bind(include_regex)
        .bind(exclude_regex)
        .bind(options_json)
        .bind(enabled)
        .bind(now)
        .bind(route_id)
        .fetch_one(&mut *tx)
        .await?;

        if target_changed {
            // Repoint queued/deferred/retrying tasks to the new destination and clear any
            // destination-specific checkpoints/retry counters; leave currently `delivering` tasks
            // untouched so active leases finish cleanly.
            sqlx::query(
                r#"
                UPDATE delivery_tasks
                SET channel_id = ?1,
                    destination_id = ?2,
                    checkpoint_json = NULL,
                    attempt_count = 0,
                    last_status_code = NULL,
                    next_attempt_at = ?3
                WHERE route_id = ?4
                  AND status IN ('queued', 'deferred', 'retrying')
                "#,
            )
            .bind(channel_id)
            .bind(destination_id)
            .bind(now)
            .bind(route_id)
            .execute(&mut *tx)
            .await?;
        } else if schedule_changed || re_enabled {
            // Pull forward deferred/queued tasks so the new schedule or re-enabled route is
            // re-evaluated immediately on the next outbox wakeup.
            sqlx::query(
                r#"
                UPDATE delivery_tasks
                SET next_attempt_at = ?1
                WHERE route_id = ?2
                  AND status IN ('queued', 'deferred')
                  AND next_attempt_at > ?1
                "#,
            )
            .bind(now)
            .bind(route_id)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
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
        destination_id: i64,
        entry_id: i64,
        next_attempt_at: i64,
    ) -> AppResult<Option<DeliveryTask>> {
        let now = now_unix();
        let row = sqlx::query_as::<_, DeliveryTask>(
            r#"
            INSERT INTO delivery_tasks (
                route_id, channel_id, destination_id, entry_id, status, next_attempt_at, created_at
            )
            VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6)
            ON CONFLICT (route_id, entry_id) DO NOTHING
            RETURNING *
            "#,
        )
        .bind(route_id)
        .bind(channel_id)
        .bind(destination_id)
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
    /// Channels or destinations that are disabled, or parent channels currently inside
    /// `cooldown_until > now`, are skipped.
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
                channel_id = COALESCE(
                    (SELECT r.channel_id FROM routes r WHERE r.id = delivery_tasks.route_id),
                    channel_id
                ),
                destination_id = COALESCE(
                    (SELECT r.destination_id FROM routes r WHERE r.id = delivery_tasks.route_id),
                    destination_id
                ),
                checkpoint_json = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM routes r
                        WHERE r.id = delivery_tasks.route_id
                          AND (
                              r.channel_id != delivery_tasks.channel_id
                              OR r.destination_id != delivery_tasks.destination_id
                          )
                    ) THEN NULL
                    ELSE checkpoint_json
                END,
                last_status_code = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM routes r
                        WHERE r.id = delivery_tasks.route_id
                          AND (
                              r.channel_id != delivery_tasks.channel_id
                              OR r.destination_id != delivery_tasks.destination_id
                          )
                    ) THEN NULL
                    ELSE last_status_code
                END,
                attempt_count = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM routes r
                        WHERE r.id = delivery_tasks.route_id
                          AND (
                              r.channel_id != delivery_tasks.channel_id
                              OR r.destination_id != delivery_tasks.destination_id
                          )
                    ) THEN 1
                    ELSE attempt_count + 1
                END
            WHERE id = (
                SELECT dt.id
                FROM delivery_tasks dt
                JOIN routes r ON r.id = dt.route_id
                JOIN channels c ON c.id = r.channel_id
                JOIN channel_destinations cd ON cd.id = r.destination_id
                JOIN entries e ON e.id = dt.entry_id
                WHERE r.enabled = 1
                  AND c.enabled = 1
                  AND cd.enabled = 1
                  AND (c.cooldown_until IS NULL OR c.cooldown_until <= ?2)
                  AND (
                      (dt.status IN ('queued', 'retrying', 'deferred') AND dt.next_attempt_at <= ?2)
                      OR (dt.status = 'delivering' AND dt.lease_until IS NOT NULL AND dt.lease_until <= ?2)
                  )
                ORDER BY
                    CASE WHEN dt.status = 'delivering' THEN 0 ELSE 1 END ASC,
                    CASE
                        WHEN dt.status = 'delivering' THEN COALESCE(dt.lease_until, dt.next_attempt_at)
                        ELSE COALESCE(e.published_at, e.discovered_at)
                    END ASC,
                    e.id ASC,
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

    /// Transition an in-flight task to terminal `delivered` state, release its lease, and record
    /// an independent quota/pacing ledger entry that survives destination/route/feed deletions.
    pub async fn mark_task_delivered(
        &self,
        task_id: i64,
        completed_at: i64,
        status_code: Option<i32>,
        checkpoint: Option<&DeliveryCheckpoint>,
        diagnostic_log: Option<&str>,
    ) -> AppResult<DeliveryTask> {
        let checkpoint_json = checkpoint.map(serde_json::to_string).transpose()?;
        let mut tx = self.write_pool.begin().await?;
        let row = sqlx::query_as::<_, DeliveryTask>(
            r#"
            UPDATE delivery_tasks
            SET status = 'delivered',
                lease_until = NULL,
                checkpoint_json = COALESCE(?1, checkpoint_json),
                last_status_code = CASE
                    WHEN ?2 IS NOT NULL THEN ?2
                    WHEN last_status_code IS NOT NULL AND last_status_code >= 400 THEN NULL
                    ELSE last_status_code
                END,
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
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("delivery task {task_id} not found")))?;

        sqlx::query(
            r#"
            INSERT INTO delivery_quota_ledger (credential_group_key, channel_id, completed_at)
            SELECT c.credential_group_key, c.id, ?1
            FROM channels c
            WHERE c.id = ?2
            "#,
        )
        .bind(completed_at)
        .bind(row.channel_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(row)
    }

    /// Transition an in-flight task to `retrying` with updated `next_attempt_at` and checkpoint.
    ///
    /// If the parent route switched `(channel_id, destination_id)` while this task was `delivering`,
    /// the task is atomically realigned to the route's current target with cleared destination
    /// checkpoint, retry counter, and status code.
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
                channel_id = COALESCE(
                    (SELECT r.channel_id FROM routes r WHERE r.id = delivery_tasks.route_id),
                    channel_id
                ),
                destination_id = COALESCE(
                    (SELECT r.destination_id FROM routes r WHERE r.id = delivery_tasks.route_id),
                    destination_id
                ),
                next_attempt_at = ?1,
                attempt_count = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM routes r
                        WHERE r.id = delivery_tasks.route_id
                          AND (
                              r.channel_id != delivery_tasks.channel_id
                              OR r.destination_id != delivery_tasks.destination_id
                          )
                    ) THEN 0
                    ELSE attempt_count
                END,
                checkpoint_json = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM routes r
                        WHERE r.id = delivery_tasks.route_id
                          AND (
                              r.channel_id != delivery_tasks.channel_id
                              OR r.destination_id != delivery_tasks.destination_id
                          )
                    ) THEN NULL
                    ELSE COALESCE(?2, checkpoint_json)
                END,
                last_status_code = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM routes r
                        WHERE r.id = delivery_tasks.route_id
                          AND (
                              r.channel_id != delivery_tasks.channel_id
                              OR r.destination_id != delivery_tasks.destination_id
                          )
                    ) THEN NULL
                    ELSE ?3
                END,
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

    /// Transition a task to `deferred` (e.g. quota, pacing, delivery window, or Notion `202` async poll).
    /// Decrements `attempt_count` back by 1 so non-error deferral never consumes retry attempts.
    ///
    /// If the parent route switched `(channel_id, destination_id)` while this task was `delivering`,
    /// the task is atomically realigned to the route's current target with cleared destination
    /// checkpoint, retry counter, and status code.
    pub async fn mark_task_deferred(
        &self,
        task_id: i64,
        next_window_at: i64,
        status_code: Option<i32>,
        checkpoint: Option<&DeliveryCheckpoint>,
        diagnostic_log: &str,
    ) -> AppResult<DeliveryTask> {
        let checkpoint_json = checkpoint.map(serde_json::to_string).transpose()?;
        let row = sqlx::query_as::<_, DeliveryTask>(
            r#"
            UPDATE delivery_tasks
            SET status = 'deferred',
                lease_until = NULL,
                channel_id = COALESCE(
                    (SELECT r.channel_id FROM routes r WHERE r.id = delivery_tasks.route_id),
                    channel_id
                ),
                destination_id = COALESCE(
                    (SELECT r.destination_id FROM routes r WHERE r.id = delivery_tasks.route_id),
                    destination_id
                ),
                attempt_count = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM routes r
                        WHERE r.id = delivery_tasks.route_id
                          AND (
                              r.channel_id != delivery_tasks.channel_id
                              OR r.destination_id != delivery_tasks.destination_id
                          )
                    ) THEN 0
                    ELSE MAX(attempt_count - 1, 0)
                END,
                next_attempt_at = ?1,
                last_status_code = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM routes r
                        WHERE r.id = delivery_tasks.route_id
                          AND (
                              r.channel_id != delivery_tasks.channel_id
                              OR r.destination_id != delivery_tasks.destination_id
                          )
                    ) THEN NULL
                    ELSE COALESCE(?2, last_status_code)
                END,
                checkpoint_json = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM routes r
                        WHERE r.id = delivery_tasks.route_id
                          AND (
                              r.channel_id != delivery_tasks.channel_id
                              OR r.destination_id != delivery_tasks.destination_id
                          )
                    ) THEN NULL
                    ELSE COALESCE(?3, checkpoint_json)
                END,
                diagnostic_log = ?4
            WHERE id = ?5
            RETURNING *
            "#,
        )
        .bind(next_window_at)
        .bind(status_code)
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

    /// List delivery tasks joined with entry/route/channel/destination metadata for WebUI and MCP inspection.
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
                        dt.destination_id,
                        cd.name AS destination_name,
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
                    JOIN channel_destinations cd ON cd.id = dt.destination_id
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
                        dt.destination_id,
                        cd.name AS destination_name,
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
                    JOIN channel_destinations cd ON cd.id = dt.destination_id
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

    /// Requeue specific dead-letter/failed tasks by ID (provided their entry payload has not been purged),
    /// realigning `(channel_id, destination_id)` to the parent route's current target and clearing
    /// stale checkpoints/status codes when the route target changed.
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
                    channel_id = COALESCE(
                        (SELECT r.channel_id FROM routes r WHERE r.id = delivery_tasks.route_id),
                        channel_id
                    ),
                    destination_id = COALESCE(
                        (SELECT r.destination_id FROM routes r WHERE r.id = delivery_tasks.route_id),
                        destination_id
                    ),
                    checkpoint_json = CASE
                        WHEN EXISTS (
                            SELECT 1 FROM routes r
                            WHERE r.id = delivery_tasks.route_id
                              AND (
                                  r.channel_id != delivery_tasks.channel_id
                                  OR r.destination_id != delivery_tasks.destination_id
                              )
                        ) THEN NULL
                        ELSE checkpoint_json
                    END,
                    last_status_code = CASE
                        WHEN EXISTS (
                            SELECT 1 FROM routes r
                            WHERE r.id = delivery_tasks.route_id
                              AND (
                                  r.channel_id != delivery_tasks.channel_id
                                  OR r.destination_id != delivery_tasks.destination_id
                              )
                        ) THEN NULL
                        ELSE last_status_code
                    END,
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

    /// Requeue all `dead_letter` tasks whose entry payloads are still within retention, realigning
    /// `(channel_id, destination_id)` to the parent route's current target and clearing stale
    /// checkpoints/status codes when the route target changed.
    pub async fn requeue_all_dead_letter_tasks(&self, now: i64) -> AppResult<u64> {
        let res = sqlx::query(
            r#"
            UPDATE delivery_tasks
            SET status = 'queued',
                lease_until = NULL,
                channel_id = COALESCE(
                    (SELECT r.channel_id FROM routes r WHERE r.id = delivery_tasks.route_id),
                    channel_id
                ),
                destination_id = COALESCE(
                    (SELECT r.destination_id FROM routes r WHERE r.id = delivery_tasks.route_id),
                    destination_id
                ),
                checkpoint_json = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM routes r
                        WHERE r.id = delivery_tasks.route_id
                          AND (
                              r.channel_id != delivery_tasks.channel_id
                              OR r.destination_id != delivery_tasks.destination_id
                          )
                    ) THEN NULL
                    ELSE checkpoint_json
                END,
                last_status_code = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM routes r
                        WHERE r.id = delivery_tasks.route_id
                          AND (
                              r.channel_id != delivery_tasks.channel_id
                              OR r.destination_id != delivery_tasks.destination_id
                          )
                    ) THEN NULL
                    ELSE last_status_code
                END,
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

    /// Pull forward `queued`/`deferred` outbox tasks belonging to scheduled routes that inherit
    /// the global system timezone (`options.timezone` is absent/null/blank) so changing
    /// `system_settings.timezone` re-evaluates their delivery windows immediately.
    pub async fn release_system_timezone_scheduled_tasks(&self, now: i64) -> AppResult<u64> {
        let res = sqlx::query(
            r#"
            UPDATE delivery_tasks
            SET next_attempt_at = ?1
            WHERE status IN ('queued', 'deferred')
              AND next_attempt_at > ?1
              AND route_id IN (
                  SELECT id FROM routes
                  WHERE (
                      json_extract(options_json, '$.timezone') IS NULL
                      OR TRIM(CAST(json_extract(options_json, '$.timezone') AS TEXT)) = ''
                  )
                    AND (
                        COALESCE(json_array_length(options_json, '$.active_windows'), 0) > 0
                        OR COALESCE(json_array_length(options_json, '$.active_weekdays'), 0) > 0
                    )
              )
            "#,
        )
        .bind(now)
        .execute(&self.write_pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Compute rolling 1-hour (`3600s`) and 24-hour (`86400s`) delivered counts for both the
    /// individual `channel_id` and its shared `credential_group_key`, plus the latest completion
    /// timestamp, backed by the cascade-proof `delivery_quota_ledger`.
    pub async fn get_channel_quota_usage(
        &self,
        channel_id: i64,
        credential_group_key: &str,
        now: i64,
    ) -> AppResult<QuotaUsage> {
        let hour_cutoff = now.saturating_sub(3600);
        let day_cutoff = now.saturating_sub(86_400);

        let (channel_last_hour, channel_last_day): (i64, i64) = sqlx::query_as(
            r#"
            SELECT
                COALESCE(SUM(CASE WHEN completed_at >= ?2 THEN 1 ELSE 0 END), 0),
                COUNT(*)
            FROM delivery_quota_ledger
            WHERE channel_id = ?1
              AND completed_at >= ?3
            "#,
        )
        .bind(channel_id)
        .bind(hour_cutoff)
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
                COALESCE(SUM(CASE WHEN completed_at >= ?2 THEN 1 ELSE 0 END), 0),
                COUNT(*),
                MAX(completed_at)
            FROM delivery_quota_ledger
            WHERE credential_group_key = ?1
              AND completed_at >= ?3
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

    /// Return the strictest effective quota and pacing limits across all enabled channels sharing
    /// `credential_group_key`.
    pub async fn get_credential_group_effective_limits(
        &self,
        credential_group_key: &str,
    ) -> AppResult<Option<GroupEffectiveLimits>> {
        let row: (Option<i64>, Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as(
            r#"
            SELECT
                MIN(hourly_quota),
                MIN(daily_quota),
                MAX(min_interval_secs),
                MAX(jitter_max_secs)
            FROM channels
            WHERE credential_group_key = ?1
              AND enabled = 1
            "#,
        )
        .bind(credential_group_key)
        .fetch_one(&self.read_pool)
        .await?;

        match row {
            (_, _, None, None) => Ok(None),
            (hourly_quota, daily_quota, min_iv, jitter) => Ok(Some(GroupEffectiveLimits {
                hourly_quota,
                daily_quota,
                min_interval_secs: min_iv.unwrap_or(0),
                jitter_max_secs: jitter.unwrap_or(0),
            })),
        }
    }

    /// Purge rolling quota ledger entries strictly older than 24 hours (`completed_at < now - 86400`).
    pub async fn purge_expired_quota_ledger(&self, now: i64) -> AppResult<u64> {
        let cutoff = now.saturating_sub(86_400);
        let res = sqlx::query("DELETE FROM delivery_quota_ledger WHERE completed_at < ?1")
            .bind(cutoff)
            .execute(&self.write_pool)
            .await?;
        Ok(res.rows_affected())
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

        if fs::symlink_metadata(dest_path).is_ok() {
            fs::remove_file(dest_path)?;
        }
        for suffix in ["-wal", "-shm"] {
            let mut os = dest_path.as_os_str().to_os_string();
            os.push(suffix);
            let sidecar = PathBuf::from(os);
            if fs::symlink_metadata(&sidecar).is_ok() {
                fs::remove_file(&sidecar)?;
            }
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

    /// Revoke every session except `keep_token_hash` (used after password changes).
    pub async fn delete_sessions_except(&self, keep_token_hash: Option<&str>) -> AppResult<u64> {
        let res = sqlx::query("DELETE FROM sessions WHERE token_hash != ?1")
            .bind(keep_token_hash.unwrap_or(""))
            .execute(&self.write_pool)
            .await?;
        Ok(res.rows_affected())
    }

    /// Resolve the configured IANA system timezone (falls back to [`DEFAULT_TIMEZONE`]).
    pub async fn get_timezone(&self) -> Tz {
        let stored = self
            .get_setting_typed::<String>(SETTING_TIMEZONE)
            .await
            .ok()
            .flatten();
        stored
            .and_then(|raw| raw.trim().parse::<Tz>().ok())
            .or_else(|| DEFAULT_TIMEZONE.parse::<Tz>().ok())
            .unwrap_or(chrono_tz::UTC)
    }

    /// Read the raw persisted outbox queue state.
    pub async fn get_outbox_queue_state(&self) -> AppResult<OutboxQueueState> {
        Ok(self
            .get_setting_typed::<OutboxQueueState>(SETTING_OUTBOX_QUEUE)
            .await?
            .unwrap_or_default())
    }

    /// Persist the outbox queue state.
    pub async fn set_outbox_queue_state(&self, state: &OutboxQueueState) -> AppResult<()> {
        self.set_setting_typed(SETTING_OUTBOX_QUEUE, state).await?;
        Ok(())
    }

    /// Outbox queue state at `now`, lazily clearing an expired timed pause.
    pub async fn effective_outbox_queue_state(&self, now: i64) -> AppResult<OutboxQueueState> {
        let state = self.get_outbox_queue_state().await?;
        if state.paused && !state.is_blocking(now) {
            let resumed = OutboxQueueState::default();
            self.set_outbox_queue_state(&resumed).await?;
            return Ok(resumed);
        }
        Ok(state)
    }
}
