//! Axum route handlers for all `/api/v1/*` endpoints.

use std::collections::HashMap;
use std::str::FromStr;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE, HOST};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use bytes::Bytes;
use chrono::Utc;
use sqlx::QueryBuilder;
use tracing::warn;
use url::Url;

use crate::api::AppState;
use crate::api::dto::{
    ActionAckResponse, ChannelCooldownSummary, ChannelDestinationDto, ChannelDto,
    ClearFeedCacheRequest, ClearFeedCacheResponse, CreateChannelRequest, CreateDestinationRequest,
    CreateFeedRequest, CreateMcpTokenRequest, CreateMcpTokenResponse, CreateRouteRequest,
    DebugPushRequest, DebugPushResponse, DeliveryTaskDto, FeedDto, FeedPreviewRequest, GcResponse,
    ListTasksQuery, McpTokenDto, OpmlImportRequest, OpmlImportResponse, OverviewResponse,
    PollFeedResponse, QueueStateDto, RetryTasksRequest, RetryTasksResponse, RouteDto,
    SystemSettingsDto, TestChannelRequest, TestChannelResponse, UpdateChannelRequest,
    UpdateDestinationRequest, UpdateFeedRequest, UpdateQueueRequest, UpdateRouteRequest,
    UpdateSystemSettingsRequest, normalize_channel_config_on_create,
    normalize_destination_config_on_create, normalize_opt_string, normalize_string_vec,
};
use crate::channels::notion::{
    NotionInitDbRequest, NotionInitDbResponse, NotionInspectSchemaRequest,
    NotionInspectSchemaResponse,
};
use crate::channels::{ChannelConfig, ChannelKind, ChannelSender, DestinationConfig, RouteOptions};
use crate::config::{APP_NAME, LICENSE_SPDX, REPO_URL, VERSION};
use crate::crypto::hash_token;
use crate::db::models::{
    AppLocale, Channel, ChannelDestination, DeliveryTaskStatus, FeedHealthStatus, NewChannel,
    NewDestination, NewFeed, NewRoute, SETTING_LOCALE, SETTING_OUTBOUND_PROXY_URL,
    SETTING_SSRF_ALLOWLIST, SETTING_TIMEZONE, UpdateChannel, UpdateDestination, UpdateFeed,
    UpdateRoute, now_unix, unix_to_rfc3339,
};
use crate::error::{AppError, AppResult};
use crate::ingest::{ContentFilterRules, FeedPreviewResponse};
use crate::net::NetSettings;
use crate::schedule::{OutboxQueueState, parse_timezone, validate_schedule};
use crate::scheduler::{DebugPushParams, load_persisted_locale, normalize_net_settings};

// ===========================================================================
// Public Health Check
// ===========================================================================

/// `GET /api/v1/health`
pub async fn health_check() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "name": APP_NAME,
        "version": VERSION,
        "license": LICENSE_SPDX,
        "repo_url": REPO_URL,
        "status": "ok",
    }))
}

// ===========================================================================
// Overview & Maintenance (`/api/v1/overview`, `/api/v1/maintenance/*`)
// ===========================================================================

/// `GET /api/v1/overview`
pub async fn get_overview(State(state): State<AppState>) -> AppResult<Json<OverviewResponse>> {
    let snap = state.db().get_overview_snapshot().await?;
    let channels = state.db().list_channels().await?;
    let now = now_unix();

    let active_cooldowns = channels
        .into_iter()
        .filter_map(|ch| {
            let until = ch.cooldown_until?;
            if until > now {
                Some(ChannelCooldownSummary {
                    channel_id: ch.id,
                    channel_name: ch.name,
                    kind: ch.kind,
                    cooldown_until: unix_to_rfc3339(until),
                })
            } else {
                None
            }
        })
        .collect();

    Ok(Json(OverviewResponse::from_snapshot(
        snap,
        active_cooldowns,
    )))
}

/// `POST /api/v1/maintenance/gc`
pub async fn run_maintenance_gc(State(state): State<AppState>) -> AppResult<Json<GcResponse>> {
    let purged_entries = state.runtime().run_maintenance_once().await?;
    Ok(Json(GcResponse { purged_entries }))
}

/// `GET /api/v1/maintenance/backup`
pub async fn download_backup(State(state): State<AppState>) -> AppResult<Response> {
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let nonce: u32 = rand::random();
    let temp_path = state
        .config()
        .data_dir
        .join(format!(".feedcourier-backup-{stamp}-{nonce:08x}.db"));

    state.db().vacuum_into(&temp_path).await?;

    let read_res = tokio::fs::read(&temp_path).await;
    let _ = tokio::fs::remove_file(&temp_path).await;
    let bytes = read_res?;

    let disposition = format!("attachment; filename=\"feedcourier-snapshot-{stamp}.db\"");
    let mut resp = Response::new(Body::from(Bytes::from(bytes)));
    *resp.status_mut() = StatusCode::OK;
    resp.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    resp.headers_mut().insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition).map_err(|err| {
            AppError::Internal(anyhow::anyhow!("invalid Content-Disposition header: {err}"))
        })?,
    );
    Ok(resp)
}

// ===========================================================================
// Feeds & OPML (`/api/v1/feeds*`)
// ===========================================================================

/// `GET /api/v1/feeds`
pub async fn list_feeds(State(state): State<AppState>) -> AppResult<Json<Vec<FeedDto>>> {
    let feeds = state.db().list_feeds().await?;
    Ok(Json(feeds.into_iter().map(FeedDto::from_model).collect()))
}

/// `POST /api/v1/feeds`
pub async fn create_feed(
    State(state): State<AppState>,
    Json(req): Json<CreateFeedRequest>,
) -> AppResult<(StatusCode, Json<FeedDto>)> {
    let normalized_url = validate_http_url(&req.url)?;
    let poll_interval_secs = req.poll_interval_secs.unwrap_or(1800);
    if poll_interval_secs <= 0 {
        return Err(AppError::Validation(
            "poll_interval_secs must be greater than 0".to_string(),
        ));
    }

    if state.db().get_feed_by_url(&normalized_url).await?.is_some() {
        return Err(AppError::Conflict(format!(
            "Feed URL '{normalized_url}' is already subscribed"
        )));
    }

    let mut new_feed = NewFeed::new(normalized_url);
    new_feed.title = req.title.unwrap_or_default().trim().to_string();
    new_feed.tags = normalize_string_vec(req.tags.unwrap_or_default());
    new_feed.poll_interval_secs = poll_interval_secs;
    new_feed.fetch_full_content = req.fetch_full_content.unwrap_or(false);
    new_feed.use_proxy = req.use_proxy.unwrap_or(false);
    new_feed.silent_init = req.silent_init.unwrap_or(true);

    let created = state.db().create_feed(&new_feed).await?;
    let push_latest_one = req.push_latest_one.unwrap_or(false);

    // Best-effort initial poll so feed metadata & optional `push_latest_one` task are populated immediately.
    let final_feed = match state
        .ingest()
        .poll_feed(created.id, false, push_latest_one)
        .await
    {
        Ok(outcome) => {
            if outcome.enqueued_tasks > 0 {
                state.runtime().notify_outbox();
            }
            outcome.feed
        }
        Err(err) => {
            warn!(
                feed_id = created.id,
                url = %created.url,
                error = %err,
                "initial feed poll failed during creation; persisted with backoff status"
            );
            state.db().get_feed(created.id).await?.unwrap_or(created)
        }
    };

    Ok((StatusCode::CREATED, Json(FeedDto::from_model(final_feed))))
}

/// `PATCH /api/v1/feeds/{id}`
pub async fn update_feed(
    State(state): State<AppState>,
    Path(feed_id): Path<i64>,
    Json(req): Json<UpdateFeedRequest>,
) -> AppResult<Json<FeedDto>> {
    let url = match req.url {
        Some(raw) => Some(validate_http_url(&raw)?),
        None => None,
    };

    if let Some(st) = req.health_status
        && !matches!(st, FeedHealthStatus::Active | FeedHealthStatus::Paused)
    {
        return Err(AppError::Validation(
            "health_status patch must be 'active' or 'paused'".to_string(),
        ));
    }

    let title = req
        .title
        .map(|opt| opt.unwrap_or_default().trim().to_string());
    let tags = req.tags.map(normalize_string_vec);

    let patch = UpdateFeed {
        url,
        title,
        tags,
        poll_interval_secs: req.poll_interval_secs,
        fetch_full_content: req.fetch_full_content,
        use_proxy: req.use_proxy,
        silent_init: req.silent_init,
        health_status: req.health_status,
        ..UpdateFeed::default()
    };

    let updated = state.db().update_feed(feed_id, &patch).await?;
    if req.health_status == Some(FeedHealthStatus::Active) || req.poll_interval_secs.is_some() {
        state.runtime().notify_poller();
    }

    Ok(Json(FeedDto::from_model(updated)))
}

/// `DELETE /api/v1/feeds/{id}`
pub async fn delete_feed(
    State(state): State<AppState>,
    Path(feed_id): Path<i64>,
) -> AppResult<Json<ActionAckResponse>> {
    if !state.db().delete_feed(feed_id).await? {
        return Err(AppError::NotFound(format!("feed {feed_id} not found")));
    }
    Ok(Json(ActionAckResponse::ok()))
}

/// `POST /api/v1/feeds/{id}/poll`
pub async fn poll_feed_now(
    State(state): State<AppState>,
    Path(feed_id): Path<i64>,
) -> AppResult<Json<PollFeedResponse>> {
    let outcome = state.ingest().poll_feed(feed_id, true, false).await?;
    if outcome.enqueued_tasks > 0 {
        state.runtime().notify_outbox();
    }

    Ok(Json(PollFeedResponse {
        feed: FeedDto::from_model(outcome.feed),
        new_entries: outcome.new_entries,
        enqueued_tasks: outcome.enqueued_tasks,
        not_modified: outcome.not_modified,
    }))
}

/// `POST /api/v1/feeds/preview`
pub async fn preview_feed(
    State(state): State<AppState>,
    Json(req): Json<FeedPreviewRequest>,
) -> AppResult<Json<FeedPreviewResponse>> {
    let normalized_url = validate_http_url(&req.url)?;
    let preview = state
        .ingest()
        .preview_feed(
            &normalized_url,
            req.fetch_full_content,
            req.use_proxy,
            req.filter.as_ref(),
        )
        .await?;
    Ok(Json(preview))
}

/// `POST /api/v1/feeds/{id}/clear-cache`
pub async fn clear_feed_cache(
    State(state): State<AppState>,
    Path(feed_id): Path<i64>,
    body: Option<Json<ClearFeedCacheRequest>>,
) -> AppResult<Json<ClearFeedCacheResponse>> {
    let poll_now = body.map(|Json(req)| req.poll_now).unwrap_or(true);
    let outcome = state.ingest().clear_feed_cache(feed_id, poll_now).await?;
    if outcome.poll.as_ref().is_some_and(|p| p.enqueued_tasks > 0) {
        state.runtime().notify_outbox();
    }
    Ok(Json(ClearFeedCacheResponse {
        deleted_entries: outcome.deleted_entries,
        poll: outcome.poll.map(|p| PollFeedResponse {
            feed: FeedDto::from_model(p.feed),
            new_entries: p.new_entries,
            enqueued_tasks: p.enqueued_tasks,
            not_modified: p.not_modified,
        }),
        poll_error: outcome.poll_error,
    }))
}

/// `POST /api/v1/feeds/preview/push` — stateless debug push of a single live feed item.
pub async fn debug_push_feed(
    State(state): State<AppState>,
    Json(req): Json<DebugPushRequest>,
) -> AppResult<Json<DebugPushResponse>> {
    let url = validate_http_url(&req.url)?;
    let mut options = RouteOptions {
        telegram_mode: req.options.telegram_mode.unwrap_or_default(),
        link_preview: req.options.link_preview.unwrap_or(true),
        locale: req.options.locale,
        timezone: req.options.timezone,
        ..RouteOptions::default()
    };
    validate_schedule(&mut options)?;
    let result = state
        .runtime()
        .debug_push(DebugPushParams {
            destination_id: req.destination_id,
            channel_id: req.channel_id,
            url,
            feed_id: req.feed_id,
            fingerprint: req.fingerprint_sha256,
            fetch_full_content: req.fetch_full_content,
            use_proxy: req.use_proxy,
            options,
        })
        .await?;
    Ok(Json(DebugPushResponse {
        ok: result.ok,
        message: result.message,
        entry_title: result.entry_title,
        entry_url: result.entry_url,
    }))
}

/// `POST /api/v1/feeds/opml/import`
pub async fn import_opml(
    State(state): State<AppState>,
    Json(req): Json<OpmlImportRequest>,
) -> AppResult<Json<OpmlImportResponse>> {
    let result = state
        .ingest()
        .import_opml(&req.opml_xml, req.silent_init)
        .await?;
    if result.imported_count > 0 {
        state.runtime().notify_poller();
    }

    Ok(Json(OpmlImportResponse {
        imported_count: result.imported_count,
        skipped_existing_count: result.skipped_existing_count,
        feeds: result.feeds.into_iter().map(FeedDto::from_model).collect(),
    }))
}

/// `GET /api/v1/feeds/opml/export`
pub async fn export_opml(State(state): State<AppState>) -> AppResult<Response> {
    let xml = state.ingest().export_opml().await?;
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let disposition = format!("attachment; filename=\"feedcourier-feeds-{stamp}.opml\"");

    let mut resp = Response::new(Body::from(Bytes::from(xml)));
    *resp.status_mut() = StatusCode::OK;
    resp.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/xml; charset=utf-8"),
    );
    resp.headers_mut().insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition).map_err(|err| {
            AppError::Internal(anyhow::anyhow!("invalid Content-Disposition header: {err}"))
        })?,
    );
    Ok(resp)
}

// ===========================================================================
// Trusted Channels (Accounts) & Destinations (`/api/v1/channels*`, `/api/v1/destinations*`)
// ===========================================================================

/// `GET /api/v1/channels`
pub async fn list_channels(State(state): State<AppState>) -> AppResult<Json<Vec<ChannelDto>>> {
    let channels = state.db().list_channels().await?;
    let destinations = state.db().list_destinations().await?;
    let mut dests_by_channel: HashMap<i64, Vec<ChannelDestination>> = HashMap::new();
    for d in destinations {
        dests_by_channel.entry(d.channel_id).or_default().push(d);
    }

    let mut out = Vec::with_capacity(channels.len());
    for ch in channels {
        let dests = dests_by_channel.remove(&ch.id).unwrap_or_default();
        out.push(ChannelDto::from_model(ch, dests, state.master_key())?);
    }
    Ok(Json(out))
}

/// `POST /api/v1/channels`
pub async fn create_channel(
    State(state): State<AppState>,
    Json(mut req): Json<CreateChannelRequest>,
) -> AppResult<(StatusCode, Json<ChannelDto>)> {
    if req.kind != req.config.kind() {
        return Err(AppError::Validation(format!(
            "channel kind '{}' does not match config.kind '{}'",
            req.kind,
            req.config.kind()
        )));
    }
    validate_pacing_and_quotas(
        Some(req.min_interval_secs),
        Some(req.jitter_max_secs),
        req.hourly_quota,
        req.daily_quota,
    )?;

    normalize_channel_config_on_create(&mut req.config)?;

    if let Some(ref mut init_dest) = req.initial_destination {
        if init_dest.config.kind() != req.kind {
            return Err(AppError::Validation(format!(
                "initial_destination kind '{}' does not match channel kind '{}'",
                init_dest.config.kind(),
                req.kind
            )));
        }
        normalize_destination_config_on_create(&mut init_dest.config)?;
    }

    let explicit_name = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let (derived_name, capabilities_json) = match &req.config {
        ChannelConfig::Telegram(tg_cfg) => {
            match state
                .runtime()
                .telegram_sender()
                .probe_capabilities(tg_cfg, req.use_proxy)
                .await
            {
                Ok(caps) => {
                    let default_name = caps
                        .bot_username
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
                        .unwrap_or_else(|| "Telegram Bot".to_string());
                    (default_name, serde_json::to_string(&caps)?)
                }
                Err(
                    err @ (AppError::SsrfBlocked(_)
                    | AppError::Unauthorized(_)
                    | AppError::NotFound(_)),
                ) => {
                    return Err(err);
                }
                Err(err) => {
                    warn!(
                        error = %err,
                        "best-effort Telegram capability probe skipped during channel creation"
                    );
                    ("Telegram Bot".to_string(), "{}".to_string())
                }
            }
        }
        ChannelConfig::Smtp(smtp_cfg) => {
            (smtp_cfg.from_address.trim().to_string(), "{}".to_string())
        }
        ChannelConfig::Notion(notion_cfg) => {
            let bot_name = if explicit_name.is_none() {
                match state
                    .runtime()
                    .notion_sender()
                    .get_bot_name(&notion_cfg.integration_secret, req.use_proxy)
                    .await
                {
                    Ok(Some(n)) => n,
                    Ok(None) => "Notion Integration".to_string(),
                    Err(err) => {
                        warn!(
                            error = %err,
                            "best-effort Notion /v1/users/me lookup skipped during channel creation"
                        );
                        "Notion Integration".to_string()
                    }
                }
            } else {
                "Notion Integration".to_string()
            };
            (bot_name, "{}".to_string())
        }
    };

    let name = explicit_name.unwrap_or(derived_name);
    let credential_group_key = req.config.credential_group_key();
    let encrypted_config = state.master_key().encrypt_json(&req.config)?;

    let created = state
        .db()
        .create_channel(&NewChannel {
            name,
            kind: req.kind,
            encrypted_config,
            credential_group_key,
            capabilities_json,
            min_interval_secs: req.min_interval_secs,
            jitter_max_secs: req.jitter_max_secs,
            hourly_quota: req.hourly_quota,
            daily_quota: req.daily_quota,
            use_proxy: req.use_proxy,
            enabled: req.enabled,
        })
        .await?;

    let mut created_dests = Vec::new();
    if let Some(init_dest) = req.initial_destination {
        let dest = create_destination_for_channel(&state, &created, &req.config, init_dest).await?;
        created_dests.push(dest);
    }

    Ok((
        StatusCode::CREATED,
        Json(ChannelDto::from_model(
            created,
            created_dests,
            state.master_key(),
        )?),
    ))
}

/// `PATCH /api/v1/channels/{id}`
pub async fn update_channel(
    State(state): State<AppState>,
    Path(channel_id): Path<i64>,
    Json(req): Json<UpdateChannelRequest>,
) -> AppResult<Json<ChannelDto>> {
    let existing = state
        .db()
        .get_channel(channel_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("channel {channel_id} not found")))?;

    if let Some(ref raw_name) = req.name
        && raw_name.trim().is_empty()
    {
        return Err(AppError::Validation(
            "Channel name must not be empty".to_string(),
        ));
    }

    validate_pacing_and_quotas(
        req.min_interval_secs,
        req.jitter_max_secs,
        req.hourly_quota.flatten(),
        req.daily_quota.flatten(),
    )?;

    let mut patch = UpdateChannel {
        name: req.name.map(|s| s.trim().to_string()),
        min_interval_secs: req.min_interval_secs,
        jitter_max_secs: req.jitter_max_secs,
        hourly_quota: req.hourly_quota,
        daily_quota: req.daily_quota,
        use_proxy: req.use_proxy,
        enabled: req.enabled,
        ..UpdateChannel::default()
    };

    if let Some(ref cfg_patch) = req.config {
        let current_cfg: ChannelConfig = state
            .master_key()
            .decrypt_json(&existing.encrypted_config)?;
        let (merged_cfg, tg_endpoint_changed) = cfg_patch.apply_to(&current_cfg)?;

        patch.credential_group_key = Some(merged_cfg.credential_group_key());
        patch.encrypted_config = Some(state.master_key().encrypt_json(&merged_cfg)?);

        if tg_endpoint_changed && let ChannelConfig::Telegram(ref tg_cfg) = merged_cfg {
            match state
                .runtime()
                .telegram_sender()
                .probe_capabilities(tg_cfg, req.use_proxy.unwrap_or(existing.use_proxy))
                .await
            {
                Ok(caps) => {
                    patch.capabilities_json = Some(serde_json::to_string(&caps)?);
                }
                Err(err @ AppError::SsrfBlocked(_)) => return Err(err),
                Err(err) => {
                    warn!(
                        channel_id,
                        error = %err,
                        "best-effort Telegram capability probe skipped during channel update"
                    );
                }
            }
        }
    }

    let updated = state.db().update_channel(channel_id, &patch).await?;
    if req.enabled == Some(true) {
        state.runtime().notify_outbox();
    }

    let dests = state.db().list_destinations_for_channel(updated.id).await?;
    Ok(Json(ChannelDto::from_model(
        updated,
        dests,
        state.master_key(),
    )?))
}

/// `DELETE /api/v1/channels/{id}`
pub async fn delete_channel(
    State(state): State<AppState>,
    Path(channel_id): Path<i64>,
) -> AppResult<Json<ActionAckResponse>> {
    if !state.db().delete_channel(channel_id).await? {
        return Err(AppError::NotFound(format!(
            "channel {channel_id} not found"
        )));
    }
    Ok(Json(ActionAckResponse::ok()))
}

/// `POST /api/v1/channels/{id}/test`
///
/// When `destination_id` is passed in the request body, sends a full test delivery to that
/// destination; otherwise verifies account-level credentials (`getMe` + `sendRichMessage` probe
/// for Telegram, `GET /v1/users/me` for Notion, `EHLO`/`AUTH` connection test for SMTP).
pub async fn test_channel(
    State(state): State<AppState>,
    Path(channel_id): Path<i64>,
    body: Option<Json<TestChannelRequest>>,
) -> AppResult<Json<TestChannelResponse>> {
    let req = body.map(|Json(r)| r).unwrap_or_default();
    let channel = state
        .db()
        .get_channel(channel_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("channel {channel_id} not found")))?;

    if let Some(dest_id) = req.destination_id {
        let dest = state
            .db()
            .get_destination(dest_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("destination {dest_id} not found")))?;
        if dest.channel_id != channel.id {
            return Err(AppError::Validation(format!(
                "destination {dest_id} does not belong to channel {channel_id}"
            )));
        }
        return run_destination_test(&state, &channel, &dest, req.locale)
            .await
            .map(Json);
    }

    let cfg: ChannelConfig = state.master_key().decrypt_json(&channel.encrypted_config)?;
    match &cfg {
        ChannelConfig::Smtp(smtp_cfg) => {
            let message = state.runtime().smtp_sender().test_account(smtp_cfg).await?;
            Ok(Json(TestChannelResponse {
                ok: true,
                message,
                capabilities: None,
            }))
        }
        ChannelConfig::Telegram(tg_cfg) => {
            let sender = state.runtime().telegram_sender();
            let caps = sender.probe_capabilities(tg_cfg, channel.use_proxy).await?;
            let caps_val = serde_json::to_value(&caps)?;
            state
                .db()
                .update_channel_capabilities(channel.id, &caps_val)
                .await?;
            let bot_label = caps
                .bot_username
                .as_deref()
                .map(|u| format!("@{u}"))
                .or_else(|| caps.bot_first_name.clone())
                .unwrap_or_else(|| "bot".to_string());
            Ok(Json(TestChannelResponse {
                ok: true,
                message: format!(
                    "Telegram bot {bot_label} verified (sendRichMessage={})",
                    caps.supports_rich_message
                ),
                capabilities: Some(caps_val),
            }))
        }
        ChannelConfig::Notion(notion_cfg) => {
            let name = state
                .runtime()
                .notion_sender()
                .get_bot_name(&notion_cfg.integration_secret, channel.use_proxy)
                .await?
                .unwrap_or_else(|| "Notion Integration".to_string());
            Ok(Json(TestChannelResponse {
                ok: true,
                message: format!("Notion integration \"{name}\" verified via GET /v1/users/me"),
                capabilities: None,
            }))
        }
    }
}

/// `GET /api/v1/channels/{id}/destinations`
pub async fn list_channel_destinations(
    State(state): State<AppState>,
    Path(channel_id): Path<i64>,
) -> AppResult<Json<Vec<ChannelDestinationDto>>> {
    let channel = state
        .db()
        .get_channel(channel_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("channel {channel_id} not found")))?;
    let dests = state.db().list_destinations_for_channel(channel_id).await?;
    let mut out = Vec::with_capacity(dests.len());
    for d in dests {
        out.push(ChannelDestinationDto::from_model(
            d,
            channel.kind,
            state.master_key(),
        )?);
    }
    Ok(Json(out))
}

/// `POST /api/v1/channels/{id}/destinations`
pub async fn create_destination(
    State(state): State<AppState>,
    Path(channel_id): Path<i64>,
    Json(mut req): Json<CreateDestinationRequest>,
) -> AppResult<(StatusCode, Json<ChannelDestinationDto>)> {
    let channel = state
        .db()
        .get_channel(channel_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("channel {channel_id} not found")))?;

    if req.config.kind() != channel.kind {
        return Err(AppError::Validation(format!(
            "destination config kind '{}' does not match parent channel kind '{}'",
            req.config.kind(),
            channel.kind
        )));
    }
    normalize_destination_config_on_create(&mut req.config)?;

    let account_cfg: ChannelConfig = state.master_key().decrypt_json(&channel.encrypted_config)?;
    let created = create_destination_for_channel(&state, &channel, &account_cfg, req).await?;

    Ok((
        StatusCode::CREATED,
        Json(ChannelDestinationDto::from_model(
            created,
            channel.kind,
            state.master_key(),
        )?),
    ))
}

/// `PATCH /api/v1/destinations/{id}`
pub async fn update_destination(
    State(state): State<AppState>,
    Path(destination_id): Path<i64>,
    Json(req): Json<UpdateDestinationRequest>,
) -> AppResult<Json<ChannelDestinationDto>> {
    let existing = state
        .db()
        .get_destination(destination_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("destination {destination_id} not found")))?;
    let channel = state
        .db()
        .get_channel(existing.channel_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("channel {} not found", existing.channel_id)))?;

    if let Some(ref raw_name) = req.name
        && raw_name.trim().is_empty()
    {
        return Err(AppError::Validation(
            "Destination name must not be empty".to_string(),
        ));
    }

    let mut patch = UpdateDestination {
        name: req.name.map(|s| s.trim().to_string()),
        enabled: req.enabled,
        ..UpdateDestination::default()
    };

    if let Some(ref cfg_patch) = req.config {
        let current_cfg: DestinationConfig = state
            .master_key()
            .decrypt_json(&existing.encrypted_config)?;
        let merged = cfg_patch.apply_to(&current_cfg)?;
        patch.encrypted_config = Some(state.master_key().encrypt_json(&merged)?);
    }

    let updated = state
        .db()
        .update_destination(destination_id, &patch)
        .await?;
    if req.enabled == Some(true) {
        state.runtime().notify_outbox();
    }

    Ok(Json(ChannelDestinationDto::from_model(
        updated,
        channel.kind,
        state.master_key(),
    )?))
}

/// `DELETE /api/v1/destinations/{id}`
pub async fn delete_destination(
    State(state): State<AppState>,
    Path(destination_id): Path<i64>,
) -> AppResult<Json<ActionAckResponse>> {
    if !state.db().delete_destination(destination_id).await? {
        return Err(AppError::NotFound(format!(
            "destination {destination_id} not found"
        )));
    }
    Ok(Json(ActionAckResponse::ok()))
}

/// `POST /api/v1/destinations/{id}/test`
pub async fn test_destination(
    State(state): State<AppState>,
    Path(destination_id): Path<i64>,
    body: Option<Json<TestChannelRequest>>,
) -> AppResult<Json<TestChannelResponse>> {
    let test_locale = body.and_then(|Json(r)| r.locale);
    let dest = state
        .db()
        .get_destination(destination_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("destination {destination_id} not found")))?;
    let channel = state
        .db()
        .get_channel(dest.channel_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("channel {} not found", dest.channel_id)))?;
    run_destination_test(&state, &channel, &dest, test_locale)
        .await
        .map(Json)
}

async fn create_destination_for_channel(
    state: &AppState,
    channel: &Channel,
    account_cfg: &ChannelConfig,
    req: CreateDestinationRequest,
) -> AppResult<ChannelDestination> {
    let explicit_name = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let name = match explicit_name {
        Some(n) => n,
        None => derive_default_destination_name(state, channel, account_cfg, &req.config).await?,
    };

    let encrypted_config = state.master_key().encrypt_json(&req.config)?;
    state
        .db()
        .create_destination(&NewDestination {
            channel_id: channel.id,
            name,
            encrypted_config,
            enabled: req.enabled,
        })
        .await
}

async fn derive_default_destination_name(
    state: &AppState,
    channel: &Channel,
    account_cfg: &ChannelConfig,
    dest_cfg: &DestinationConfig,
) -> AppResult<String> {
    match (account_cfg, dest_cfg) {
        (ChannelConfig::Telegram(tg_acc), DestinationConfig::Telegram(tg_dest)) => {
            match state
                .runtime()
                .telegram_sender()
                .get_chat_name(tg_acc, &tg_dest.chat_id, channel.use_proxy)
                .await
            {
                Ok(Some(chat_name)) => Ok(chat_name),
                Ok(None) => Ok(tg_dest.chat_id.trim().to_string()),
                Err(err @ AppError::SsrfBlocked(_)) => Err(err),
                Err(err) => {
                    warn!(
                        channel_id = channel.id,
                        chat_id = %tg_dest.chat_id,
                        error = %err,
                        "best-effort Telegram getChat lookup skipped; falling back to chat_id"
                    );
                    Ok(tg_dest.chat_id.trim().to_string())
                }
            }
        }
        (_, DestinationConfig::Smtp(smtp_dest)) => Ok(smtp_dest
            .to_addresses
            .iter()
            .map(|s| s.trim())
            .find(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "SMTP Recipient".to_string())),
        (ChannelConfig::Notion(n_acc), DestinationConfig::Notion(n_dest)) => {
            match state
                .runtime()
                .notion_sender()
                .get_data_source_title(
                    &n_acc.integration_secret,
                    &n_dest.data_source_id,
                    channel.use_proxy,
                )
                .await
            {
                Ok(Some(title)) => Ok(title),
                Ok(None) => Ok(n_dest.data_source_id.clone()),
                Err(err) => {
                    warn!(
                        channel_id = channel.id,
                        data_source_id = %n_dest.data_source_id,
                        error = %err,
                        "best-effort Notion database title lookup skipped; falling back to data_source_id"
                    );
                    Ok(n_dest.data_source_id.clone())
                }
            }
        }
        _ => Ok("Destination".to_string()),
    }
}

async fn run_destination_test(
    state: &AppState,
    channel: &Channel,
    dest: &ChannelDestination,
    test_locale: Option<AppLocale>,
) -> AppResult<TestChannelResponse> {
    let channel_cfg: ChannelConfig = state.master_key().decrypt_json(&channel.encrypted_config)?;
    let dest_cfg: DestinationConfig = state.master_key().decrypt_json(&dest.encrypted_config)?;

    match channel.kind {
        ChannelKind::Smtp => {
            let sender = match test_locale {
                Some(loc) => state.runtime().smtp_sender().for_locale(loc),
                None => state.runtime().smtp_sender().clone(),
            };
            let message = sender
                .send_test(&channel_cfg, &dest_cfg, channel.use_proxy)
                .await?;
            Ok(TestChannelResponse {
                ok: true,
                message,
                capabilities: None,
            })
        }
        ChannelKind::Telegram => {
            let ChannelConfig::Telegram(ref tg_cfg) = channel_cfg else {
                return Err(AppError::Internal(anyhow::anyhow!(
                    "decrypted config kind mismatch for telegram channel {}",
                    channel.id
                )));
            };
            let mut sender = state.runtime().telegram_sender();
            if let Some(loc) = test_locale {
                sender.set_locale(loc);
            }
            let caps = sender.probe_capabilities(tg_cfg, channel.use_proxy).await?;
            let caps_val = serde_json::to_value(&caps)?;
            state
                .db()
                .update_channel_capabilities(channel.id, &caps_val)
                .await?;

            let message = sender
                .send_test(&channel_cfg, &dest_cfg, channel.use_proxy)
                .await?;
            Ok(TestChannelResponse {
                ok: true,
                message,
                capabilities: Some(caps_val),
            })
        }
        ChannelKind::Notion => {
            let message = state
                .runtime()
                .notion_sender()
                .send_test(&channel_cfg, &dest_cfg, channel.use_proxy)
                .await?;
            Ok(TestChannelResponse {
                ok: true,
                message,
                capabilities: None,
            })
        }
    }
}

/// `POST /api/v1/channels/notion/init-db`
pub async fn notion_init_db(
    State(state): State<AppState>,
    Json(mut req): Json<NotionInitDbRequest>,
) -> AppResult<Json<NotionInitDbResponse>> {
    if req.secret.trim().is_empty()
        && let Some(cid) = req.channel_id
    {
        let channel = state
            .db()
            .get_channel(cid)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("channel {cid} not found")))?;
        let decrypted: ChannelConfig =
            state.master_key().decrypt_json(&channel.encrypted_config)?;
        match decrypted {
            ChannelConfig::Notion(ncfg) => {
                req.secret = ncfg.integration_secret;
                req.use_proxy = req.use_proxy || channel.use_proxy;
            }
            other => {
                return Err(AppError::Validation(format!(
                    "channel {cid} is of kind '{}', expected 'notion'",
                    other.kind()
                )));
            }
        }
    }

    let resp = state.runtime().notion_sender().init_database(&req).await?;
    Ok(Json(resp))
}

/// `POST /api/v1/channels/notion/inspect-schema`
pub async fn notion_inspect_schema(
    State(state): State<AppState>,
    Json(mut req): Json<NotionInspectSchemaRequest>,
) -> AppResult<Json<NotionInspectSchemaResponse>> {
    if req.secret.trim().is_empty()
        && let Some(cid) = req.channel_id
    {
        let channel = state
            .db()
            .get_channel(cid)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("channel {cid} not found")))?;
        let decrypted: ChannelConfig =
            state.master_key().decrypt_json(&channel.encrypted_config)?;
        match decrypted {
            ChannelConfig::Notion(ncfg) => {
                req.secret = ncfg.integration_secret;
                req.use_proxy = req.use_proxy || channel.use_proxy;
            }
            other => {
                return Err(AppError::Validation(format!(
                    "channel {cid} is of kind '{}', expected 'notion'",
                    other.kind()
                )));
            }
        }
    }

    let resp = state.runtime().notion_sender().inspect_schema(&req).await?;
    Ok(Json(resp))
}

// ===========================================================================
// Routes (`/api/v1/routes*`)
// ===========================================================================

/// `GET /api/v1/routes`
pub async fn list_routes(State(state): State<AppState>) -> AppResult<Json<Vec<RouteDto>>> {
    let routes = state.db().list_routes().await?;
    Ok(Json(routes.into_iter().map(RouteDto::from_model).collect()))
}

/// `POST /api/v1/routes`
pub async fn create_route(
    State(state): State<AppState>,
    Json(req): Json<CreateRouteRequest>,
) -> AppResult<(StatusCode, Json<RouteDto>)> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Validation(
            "Route name must not be empty".to_string(),
        ));
    }

    let (channel, destination) = state
        .db()
        .resolve_destination_target(req.destination_id, req.channel_id)
        .await?;

    let include_keywords = normalize_string_vec(req.include_keywords);
    let exclude_keywords = normalize_string_vec(req.exclude_keywords);
    let include_regex = normalize_opt_string(req.include_regex);
    let exclude_regex = normalize_opt_string(req.exclude_regex);

    ContentFilterRules {
        include_keywords: include_keywords.clone(),
        exclude_keywords: exclude_keywords.clone(),
        include_regex: include_regex.clone(),
        exclude_regex: exclude_regex.clone(),
    }
    .compile()?;

    let mut options = req.options;
    validate_schedule(&mut options)?;

    let created = state
        .db()
        .create_route(&NewRoute {
            name,
            feed_ids: deduplicate_ids(req.feed_ids),
            match_tags: normalize_string_vec(req.match_tags),
            channel_id: channel.id,
            destination_id: destination.id,
            include_keywords,
            exclude_keywords,
            include_regex,
            exclude_regex,
            options,
            enabled: req.enabled,
        })
        .await?;

    Ok((StatusCode::CREATED, Json(RouteDto::from_model(created))))
}

/// `PATCH /api/v1/routes/{id}`
pub async fn update_route(
    State(state): State<AppState>,
    Path(route_id): Path<i64>,
    Json(req): Json<UpdateRouteRequest>,
) -> AppResult<Json<RouteDto>> {
    let existing = state
        .db()
        .get_route(route_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("route {route_id} not found")))?;

    if let Some(ref raw_name) = req.name
        && raw_name.trim().is_empty()
    {
        return Err(AppError::Validation(
            "Route name must not be empty".to_string(),
        ));
    }

    let (resolved_channel_id, resolved_destination_id) =
        if req.destination_id.is_some() || req.channel_id.is_some() {
            let (ch, dest) = state
                .db()
                .resolve_destination_target(req.destination_id, req.channel_id)
                .await?;
            (Some(ch.id), Some(dest.id))
        } else {
            (None, None)
        };

    let include_keywords = req.include_keywords.map(normalize_string_vec);
    let exclude_keywords = req.exclude_keywords.map(normalize_string_vec);
    let include_regex = req.include_regex.map(normalize_opt_string);
    let exclude_regex = req.exclude_regex.map(normalize_opt_string);

    ContentFilterRules {
        include_keywords: include_keywords
            .clone()
            .unwrap_or_else(|| existing.include_keywords()),
        exclude_keywords: exclude_keywords
            .clone()
            .unwrap_or_else(|| existing.exclude_keywords()),
        include_regex: include_regex
            .clone()
            .unwrap_or_else(|| existing.include_regex.clone()),
        exclude_regex: exclude_regex
            .clone()
            .unwrap_or_else(|| existing.exclude_regex.clone()),
    }
    .compile()?;

    let options = match req.options {
        Some(mut opts) => {
            validate_schedule(&mut opts)?;
            Some(opts)
        }
        None => None,
    };

    let patch = UpdateRoute {
        name: req.name.map(|s| s.trim().to_string()),
        feed_ids: req.feed_ids.map(deduplicate_ids),
        match_tags: req.match_tags.map(normalize_string_vec),
        channel_id: resolved_channel_id,
        destination_id: resolved_destination_id,
        include_keywords,
        exclude_keywords,
        include_regex,
        exclude_regex,
        options,
        enabled: req.enabled,
    };

    let should_wake_outbox =
        req.enabled == Some(true) || resolved_destination_id.is_some() || patch.options.is_some();

    let updated = state.db().update_route(route_id, &patch).await?;
    if should_wake_outbox {
        state.runtime().notify_outbox();
    }

    Ok(Json(RouteDto::from_model(updated)))
}

/// `DELETE /api/v1/routes/{id}`
pub async fn delete_route(
    State(state): State<AppState>,
    Path(route_id): Path<i64>,
) -> AppResult<Json<ActionAckResponse>> {
    if !state.db().delete_route(route_id).await? {
        return Err(AppError::NotFound(format!("route {route_id} not found")));
    }
    Ok(Json(ActionAckResponse::ok()))
}

// ===========================================================================
// Outbox & Dead-Letter Tasks (`/api/v1/tasks*`)
// ===========================================================================

/// `GET /api/v1/tasks`
pub async fn list_tasks(
    State(state): State<AppState>,
    Query(query): Query<ListTasksQuery>,
) -> AppResult<Json<Vec<DeliveryTaskDto>>> {
    let status = match query
        .status
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(raw) => Some(DeliveryTaskStatus::from_str(raw)?),
        None => None,
    };
    let limit = query.limit.unwrap_or(100).clamp(1, 500);

    let details = state.db().list_delivery_task_details(status, limit).await?;
    let feed_lookup = load_entry_feed_lookup(state.db(), &details).await?;

    let dtos = details
        .into_iter()
        .map(|detail| {
            let meta = feed_lookup.get(&detail.entry_id).cloned();
            DeliveryTaskDto::from_detail(detail, meta)
        })
        .collect();

    Ok(Json(dtos))
}

/// `POST /api/v1/tasks/retry`
pub async fn retry_tasks(
    State(state): State<AppState>,
    Json(req): Json<RetryTasksRequest>,
) -> AppResult<Json<RetryTasksResponse>> {
    let now = now_unix();
    let requeued_count = if req.all_dead_letter.unwrap_or(false) {
        state.db().requeue_all_dead_letter_tasks(now).await?
    } else if let Some(ref ids) = req.task_ids
        && !ids.is_empty()
    {
        state.db().requeue_tasks_by_ids(ids, now).await?
    } else {
        return Err(AppError::Validation(
            "Specify non-empty 'task_ids' or set 'all_dead_letter: true'".to_string(),
        ));
    };

    if requeued_count > 0 {
        state.runtime().notify_outbox();
    }

    Ok(Json(RetryTasksResponse { requeued_count }))
}

/// `GET /api/v1/tasks/queue`
pub async fn get_queue_state(State(state): State<AppState>) -> AppResult<Json<QueueStateDto>> {
    let queue = state.db().effective_outbox_queue_state(now_unix()).await?;
    Ok(Json(QueueStateDto::from_state(queue)))
}

/// `PUT /api/v1/tasks/queue`
pub async fn update_queue_state(
    State(state): State<AppState>,
    Json(req): Json<UpdateQueueRequest>,
) -> AppResult<Json<QueueStateDto>> {
    let now = now_unix();
    let next = match req.action.trim().to_ascii_lowercase().as_str() {
        "pause" => {
            let resume_at = if let Some(raw) = req
                .resume_at
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                let parsed = chrono::DateTime::parse_from_rfc3339(raw).map_err(|err| {
                    AppError::Validation(format!("invalid resume_at '{raw}': {err}"))
                })?;
                Some(parsed.timestamp())
            } else {
                req.resume_in_secs.map(|secs| now.saturating_add(secs))
            };
            if let Some(at) = resume_at
                && at <= now
            {
                return Err(AppError::Validation(
                    "resume time must be in the future".to_string(),
                ));
            }
            OutboxQueueState {
                paused: true,
                resume_at,
            }
        }
        "resume" => OutboxQueueState::default(),
        other => {
            return Err(AppError::Validation(format!(
                "invalid queue action '{other}' (expected 'pause' or 'resume')"
            )));
        }
    };
    state.db().set_outbox_queue_state(&next).await?;
    if !next.paused {
        state.runtime().notify_outbox();
    }
    Ok(Json(QueueStateDto::from_state(next)))
}

// ===========================================================================
// System Settings & MCP Bearer Tokens (`/api/v1/settings`, `/api/v1/mcp-tokens*`)
// ===========================================================================

/// `GET /api/v1/settings`
pub async fn get_settings(State(state): State<AppState>) -> AppResult<Json<SystemSettingsDto>> {
    let settings = state.http().settings();
    let locale = load_persisted_locale(state.config(), state.db()).await?;
    Ok(Json(SystemSettingsDto {
        outbound_proxy_url: settings.outbound_proxy_url,
        ssrf_allowlist: settings.ssrf_allowlist,
        timezone: state.db().get_timezone().await.name().to_string(),
        locale,
    }))
}

/// `PUT /api/v1/settings`
pub async fn update_settings(
    State(state): State<AppState>,
    Json(req): Json<UpdateSystemSettingsRequest>,
) -> AppResult<Json<SystemSettingsDto>> {
    let current_net = state.http().settings();
    let has_net_update = req.outbound_proxy_url.is_some() || req.ssrf_allowlist.is_some();

    let next_net = if !has_net_update {
        current_net
    } else {
        let normalized = normalize_net_settings(NetSettings {
            outbound_proxy_url: req.outbound_proxy_url.flatten(),
            ssrf_allowlist: req.ssrf_allowlist.unwrap_or_default(),
        });

        // Validate CIDRs/hostnames & proxy URL and hot-reload runtime clients FIRST.
        state
            .runtime()
            .reload_network_settings(normalized.clone())?;

        // Persist validated settings to SQLite `system_settings`.
        state
            .db()
            .set_setting_typed(SETTING_OUTBOUND_PROXY_URL, &normalized.outbound_proxy_url)
            .await?;
        state
            .db()
            .set_setting_typed(SETTING_SSRF_ALLOWLIST, &normalized.ssrf_allowlist)
            .await?;
        normalized
    };

    if let Some(raw_tz) = req.timezone.as_deref() {
        let tz = parse_timezone(raw_tz)?;
        state
            .db()
            .set_setting_typed(SETTING_TIMEZONE, &tz.name().to_string())
            .await?;
    }

    if let Some(locale) = req.locale {
        state.runtime().set_locale(locale);
        state
            .db()
            .set_setting_typed(SETTING_LOCALE, &locale.as_str())
            .await?;
    }

    let persisted_locale = load_persisted_locale(state.config(), state.db()).await?;

    Ok(Json(SystemSettingsDto {
        outbound_proxy_url: next_net.outbound_proxy_url,
        ssrf_allowlist: next_net.ssrf_allowlist,
        timezone: state.db().get_timezone().await.name().to_string(),
        locale: persisted_locale,
    }))
}

/// `GET /api/v1/mcp-tokens`
pub async fn list_mcp_tokens(State(state): State<AppState>) -> AppResult<Json<Vec<McpTokenDto>>> {
    let tokens = state.db().list_mcp_tokens().await?;
    Ok(Json(
        tokens.into_iter().map(McpTokenDto::from_model).collect(),
    ))
}

/// `POST /api/v1/mcp-tokens`
pub async fn create_mcp_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateMcpTokenRequest>,
) -> AppResult<(StatusCode, Json<CreateMcpTokenResponse>)> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Validation(
            "MCP token name must not be empty".to_string(),
        ));
    }

    let random_bytes: [u8; 24] = rand::random();
    let plaintext_token = format!("fc_mcp_{}", hex::encode(random_bytes));
    let token_hash = hash_token(&plaintext_token);
    let token_prefix = plaintext_token[..15].to_string();

    let created = state
        .db()
        .create_mcp_token(&name, &token_hash, &token_prefix, req.scope)
        .await?;

    let mcp_url = resolve_mcp_endpoint_url(&state, &headers);
    let snippet = serde_json::json!({
        "mcpServers": {
            "feedcourier": {
                "type": "streamable-http",
                "url": mcp_url,
                "headers": {
                    "Authorization": format!("Bearer {plaintext_token}"),
                }
            }
        }
    });
    let client_config_json = serde_json::to_string_pretty(&snippet)?;

    Ok((
        StatusCode::CREATED,
        Json(CreateMcpTokenResponse {
            token: McpTokenDto::from_model(created),
            plaintext_token,
            client_config_json,
        }),
    ))
}

/// `DELETE /api/v1/mcp-tokens/{id}`
pub async fn delete_mcp_token(
    State(state): State<AppState>,
    Path(token_id): Path<i64>,
) -> AppResult<Json<ActionAckResponse>> {
    if !state.db().delete_mcp_token(token_id).await? {
        return Err(AppError::NotFound(format!(
            "MCP token {token_id} not found"
        )));
    }
    Ok(Json(ActionAckResponse::ok()))
}

// ===========================================================================
// Internal Helpers
// ===========================================================================

fn validate_http_url(raw: &str) -> AppResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::Validation("URL must not be empty".to_string()));
    }
    let parsed = Url::parse(trimmed)?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(AppError::Validation(format!(
            "unsupported URL scheme '{}' (expected http or https)",
            parsed.scheme()
        )));
    }
    if parsed.host_str().is_none() {
        return Err(AppError::Validation("URL must include a host".to_string()));
    }
    Ok(parsed.to_string())
}

fn validate_pacing_and_quotas(
    min_interval_secs: Option<i64>,
    jitter_max_secs: Option<i64>,
    hourly_quota: Option<i64>,
    daily_quota: Option<i64>,
) -> AppResult<()> {
    if let Some(min_iv) = min_interval_secs
        && min_iv < 0
    {
        return Err(AppError::Validation(
            "min_interval_secs must be >= 0".to_string(),
        ));
    }
    if let Some(jitter) = jitter_max_secs
        && jitter < 0
    {
        return Err(AppError::Validation(
            "jitter_max_secs must be >= 0".to_string(),
        ));
    }
    if let Some(hq) = hourly_quota
        && hq <= 0
    {
        return Err(AppError::Validation(
            "hourly_quota must be greater than 0 when set".to_string(),
        ));
    }
    if let Some(dq) = daily_quota
        && dq <= 0
    {
        return Err(AppError::Validation(
            "daily_quota must be greater than 0 when set".to_string(),
        ));
    }
    Ok(())
}

fn deduplicate_ids(ids: Vec<i64>) -> Vec<i64> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

async fn load_entry_feed_lookup(
    db: &crate::db::Database,
    details: &[crate::db::models::DeliveryTaskDetail],
) -> AppResult<HashMap<i64, (i64, String)>> {
    if details.is_empty() {
        return Ok(HashMap::new());
    }

    let mut unique_entry_ids: Vec<i64> = Vec::with_capacity(details.len());
    for d in details {
        if !unique_entry_ids.contains(&d.entry_id) {
            unique_entry_ids.push(d.entry_id);
        }
    }

    let mut qb = QueryBuilder::new(
        "SELECT e.id, e.feed_id, f.title FROM entries e JOIN feeds f ON f.id = e.feed_id WHERE e.id IN (",
    );
    let mut separated = qb.separated(", ");
    for entry_id in unique_entry_ids {
        separated.push_bind(entry_id);
    }
    separated.push_unseparated(")");

    let rows: Vec<(i64, i64, String)> = qb.build_query_as().fetch_all(db.read_pool()).await?;
    let mut map = HashMap::with_capacity(rows.len());
    for (entry_id, feed_id, feed_title) in rows {
        map.insert(entry_id, (feed_id, feed_title));
    }
    Ok(map)
}

fn resolve_mcp_endpoint_url(state: &AppState, headers: &HeaderMap) -> String {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|p| matches!(*p, "http" | "https"))
        .unwrap_or("http");

    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(HOST))
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            let addr = state.config().listen_addr;
            if addr.ip().is_unspecified() {
                format!("127.0.0.1:{}", addr.port())
            } else {
                addr.to_string()
            }
        });

    format!("{scheme}://{host}/mcp")
}
