//! Axum route handlers for all `/api/v1/*` endpoints.

use std::collections::HashMap;
use std::str::FromStr;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE, HOST};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use chrono::Utc;
use sqlx::QueryBuilder;
use tracing::warn;
use url::Url;

use crate::api::AppState;
use crate::api::dto::{
    ActionAckResponse, BridgeManifestPreviewResponse, ChannelCooldownSummary,
    ChannelDestinationDto, ChannelDto, ChannelReferenceDto, ClearFeedCacheRequest,
    ClearFeedCacheResponse, ClearPluginCacheResponse, CreateChannelRequest,
    CreateDestinationRequest, CreateFeedRequest, CreateMcpTokenRequest, CreateMcpTokenResponse,
    CreatePluginInstanceRequest, CreateRouteRequest, DebugPushRequest, DebugPushResponse,
    DeleteCascadeQuery, DeliveryTaskDto, FeedDto, FeedPreviewRequest, GcResponse, ListTasksQuery,
    MarketPluginItemDto, MarketQuery, MarketResponse, McpTokenDto, OpmlImportRequest,
    OpmlImportResponse, OverviewResponse, PluginConflictBody, PluginDto, PluginInstanceDto,
    PollFeedResponse, QueueStateDto, ReferencingConflictBody, RegisterBridgePluginRequest,
    RetryTasksRequest, RetryTasksResponse, RouteDto, RouteReferenceDto, SendVerificationResponse,
    SmtpRecipientDto, SystemSettingsDto, TestChannelRequest, TestChannelResponse,
    TestPluginInstanceResponse, UpdateChannelRequest, UpdateDestinationRequest, UpdateFeedRequest,
    UpdatePluginInstanceRequest, UpdatePluginRequest, UpdateQueueRequest, UpdateRouteRequest,
    UpdateSystemSettingsRequest, VerifyRecipientCodeRequest, normalize_channel_config_on_create,
    normalize_destination_config_on_create, normalize_opt_string, normalize_string_vec,
};
use crate::channels::notion::{
    NotionInitDbRequest, NotionInitDbResponse, NotionInspectSchemaRequest,
    NotionInspectSchemaResponse,
};
use crate::channels::{
    ChannelConfig, ChannelKind, ChannelSender, DestinationConfig, RouteOptions,
    SmtpDeliveryRecipient,
};
use crate::config::{APP_NAME, LICENSE_SPDX, REPO_URL, VERSION, validate_public_url};
use crate::crypto::{constant_time_eq, hash_token, mask_email_address, sha256_hex};
use crate::db::models::{
    AppLocale, Channel, ChannelDestination, DeliveryTaskStatus, FeedHealthStatus, NewChannel,
    NewDestination, NewFeed, NewRoute, SETTING_LOCALE, SETTING_OUTBOUND_PROXY_URL,
    SETTING_PUBLIC_URL, SETTING_SSRF_ALLOWLIST, SETTING_TIMEZONE, SmtpRecipientRow,
    SmtpRecipientStatus, UpdateChannel, UpdateDestination, UpdateFeed, UpdateRoute, now_unix,
    unix_to_rfc3339,
};
use crate::db::repo::DeleteGuardOutcome;
use crate::error::{AppError, AppResult};
use crate::ingest::{ContentFilterRules, FeedPreviewResponse};
use crate::net::NetSettings;
use crate::plugins::bridge::{BridgeClient, validate_bridge_url, validate_config_against_schema};
use crate::plugins::link_rewrite::LINK_REWRITE_PLUGIN_ID;
use crate::plugins::{PluginKind, PluginRuntime, check_plugin_compatibility};
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
        plugins: req.options.plugins.unwrap_or_default(),
        ..RouteOptions::default()
    };
    validate_schedule(&mut options)?;
    state
        .runtime()
        .plugins()
        .validate_route_plugins(state.db(), &options)
        .await?;
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
        out.push(build_channel_dto(&state, ch, dests).await?);
    }
    Ok(Json(out))
}

/// Validate that a registered bridge plugin is a compatible, enabled `channel` plugin before
/// creating, updating, or testing channel accounts or destinations.
fn ensure_channel_bridge_usable(
    bridge: &crate::plugins::BridgePluginEntry,
    plugin_id: &str,
    disabled_suffix: &str,
) -> AppResult<()> {
    if bridge.manifest.kind != PluginKind::Channel {
        return Err(AppError::Validation(format!(
            "Plugin '{plugin_id}' is a '{}' plugin, not a channel plugin",
            bridge.manifest.kind.as_str()
        )));
    }
    if !bridge.compatible {
        return Err(AppError::Validation(format!(
            "Channel plugin '{plugin_id}' is incompatible with current core: {}",
            bridge
                .compatibility_reason
                .as_deref()
                .unwrap_or("version mismatch")
        )));
    }
    if !bridge.enabled {
        return Err(AppError::Validation(format!(
            "Channel plugin '{plugin_id}' is disabled{disabled_suffix}"
        )));
    }
    Ok(())
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
    if req.kind == ChannelKind::Smtp && req.use_proxy {
        return Err(AppError::Validation(
            "SMTP channels use direct TCP/TLS connections and do not support outbound HTTP/SOCKS proxies"
                .to_string(),
        ));
    }
    if req.kind == ChannelKind::Plugin && req.use_proxy {
        return Err(AppError::Validation(
            "Bridge channel plugins connect directly to their registered sidecar and do not support outbound HTTP/SOCKS proxies"
                .to_string(),
        ));
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

    let (derived_name, capabilities_json) = match &mut req.config {
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
        ChannelConfig::Plugin(plugin_cfg) => {
            let bridge = state
                .runtime()
                .plugins()
                .get_bridge(&plugin_cfg.plugin_id)
                .ok_or_else(|| {
                    AppError::Validation(format!(
                        "Channel plugin '{}' is not installed",
                        plugin_cfg.plugin_id
                    ))
                })?;
            ensure_channel_bridge_usable(
                &bridge,
                &plugin_cfg.plugin_id,
                "; enable it before creating accounts",
            )?;
            let locally_validated = validate_config_against_schema(
                &bridge.manifest.account_schema,
                &plugin_cfg.config,
            )?;
            let normalized = bridge
                .client
                .validate_remote("account", &locally_validated)
                .await?;
            plugin_cfg.config = normalized;
            (
                bridge.manifest.name.clone(),
                serde_json::to_string(&bridge.manifest.capabilities)?,
            )
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
        Json(build_channel_dto(&state, created, created_dests).await?),
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
    if existing.kind == ChannelKind::Smtp && req.use_proxy == Some(true) {
        return Err(AppError::Validation(
            "SMTP channels use direct TCP/TLS connections and do not support outbound HTTP/SOCKS proxies"
                .to_string(),
        ));
    }
    if existing.kind == ChannelKind::Plugin && req.use_proxy == Some(true) {
        return Err(AppError::Validation(
            "Bridge channel plugins connect directly to their registered sidecar and do not support outbound HTTP/SOCKS proxies"
                .to_string(),
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
        let plugin_bridge = match &current_cfg {
            ChannelConfig::Plugin(p) => state.runtime().plugins().get_bridge(&p.plugin_id),
            _ => None,
        };
        let (mut merged_cfg, tg_endpoint_changed) = cfg_patch.apply_to_with_schema(
            &current_cfg,
            plugin_bridge.as_ref().map(|b| &b.manifest.account_schema),
        )?;
        if let ChannelConfig::Plugin(ref mut p_cfg) = merged_cfg {
            let bridge = plugin_bridge.ok_or_else(|| {
                AppError::Validation(format!(
                    "Channel plugin '{}' is not installed",
                    p_cfg.plugin_id
                ))
            })?;
            ensure_channel_bridge_usable(&bridge, &p_cfg.plugin_id, "")?;
            p_cfg.config = bridge
                .client
                .validate_remote("account", &p_cfg.config)
                .await?;
        }

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
                Err(
                    err @ (AppError::SsrfBlocked(_)
                    | AppError::Unauthorized(_)
                    | AppError::NotFound(_)),
                ) => return Err(err),
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
    Ok(Json(build_channel_dto(&state, updated, dests).await?))
}

/// `DELETE /api/v1/channels/{id}`
///
/// Refuses deletion with `409 Conflict` listing referencing routes unless `?cascade=true` is passed.
pub async fn delete_channel(
    State(state): State<AppState>,
    Path(channel_id): Path<i64>,
    Query(query): Query<DeleteCascadeQuery>,
) -> AppResult<Response> {
    match state
        .db()
        .delete_channel_guarded(channel_id, query.cascade.unwrap_or(false))
        .await?
    {
        DeleteGuardOutcome::NotFound => Err(AppError::NotFound(format!(
            "channel {channel_id} not found"
        ))),
        DeleteGuardOutcome::Conflict(refs) => {
            let names: Vec<&str> = refs.iter().map(|(_, n)| n.as_str()).collect();
            let msg = format!(
                "Channel {channel_id} is referenced by {} route(s): {}. Pass ?cascade=true to delete it together with those routes.",
                refs.len(),
                names.join(", ")
            );
            let body = ReferencingConflictBody {
                error: msg.clone(),
                code: "conflict",
                message: msg,
                referencing_routes: refs
                    .into_iter()
                    .map(|(id, name)| RouteReferenceDto { id, name })
                    .collect(),
            };
            Ok((StatusCode::CONFLICT, Json(body)).into_response())
        }
        DeleteGuardOutcome::Deleted => {
            Ok((StatusCode::OK, Json(ActionAckResponse::ok())).into_response())
        }
    }
}

/// `POST /api/v1/channels/{id}/test`
///
/// When `destination_id` is passed in the request body, sends a full test delivery to that
/// destination; otherwise verifies account-level credentials (`getMe` + `sendRichMessage` probe
/// for Telegram, `GET /v1/users/me` for Notion, self-test email to `from_address` for SMTP).
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
            let effective_locale = req.locale.unwrap_or_else(|| state.runtime().locale());
            let message = state
                .runtime()
                .smtp_sender()
                .send_test_self(smtp_cfg, effective_locale)
                .await?;
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
        ChannelConfig::Plugin(plugin_cfg) => {
            let bridge = state
                .runtime()
                .plugins()
                .get_bridge(&plugin_cfg.plugin_id)
                .ok_or_else(|| {
                    AppError::NotFound(format!(
                        "Channel plugin '{}' is not installed",
                        plugin_cfg.plugin_id
                    ))
                })?;
            ensure_channel_bridge_usable(&bridge, &plugin_cfg.plugin_id, "")?;
            let message = bridge
                .client
                .test_remote(&serde_json::json!({
                    "scope": "account",
                    "account": plugin_cfg.config,
                }))
                .await?;
            Ok(Json(TestChannelResponse {
                ok: true,
                message,
                capabilities: Some(bridge.manifest.capabilities.clone()),
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
        out.push(build_destination_dto(&state, d, channel.kind).await?);
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
        Json(build_destination_dto(&state, created, channel.kind).await?),
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

    let mut merged_smtp_cfg: Option<DestinationConfig> = None;
    if let Some(ref cfg_patch) = req.config {
        let current_cfg: DestinationConfig = state
            .master_key()
            .decrypt_json(&existing.encrypted_config)?;
        let plugin_bridge = if channel.kind == ChannelKind::Plugin {
            let acct_cfg: ChannelConfig =
                state.master_key().decrypt_json(&channel.encrypted_config)?;
            match acct_cfg {
                ChannelConfig::Plugin(p) => state.runtime().plugins().get_bridge(&p.plugin_id),
                _ => None,
            }
        } else {
            None
        };
        let mut merged = cfg_patch.apply_to_with_schema(
            &current_cfg,
            plugin_bridge
                .as_ref()
                .map(|b| &b.manifest.destination_schema),
        )?;
        if let DestinationConfig::Plugin(ref mut p_dest) = merged {
            let bridge = plugin_bridge.ok_or_else(|| {
                AppError::Validation("Parent channel plugin is not installed".to_string())
            })?;
            ensure_channel_bridge_usable(&bridge, &bridge.manifest.id, "")?;
            p_dest.config = bridge
                .client
                .validate_remote("destination", &p_dest.config)
                .await?;
        }
        patch.encrypted_config = Some(state.master_key().encrypt_json(&merged)?);
        if matches!(merged, DestinationConfig::Smtp(_)) {
            merged_smtp_cfg = Some(merged);
        }
    }

    let updated = state
        .db()
        .update_destination(destination_id, &patch)
        .await?;
    if let Some(ref smtp_cfg) = merged_smtp_cfg {
        sync_smtp_destination_recipients(&state, updated.id, smtp_cfg).await?;
    }
    if req.enabled == Some(true) {
        state.runtime().notify_outbox();
    }

    Ok(Json(
        build_destination_dto(&state, updated, channel.kind).await?,
    ))
}

/// `DELETE /api/v1/destinations/{id}`
///
/// Refuses deletion with `409 Conflict` listing referencing routes unless `?cascade=true` is passed.
pub async fn delete_destination(
    State(state): State<AppState>,
    Path(destination_id): Path<i64>,
    Query(query): Query<DeleteCascadeQuery>,
) -> AppResult<Response> {
    match state
        .db()
        .delete_destination_guarded(destination_id, query.cascade.unwrap_or(false))
        .await?
    {
        DeleteGuardOutcome::NotFound => Err(AppError::NotFound(format!(
            "destination {destination_id} not found"
        ))),
        DeleteGuardOutcome::Conflict(refs) => {
            let names: Vec<&str> = refs.iter().map(|(_, n)| n.as_str()).collect();
            let msg = format!(
                "Destination {destination_id} is referenced by {} route(s): {}. Pass ?cascade=true to delete it together with those routes.",
                refs.len(),
                names.join(", ")
            );
            let body = ReferencingConflictBody {
                error: msg.clone(),
                code: "conflict",
                message: msg,
                referencing_routes: refs
                    .into_iter()
                    .map(|(id, name)| RouteReferenceDto { id, name })
                    .collect(),
            };
            Ok((StatusCode::CONFLICT, Json(body)).into_response())
        }
        DeleteGuardOutcome::Deleted => {
            Ok((StatusCode::OK, Json(ActionAckResponse::ok())).into_response())
        }
    }
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
    mut req: CreateDestinationRequest,
) -> AppResult<ChannelDestination> {
    if let (ChannelConfig::Plugin(acct_cfg), DestinationConfig::Plugin(dest_cfg)) =
        (account_cfg, &mut req.config)
    {
        let bridge = state
            .runtime()
            .plugins()
            .get_bridge(&acct_cfg.plugin_id)
            .ok_or_else(|| {
                AppError::Validation(format!(
                    "Channel plugin '{}' is not installed",
                    acct_cfg.plugin_id
                ))
            })?;
        ensure_channel_bridge_usable(&bridge, &acct_cfg.plugin_id, "")?;
        let locally_validated =
            validate_config_against_schema(&bridge.manifest.destination_schema, &dest_cfg.config)?;
        dest_cfg.config = bridge
            .client
            .validate_remote("destination", &locally_validated)
            .await?;
    }

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
    let created = state
        .db()
        .create_destination(&NewDestination {
            channel_id: channel.id,
            name,
            encrypted_config,
            enabled: req.enabled,
        })
        .await?;
    if matches!(req.config, DestinationConfig::Smtp(_)) {
        sync_smtp_destination_recipients(state, created.id, &req.config).await?;
    }
    Ok(created)
}

async fn sync_smtp_destination_recipients(
    state: &AppState,
    destination_id: i64,
    dest_cfg: &DestinationConfig,
) -> AppResult<Vec<SmtpRecipientRow>> {
    let DestinationConfig::Smtp(smtp_dest) = dest_cfg else {
        return Ok(Vec::new());
    };
    let mut hmacs: Vec<String> = Vec::new();
    for addr in &smtp_dest.to_addresses {
        let trimmed = addr.trim();
        if trimmed.is_empty() {
            continue;
        }
        let h = state.master_key().smtp_recipient_hmac(trimmed);
        if !hmacs.contains(&h) {
            hmacs.push(h);
        }
    }
    state
        .db()
        .sync_smtp_recipients_for_destination(
            destination_id,
            &hmacs,
            SmtpRecipientStatus::Unverified,
            None,
        )
        .await
}

async fn build_destination_dto(
    state: &AppState,
    dest: ChannelDestination,
    kind: ChannelKind,
) -> AppResult<ChannelDestinationDto> {
    let plugin_dest_schema = if kind == ChannelKind::Plugin {
        state
            .db()
            .get_channel(dest.channel_id)
            .await?
            .and_then(|ch| {
                state
                    .master_key()
                    .decrypt_json::<ChannelConfig>(&ch.encrypted_config)
                    .ok()
            })
            .and_then(|cfg| match cfg {
                ChannelConfig::Plugin(p) => state
                    .runtime()
                    .plugins()
                    .get_bridge(&p.plugin_id)
                    .map(|b| b.manifest.destination_schema),
                _ => None,
            })
    } else {
        None
    };
    let smtp_summary = if kind == ChannelKind::Smtp {
        Some(state.db().get_smtp_recipient_summary(dest.id).await?)
    } else {
        None
    };
    ChannelDestinationDto::from_model_with_schema(
        dest,
        kind,
        state.master_key(),
        smtp_summary,
        plugin_dest_schema.as_ref(),
    )
}

async fn build_channel_dto(
    state: &AppState,
    channel: Channel,
    destinations: Vec<ChannelDestination>,
) -> AppResult<ChannelDto> {
    let plugin_schemas = if channel.kind == ChannelKind::Plugin {
        state
            .master_key()
            .decrypt_json::<ChannelConfig>(&channel.encrypted_config)
            .ok()
            .and_then(|cfg| match cfg {
                ChannelConfig::Plugin(p) => state
                    .runtime()
                    .plugins()
                    .get_bridge(&p.plugin_id)
                    .map(|b| (b.manifest.account_schema, b.manifest.destination_schema)),
                _ => None,
            })
    } else {
        None
    };
    let mut dto = ChannelDto::from_model_with_schema(
        channel.clone(),
        Vec::new(),
        state.master_key(),
        plugin_schemas.as_ref().map(|(a, _)| a),
    )?;
    let mut dest_dtos = Vec::with_capacity(destinations.len());
    for d in destinations {
        let smtp_summary = if channel.kind == ChannelKind::Smtp {
            Some(state.db().get_smtp_recipient_summary(d.id).await?)
        } else {
            None
        };
        dest_dtos.push(ChannelDestinationDto::from_model_with_schema(
            d,
            channel.kind,
            state.master_key(),
            smtp_summary,
            plugin_schemas.as_ref().map(|(_, ds)| ds),
        )?);
    }
    dto.destinations = dest_dtos;
    Ok(dto)
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
            .map(mask_email_address)
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
        (ChannelConfig::Plugin(acct), DestinationConfig::Plugin(dest)) => {
            let preferred = ["topic", "channel", "room", "target", "name", "webhook_name"];
            if let Some(obj) = dest.config.as_object() {
                for key in preferred {
                    if let Some(s) = obj
                        .get(key)
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        return Ok(s.chars().take(64).collect());
                    }
                }
                for (k, v) in obj {
                    if !crate::logging::redact::is_sensitive_field_name(k)
                        && let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty())
                    {
                        return Ok(s.chars().take(64).collect());
                    }
                }
            }
            let fallback = state
                .runtime()
                .plugins()
                .get_bridge(&acct.plugin_id)
                .map(|b| format!("{} Target", b.manifest.name))
                .unwrap_or_else(|| "Plugin Target".to_string());
            Ok(fallback)
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
            let (ChannelConfig::Smtp(smtp_cfg), DestinationConfig::Smtp(smtp_dest)) =
                (&channel_cfg, &dest_cfg)
            else {
                return Err(AppError::Validation(
                    "SMTP destination config kind mismatch".to_string(),
                ));
            };
            let rows = sync_smtp_destination_recipients(state, dest.id, &dest_cfg).await?;
            let mut hmac_to_email: HashMap<String, String> = HashMap::new();
            for raw in &smtp_dest.to_addresses {
                let trimmed = raw.trim();
                if !trimmed.is_empty() {
                    let h = state.master_key().smtp_recipient_hmac(trimmed);
                    hmac_to_email
                        .entry(h)
                        .or_insert_with(|| trimmed.to_string());
                }
            }
            let (public_url, _) = state.db().get_effective_public_url(state.config()).await?;
            let mut verified_recipients = Vec::new();
            for row in rows {
                if row.status == SmtpRecipientStatus::Verified
                    && let Some(email) = hmac_to_email.get(&row.address_hmac)
                {
                    let unsubscribe_url = public_url.as_deref().map(|base| {
                        let tok = state.master_key().sign_unsubscribe_token(dest.id, row.id);
                        format!("{base}/u/{tok}")
                    });
                    verified_recipients.push(SmtpDeliveryRecipient {
                        recipient_id: row.id,
                        email: email.clone(),
                        address_hmac: row.address_hmac,
                        unsubscribe_url,
                    });
                }
            }
            let effective_locale = test_locale.unwrap_or_else(|| state.runtime().locale());
            let message = state
                .runtime()
                .smtp_sender()
                .send_test_to_verified(
                    smtp_cfg,
                    smtp_dest,
                    dest.id,
                    &dest.name,
                    &verified_recipients,
                    effective_locale,
                )
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
            let (ChannelConfig::Notion(n_cfg), DestinationConfig::Notion(n_dest)) =
                (&channel_cfg, &dest_cfg)
            else {
                return Err(AppError::Validation(
                    "Notion destination config kind mismatch".to_string(),
                ));
            };
            let (message, updated_dest) = state
                .runtime()
                .notion_sender()
                .send_test_with_resolved(&n_cfg.integration_secret, n_dest, channel.use_proxy)
                .await?;
            if let Some(next_dest) = updated_dest {
                let _ = state
                    .db()
                    .update_notion_destination_ids_atomic(
                        dest.id,
                        state.master_key(),
                        &next_dest.data_source_id,
                        next_dest.database_id.as_deref(),
                    )
                    .await;
            }
            Ok(TestChannelResponse {
                ok: true,
                message,
                capabilities: None,
            })
        }
        ChannelKind::Plugin => {
            let message = state
                .runtime()
                .plugin_sender()
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
    state
        .runtime()
        .plugins()
        .validate_route_plugins(state.db(), &options)
        .await?;

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
            state
                .runtime()
                .plugins()
                .validate_route_plugins(state.db(), &opts)
                .await?;
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
    let (public_url, public_url_env_managed) =
        state.db().get_effective_public_url(state.config()).await?;
    Ok(Json(SystemSettingsDto {
        outbound_proxy_url: settings.outbound_proxy_url,
        ssrf_allowlist: settings.ssrf_allowlist,
        timezone: state.db().get_timezone().await.name().to_string(),
        locale,
        public_url,
        public_url_env_managed,
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
        let prev_tz = state.db().get_timezone().await;
        let tz = parse_timezone(raw_tz)?;
        state
            .db()
            .set_setting_typed(SETTING_TIMEZONE, &tz.name().to_string())
            .await?;
        if tz != prev_tz {
            state
                .db()
                .release_system_timezone_scheduled_tasks(now_unix())
                .await?;
            state.runtime().notify_outbox();
        }
    }

    if let Some(locale) = req.locale {
        state.runtime().set_locale(locale);
        state
            .db()
            .set_setting_typed(SETTING_LOCALE, &locale.as_str())
            .await?;
    }

    if let Some(opt_public_url) = req.public_url {
        if state.config().public_url.is_some() {
            return Err(AppError::Validation(
                "PUBLIC_URL is managed by the environment variable and cannot be changed from the WebUI".to_string(),
            ));
        }
        let normalized_public_url = match opt_public_url {
            Some(raw) if !raw.trim().is_empty() => Some(validate_public_url(&raw)?),
            _ => None,
        };
        state
            .db()
            .set_setting_typed(SETTING_PUBLIC_URL, &normalized_public_url)
            .await?;
    }

    let persisted_locale = load_persisted_locale(state.config(), state.db()).await?;
    let (public_url, public_url_env_managed) =
        state.db().get_effective_public_url(state.config()).await?;

    Ok(Json(SystemSettingsDto {
        outbound_proxy_url: next_net.outbound_proxy_url,
        ssrf_allowlist: next_net.ssrf_allowlist,
        timezone: state.db().get_timezone().await.name().to_string(),
        locale: persisted_locale,
        public_url,
        public_url_env_managed,
    }))
}

// ===========================================================================
// Plugins & Marketplace (`/api/v1/plugins*`)
// ===========================================================================

async fn build_plugin_dto_for_manifest(
    state: &AppState,
    manifest: crate::plugins::PluginManifest,
    source: &str,
    bridge_url: Option<String>,
    row: Option<&crate::db::models::PluginRow>,
) -> AppResult<PluginDto> {
    let registry = state.runtime().plugins();
    let mut instances = Vec::new();
    if manifest.kind == PluginKind::Processor
        && let Some(plugin) = registry.get(&manifest.id)
    {
        let raw_instances = state.db().list_plugin_instances(&manifest.id).await?;
        for inst in raw_instances {
            instances.push(build_plugin_instance_dto(state, plugin.as_ref(), inst).await?);
        }
    }
    let (enabled, installed_version, settings, updated_at) = match row {
        Some(r) => {
            let settings: serde_json::Value =
                serde_json::from_str(&r.settings_json).unwrap_or_else(|_| serde_json::json!({}));
            (
                r.enabled,
                r.installed_version.clone(),
                settings,
                unix_to_rfc3339(r.updated_at),
            )
        }
        None => (
            false,
            manifest.version.clone(),
            serde_json::json!({}),
            unix_to_rfc3339(now_unix()),
        ),
    };
    let (compatible, compatibility_reason) =
        check_plugin_compatibility(&manifest.api_version, manifest.min_core.as_deref());
    Ok(PluginDto {
        id: manifest.id.clone(),
        source: source.to_string(),
        kind: manifest.kind,
        runtime: manifest.runtime,
        bridge_url,
        compatible,
        compatibility_reason,
        manifest,
        enabled,
        installed_version,
        settings,
        instance_count: instances.len(),
        instances,
        updated_at,
    })
}

/// `GET /api/v1/plugins`
pub async fn list_plugins(State(state): State<AppState>) -> AppResult<Json<Vec<PluginDto>>> {
    let registry = state.runtime().plugins();
    let rows = state.db().list_plugins().await?;
    let row_by_id: HashMap<String, _> = rows.into_iter().map(|r| (r.id.clone(), r)).collect();

    let mut out = Vec::new();
    for plugin in registry.list() {
        let manifest = plugin.manifest().clone();
        let row = row_by_id.get(&manifest.id);
        out.push(build_plugin_dto_for_manifest(&state, manifest, "builtin", None, row).await?);
    }
    for bridge in registry.list_bridges() {
        let row = row_by_id.get(&bridge.manifest.id);
        out.push(
            build_plugin_dto_for_manifest(
                &state,
                bridge.manifest,
                "bridge",
                Some(bridge.bridge_url),
                row,
            )
            .await?,
        );
    }
    Ok(Json(out))
}

/// `POST /api/v1/plugins/bridges`
///
/// When `dry_run: true`, performs a signed `GET /manifest` against `bridge_url`, validates
/// Protocol v1 compatibility, and returns a preview without persisting.
/// When `dry_run: false`, encrypts `bridge_secret`, persists the bridge plugin row, reloads
/// the in-memory plugin registry cache, and returns `201 Created`.
pub async fn register_bridge_plugin(
    State(state): State<AppState>,
    Json(req): Json<RegisterBridgePluginRequest>,
) -> AppResult<Response> {
    let normalized_url = validate_bridge_url(&req.bridge_url)?;
    let secret = req.bridge_secret.trim().to_string();
    if secret.is_empty() {
        return Err(AppError::Validation(
            "bridge_secret must not be empty".to_string(),
        ));
    }

    let probe_client = BridgeClient::new(normalized_url.clone(), secret.clone(), "pending".into())?;
    let manifest = probe_client.fetch_manifest().await?;
    let (compatible, compatibility_reason) =
        check_plugin_compatibility(&manifest.api_version, manifest.min_core.as_deref());

    if req.dry_run {
        return Ok((
            StatusCode::OK,
            Json(BridgeManifestPreviewResponse {
                bridge_url: normalized_url,
                manifest,
                compatible,
                compatibility_reason,
            }),
        )
            .into_response());
    }

    let registry = state.runtime().plugins();
    if registry.is_builtin(&manifest.id) || state.db().get_plugin(&manifest.id).await?.is_some() {
        return Err(AppError::Conflict(format!(
            "Plugin '{}' is already installed",
            manifest.id
        )));
    }

    let encrypted_secret = state.master_key().encrypt_json(&secret)?;
    let manifest_json = serde_json::to_string(&manifest)?;
    let row = state
        .db()
        .register_bridge_plugin(
            &manifest.id,
            manifest.kind.as_str(),
            &manifest.version,
            &normalized_url,
            &encrypted_secret,
            &manifest_json,
            req.enabled,
        )
        .await?;
    registry
        .reload_from_db(state.db(), state.master_key())
        .await?;

    tracing::info!(
        plugin_id = %manifest.id,
        plugin_kind = manifest.kind.as_str(),
        version = %manifest.version,
        enabled = req.enabled,
        "registered HTTP bridge plugin"
    );

    let dto =
        build_plugin_dto_for_manifest(&state, manifest, "bridge", Some(normalized_url), Some(&row))
            .await?;
    Ok((StatusCode::CREATED, Json(dto)).into_response())
}

/// Synchronize `channels.capabilities_json` across all existing `ChannelKind::Plugin` accounts
/// belonging to `plugin_id` after a manifest refresh or bridge endpoint update.
async fn sync_plugin_channel_capabilities(
    state: &AppState,
    plugin_id: &str,
    capabilities: &serde_json::Value,
) -> AppResult<()> {
    for ch in state.db().list_channels().await? {
        if ch.kind == ChannelKind::Plugin
            && let Ok(ChannelConfig::Plugin(p)) =
                state.master_key().decrypt_json(&ch.encrypted_config)
            && p.plugin_id == plugin_id
        {
            state
                .db()
                .update_channel_capabilities(ch.id, capabilities)
                .await?;
        }
    }
    Ok(())
}

/// `POST /api/v1/plugins/{id}/refresh-manifest`
pub async fn refresh_bridge_plugin_manifest(
    State(state): State<AppState>,
    Path(plugin_id): Path<String>,
) -> AppResult<Json<PluginDto>> {
    let registry = state.runtime().plugins();
    if registry.is_builtin(&plugin_id) {
        return Err(AppError::Validation(
            "Built-in plugins do not have a remote bridge manifest to refresh".to_string(),
        ));
    }
    let bridge = registry
        .get_bridge(&plugin_id)
        .ok_or_else(|| AppError::NotFound(format!("bridge plugin '{plugin_id}' not found")))?;

    let refreshed = bridge.client.fetch_manifest().await?;
    if refreshed.id != plugin_id {
        return Err(AppError::Validation(format!(
            "Bridge manifest id '{}' does not match registered plugin id '{plugin_id}'",
            refreshed.id
        )));
    }
    if refreshed.kind != bridge.manifest.kind {
        return Err(AppError::Validation(format!(
            "Bridge manifest kind changed from '{}' to '{}'; cross-kind mutation is not allowed on an installed plugin",
            bridge.manifest.kind.as_str(),
            refreshed.kind.as_str()
        )));
    }
    let manifest_json = serde_json::to_string(&refreshed)?;
    let updated = state
        .db()
        .refresh_bridge_plugin_manifest(
            &plugin_id,
            refreshed.kind.as_str(),
            &refreshed.version,
            &manifest_json,
        )
        .await?;
    if refreshed.kind == PluginKind::Channel {
        sync_plugin_channel_capabilities(&state, &plugin_id, &refreshed.capabilities).await?;
    }
    registry
        .reload_from_db(state.db(), state.master_key())
        .await?;

    tracing::info!(
        plugin_id = %plugin_id,
        version = %refreshed.version,
        "refreshed HTTP bridge plugin manifest"
    );

    Ok(Json(
        build_plugin_dto_for_manifest(
            &state,
            refreshed,
            "bridge",
            Some(bridge.bridge_url),
            Some(&updated),
        )
        .await?,
    ))
}

/// `DELETE /api/v1/plugins/{id}`
///
/// Refuses deletion of built-in plugins (`400`) and refuses deletion of referenced bridge plugins
/// (`409 Conflict` listing referencing channels/routes) unless `?cascade=true` is passed.
pub async fn delete_bridge_plugin(
    State(state): State<AppState>,
    Path(plugin_id): Path<String>,
    Query(query): Query<DeleteCascadeQuery>,
) -> AppResult<Response> {
    let registry = state.runtime().plugins();
    if registry.is_builtin(&plugin_id) {
        return Err(AppError::Validation(
            "Built-in plugins cannot be deleted; disable them instead".to_string(),
        ));
    }
    let Some(row) = state.db().get_plugin(&plugin_id).await? else {
        return Err(AppError::NotFound(format!(
            "plugin '{plugin_id}' not found"
        )));
    };
    if row.source != "bridge" {
        return Err(AppError::Validation(
            "Only registered HTTP bridge plugins can be deleted".to_string(),
        ));
    }

    let mut ref_channels: Vec<(i64, String)> = Vec::new();
    for ch in state.db().list_channels().await? {
        if ch.kind == ChannelKind::Plugin
            && let Ok(ChannelConfig::Plugin(p)) =
                state.master_key().decrypt_json(&ch.encrypted_config)
            && p.plugin_id == plugin_id
        {
            ref_channels.push((ch.id, ch.name));
        }
    }
    let ref_ch_ids: Vec<i64> = ref_channels.iter().map(|(id, _)| *id).collect();

    let mut ref_routes: Vec<(i64, String)> = Vec::new();
    let mut strip_route_ids: Vec<i64> = Vec::new();
    for route in state.db().list_routes().await? {
        let uses_channel = ref_ch_ids.contains(&route.channel_id);
        let opts = route.options();
        let uses_processor = opts.plugins.contains_key(&plugin_id)
            || opts.plugin_order.iter().any(|p| p == &plugin_id);
        if uses_channel || uses_processor {
            ref_routes.push((route.id, route.name));
            if uses_processor && !uses_channel {
                strip_route_ids.push(route.id);
            }
        }
    }

    if (!ref_channels.is_empty() || !ref_routes.is_empty()) && !query.cascade.unwrap_or(false) {
        let msg = format!(
            "Bridge plugin '{plugin_id}' is referenced by {} channel(s) and {} route(s). Pass ?cascade=true to remove it together with those references.",
            ref_channels.len(),
            ref_routes.len()
        );
        let body = PluginConflictBody {
            error: msg.clone(),
            code: "conflict",
            message: msg,
            referencing_channels: ref_channels
                .into_iter()
                .map(|(id, name)| ChannelReferenceDto { id, name })
                .collect(),
            referencing_routes: ref_routes
                .into_iter()
                .map(|(id, name)| RouteReferenceDto { id, name })
                .collect(),
        };
        return Ok((StatusCode::CONFLICT, Json(body)).into_response());
    }

    state
        .db()
        .delete_bridge_plugin(&plugin_id, &ref_ch_ids, &strip_route_ids)
        .await?;
    registry
        .reload_from_db(state.db(), state.master_key())
        .await?;

    tracing::info!(
        plugin_id = %plugin_id,
        cascade = query.cascade.unwrap_or(false),
        "deleted HTTP bridge plugin"
    );

    Ok((StatusCode::OK, Json(ActionAckResponse::ok())).into_response())
}

/// `PATCH /api/v1/plugins/{id}`
pub async fn update_plugin(
    State(state): State<AppState>,
    Path(plugin_id): Path<String>,
    Json(req): Json<UpdatePluginRequest>,
) -> AppResult<Json<PluginDto>> {
    let registry = state.runtime().plugins();
    let existing_manifest = registry
        .get_manifest(&plugin_id)
        .ok_or_else(|| AppError::NotFound(format!("plugin '{plugin_id}' not found")))?;
    if registry.is_builtin(&plugin_id) {
        if req.bridge_url.is_some() || req.bridge_secret.is_some() {
            return Err(AppError::Validation(
                "Built-in plugins do not have a bridge_url or bridge_secret".to_string(),
            ));
        }
        registry.sync_builtin_rows(state.db()).await?;
    }

    let settings_str = req
        .settings
        .map(|v| serde_json::to_string(&v))
        .transpose()?;

    let wants_bridge_update = req.bridge_url.is_some()
        || req
            .bridge_secret
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty());

    let (updated, final_manifest) = if wants_bridge_update {
        let row = state
            .db()
            .get_plugin(&plugin_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("plugin '{plugin_id}' not found")))?;
        if row.source != "bridge" {
            return Err(AppError::Validation(
                "Only HTTP bridge plugins support updating bridge_url or rotating bridge_secret"
                    .to_string(),
            ));
        }
        let next_url = match req.bridge_url.as_deref() {
            Some(raw) => validate_bridge_url(raw)?,
            None => row.bridge_url.clone().ok_or_else(|| {
                AppError::Validation("bridge_url is missing on stored plugin".to_string())
            })?,
        };
        let next_secret = match req
            .bridge_secret
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(new_sec) => new_sec.to_string(),
            None => {
                let enc = row.encrypted_bridge_secret.as_deref().ok_or_else(|| {
                    AppError::Validation("bridge_secret is missing on stored plugin".to_string())
                })?;
                state.master_key().decrypt_json::<String>(enc)?
            }
        };

        // Mandatory live signed GET /manifest probe before persisting the rotated secret / URL.
        let probe_client =
            BridgeClient::new(next_url.clone(), next_secret.clone(), plugin_id.clone())?;
        let probed = probe_client.fetch_manifest().await?;
        if probed.id != plugin_id {
            return Err(AppError::Validation(format!(
                "Bridge manifest id '{}' does not match registered plugin id '{plugin_id}'",
                probed.id
            )));
        }
        if probed.kind != existing_manifest.kind {
            return Err(AppError::Validation(format!(
                "Bridge manifest kind changed from '{}' to '{}'; cross-kind mutation is not allowed on an installed plugin",
                existing_manifest.kind.as_str(),
                probed.kind.as_str()
            )));
        }

        let encrypted_secret = state.master_key().encrypt_json(&next_secret)?;
        let manifest_json = serde_json::to_string(&probed)?;
        let updated_row = state
            .db()
            .update_bridge_plugin_connection(
                &plugin_id,
                &next_url,
                &encrypted_secret,
                &probed.version,
                &manifest_json,
                req.enabled,
                settings_str.as_deref(),
            )
            .await?;
        if probed.kind == PluginKind::Channel {
            sync_plugin_channel_capabilities(&state, &plugin_id, &probed.capabilities).await?;
        }
        tracing::info!(
            plugin_id = %plugin_id,
            version = %probed.version,
            rotated_secret = req
                .bridge_secret
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty()),
            "updated HTTP bridge plugin connection after live manifest probe"
        );
        (updated_row, probed)
    } else {
        let updated_row = state
            .db()
            .update_plugin(&plugin_id, req.enabled, settings_str.as_deref())
            .await?;
        (updated_row, existing_manifest)
    };

    registry
        .reload_from_db(state.db(), state.master_key())
        .await?;
    if req.enabled == Some(true) || wants_bridge_update {
        state.runtime().notify_outbox();
        state.runtime().notify_plugin_events();
    }

    let bridge_url = updated.bridge_url.clone();
    let source = updated.source.clone();
    Ok(Json(
        build_plugin_dto_for_manifest(&state, final_manifest, &source, bridge_url, Some(&updated))
            .await?,
    ))
}

/// `GET /api/v1/plugins/{id}/instances`
pub async fn list_plugin_instances(
    State(state): State<AppState>,
    Path(plugin_id): Path<String>,
) -> AppResult<Json<Vec<PluginInstanceDto>>> {
    let plugin = state
        .runtime()
        .plugins()
        .get(&plugin_id)
        .ok_or_else(|| AppError::NotFound(format!("plugin '{plugin_id}' not found")))?;
    let raw = state.db().list_plugin_instances(&plugin_id).await?;
    let mut out = Vec::with_capacity(raw.len());
    for inst in raw {
        out.push(build_plugin_instance_dto(&state, plugin.as_ref(), inst).await?);
    }
    Ok(Json(out))
}

/// `POST /api/v1/plugins/{id}/instances`
pub async fn create_plugin_instance(
    State(state): State<AppState>,
    Path(plugin_id): Path<String>,
    Json(req): Json<CreatePluginInstanceRequest>,
) -> AppResult<(StatusCode, Json<PluginInstanceDto>)> {
    let registry = state.runtime().plugins();
    let plugin = registry
        .get(&plugin_id)
        .ok_or_else(|| AppError::NotFound(format!("plugin '{plugin_id}' not found")))?;
    registry.sync_builtin_rows(state.db()).await?;

    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Validation(
            "Plugin provider name must not be empty".to_string(),
        ));
    }

    let mut validated = plugin.validate_instance_config(&req.config)?;
    if let Some(bridge) = registry.get_bridge(&plugin_id) {
        validated = bridge
            .client
            .validate_remote("instance", &validated)
            .await?;
    }
    let encrypted = state.master_key().encrypt_json(&validated)?;
    let created = state
        .db()
        .create_plugin_instance(&plugin_id, &name, &encrypted, req.enabled)
        .await?;
    let dto = build_plugin_instance_dto(&state, plugin.as_ref(), created).await?;
    Ok((StatusCode::CREATED, Json(dto)))
}

/// `PATCH /api/v1/plugins/{id}/instances/{iid}`
pub async fn update_plugin_instance(
    State(state): State<AppState>,
    Path((plugin_id, instance_id)): Path<(String, i64)>,
    Json(req): Json<UpdatePluginInstanceRequest>,
) -> AppResult<Json<PluginInstanceDto>> {
    let plugin = state
        .runtime()
        .plugins()
        .get(&plugin_id)
        .ok_or_else(|| AppError::NotFound(format!("plugin '{plugin_id}' not found")))?;
    let existing = state
        .db()
        .get_plugin_instance(instance_id)
        .await?
        .filter(|i| i.plugin_id == plugin_id)
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "plugin instance {instance_id} not found for plugin '{plugin_id}'"
            ))
        })?;

    if let Some(ref raw_name) = req.name
        && raw_name.trim().is_empty()
    {
        return Err(AppError::Validation(
            "Plugin provider name must not be empty".to_string(),
        ));
    }

    let mut clear_link_cache = false;
    let next_encrypted = if let Some(ref cfg_patch) = req.config {
        let cur_cfg: serde_json::Value = state
            .master_key()
            .decrypt_json(&existing.encrypted_config)?;
        let mut merged = plugin.merge_instance_config(&cur_cfg, cfg_patch)?;
        if let Some(bridge) = state.runtime().plugins().get_bridge(&plugin_id) {
            merged = bridge.client.validate_remote("instance", &merged).await?;
        }
        if plugin_id == LINK_REWRITE_PLUGIN_ID {
            let old_url = cur_cfg
                .get("api_url")
                .and_then(serde_json::Value::as_str)
                .map(str::trim);
            let new_url = merged
                .get("api_url")
                .and_then(serde_json::Value::as_str)
                .map(str::trim);
            if old_url != new_url {
                clear_link_cache = true;
            }
        }
        Some(state.master_key().encrypt_json(&merged)?)
    } else {
        None
    };

    let updated = state
        .db()
        .update_plugin_instance(
            instance_id,
            req.name.as_deref(),
            next_encrypted.as_deref(),
            req.enabled,
        )
        .await?;
    if clear_link_cache {
        state.db().clear_plugin_link_cache(instance_id).await?;
    }
    Ok(Json(
        build_plugin_instance_dto(&state, plugin.as_ref(), updated).await?,
    ))
}

/// `DELETE /api/v1/plugins/{id}/instances/{iid}`
///
/// Refuses deletion with `409 Conflict` listing referencing routes if any route selects this provider.
pub async fn delete_plugin_instance(
    State(state): State<AppState>,
    Path((plugin_id, instance_id)): Path<(String, i64)>,
) -> AppResult<Response> {
    match state
        .db()
        .delete_plugin_instance_if_unreferenced(&plugin_id, instance_id)
        .await?
    {
        DeleteGuardOutcome::NotFound => Err(AppError::NotFound(format!(
            "plugin instance {instance_id} not found for plugin '{plugin_id}'"
        ))),
        DeleteGuardOutcome::Conflict(refs) => {
            let names: Vec<&str> = refs.iter().map(|(_, n)| n.as_str()).collect();
            let msg = format!(
                "Plugin provider {instance_id} is referenced by {} route(s): {}. Unassign it from those routes before deleting.",
                refs.len(),
                names.join(", ")
            );
            let body = ReferencingConflictBody {
                error: msg.clone(),
                code: "conflict",
                message: msg,
                referencing_routes: refs
                    .into_iter()
                    .map(|(id, name)| RouteReferenceDto { id, name })
                    .collect(),
            };
            Ok((StatusCode::CONFLICT, Json(body)).into_response())
        }
        DeleteGuardOutcome::Deleted => {
            Ok((StatusCode::OK, Json(ActionAckResponse::ok())).into_response())
        }
    }
}

/// `POST /api/v1/plugins/{id}/instances/{iid}/test`
pub async fn test_plugin_instance(
    State(state): State<AppState>,
    Path((plugin_id, instance_id)): Path<(String, i64)>,
) -> AppResult<Json<TestPluginInstanceResponse>> {
    let plugin = state
        .runtime()
        .plugins()
        .get(&plugin_id)
        .ok_or_else(|| AppError::NotFound(format!("plugin '{plugin_id}' not found")))?;
    let existing = state
        .db()
        .get_plugin_instance(instance_id)
        .await?
        .filter(|i| i.plugin_id == plugin_id)
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "plugin instance {instance_id} not found for plugin '{plugin_id}'"
            ))
        })?;
    let cfg: serde_json::Value = state
        .master_key()
        .decrypt_json(&existing.encrypted_config)?;
    let message = plugin.test_instance(state.http(), &cfg).await?;
    Ok(Json(TestPluginInstanceResponse { ok: true, message }))
}

/// `DELETE /api/v1/plugins/{id}/instances/{iid}/cache`
pub async fn clear_plugin_instance_cache(
    State(state): State<AppState>,
    Path((plugin_id, instance_id)): Path<(String, i64)>,
) -> AppResult<Json<ClearPluginCacheResponse>> {
    let existing = state
        .db()
        .get_plugin_instance(instance_id)
        .await?
        .filter(|i| i.plugin_id == plugin_id);
    if existing.is_none() {
        return Err(AppError::NotFound(format!(
            "plugin instance {instance_id} not found for plugin '{plugin_id}'"
        )));
    }
    let cleared_entries = state.db().clear_plugin_link_cache(instance_id).await?;
    Ok(Json(ClearPluginCacheResponse { cleared_entries }))
}

/// `GET /api/v1/plugins/market`
pub async fn get_plugin_market(
    State(state): State<AppState>,
    Query(query): Query<MarketQuery>,
) -> AppResult<Json<MarketResponse>> {
    let registry = state.runtime().plugins();
    let installed_rows: HashMap<String, bool> = state
        .db()
        .list_plugins()
        .await?
        .into_iter()
        .map(|r| (r.id, r.enabled))
        .collect();

    let market_cache = state.runtime().market_cache();
    let (doc, source) = market_cache
        .get_index(state.http(), query.refresh.unwrap_or(false))
        .await;

    let plugins = doc
        .plugins
        .into_iter()
        .map(|item| {
            let is_builtin = registry.is_builtin(&item.id);
            let is_installed = is_builtin || installed_rows.contains_key(&item.id);
            let enabled = installed_rows.get(&item.id).copied().unwrap_or(false);
            let (compatible, compatibility_reason) =
                check_plugin_compatibility(&item.api_version, item.min_core.as_deref());
            let runtime = item.runtime.or_else(|| {
                if is_builtin || item.kind == "builtin" {
                    Some(PluginRuntime::Builtin.as_str().to_string())
                } else {
                    None
                }
            });
            MarketPluginItemDto {
                id: item.id,
                name: item.name,
                version: item.version,
                api_version: item.api_version,
                kind: if is_builtin {
                    "builtin".to_string()
                } else {
                    item.kind
                },
                runtime,
                min_core: item.min_core,
                category: item.category,
                icon: item.icon,
                description: item.description,
                homepage: item.homepage,
                repository: item.repository,
                image: item.image,
                compose_snippet: item.compose_snippet,
                hooks: item.hooks,
                permissions: item.permissions,
                compatible,
                compatibility_reason,
                installed: is_installed,
                enabled,
            }
        })
        .collect();

    Ok(Json(MarketResponse {
        source: source.to_string(),
        index_url: market_cache.index_url().to_string(),
        generated_at: doc.generated_at,
        plugins,
    }))
}

async fn build_plugin_instance_dto(
    state: &AppState,
    plugin: &dyn crate::plugins::Plugin,
    inst: crate::db::models::PluginInstanceRow,
) -> AppResult<PluginInstanceDto> {
    let decrypted: serde_json::Value = state.master_key().decrypt_json(&inst.encrypted_config)?;
    let redacted = plugin.redact_instance_config(&decrypted);
    let cache_entries = state
        .db()
        .count_plugin_link_cache_by_instance(inst.id)
        .await?;
    Ok(PluginInstanceDto {
        id: inst.id,
        plugin_id: inst.plugin_id,
        name: inst.name,
        config: redacted,
        enabled: inst.enabled,
        cache_entries,
        created_at: unix_to_rfc3339(inst.created_at),
        updated_at: unix_to_rfc3339(inst.updated_at),
    })
}

// ===========================================================================
// SMTP Per-Recipient Compliance (`/api/v1/destinations/{id}/recipients*`)
// ===========================================================================

const SMTP_VERIFY_CODE_TTL_SECS: i64 = 30 * 60;
const SMTP_VERIFY_RESEND_COOLDOWN_SECS: i64 = 60;
const SMTP_VERIFY_HOURLY_CAP: i64 = 5;
const SMTP_VERIFY_MAX_ATTEMPTS: i64 = 5;

async fn load_smtp_destination_and_map(
    state: &AppState,
    destination_id: i64,
) -> AppResult<(
    Channel,
    ChannelDestination,
    ChannelConfig,
    HashMap<String, String>,
)> {
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
    if channel.kind != ChannelKind::Smtp {
        return Err(AppError::Validation(format!(
            "destination {destination_id} belongs to a '{}' channel, expected 'smtp'",
            channel.kind
        )));
    }
    let channel_cfg: ChannelConfig = state.master_key().decrypt_json(&channel.encrypted_config)?;
    let dest_cfg: DestinationConfig = state.master_key().decrypt_json(&dest.encrypted_config)?;
    sync_smtp_destination_recipients(state, dest.id, &dest_cfg).await?;

    let mut map = HashMap::new();
    if let DestinationConfig::Smtp(smtp_dest) = dest_cfg {
        for addr in smtp_dest.to_addresses {
            let trimmed = addr.trim();
            if trimmed.is_empty() {
                continue;
            }
            let h = state.master_key().smtp_recipient_hmac(trimmed);
            map.entry(h).or_insert_with(|| trimmed.to_string());
        }
    }
    Ok((channel, dest, channel_cfg, map))
}

fn recipient_row_to_dto(row: SmtpRecipientRow, email: String) -> SmtpRecipientDto {
    SmtpRecipientDto {
        id: row.id,
        destination_id: row.destination_id,
        email,
        status: row.status,
        code_expires_at: row.code_expires_at.map(unix_to_rfc3339),
        code_attempts: row.code_attempts,
        last_code_sent_at: row.last_code_sent_at.map(unix_to_rfc3339),
        consent_method: row.consent_method,
        consented_at: row.consented_at.map(unix_to_rfc3339),
        revoked_method: row.revoked_method,
        revoked_at: row.revoked_at.map(unix_to_rfc3339),
        bounce_diagnostic: row.bounce_diagnostic,
        created_at: unix_to_rfc3339(row.created_at),
        updated_at: unix_to_rfc3339(row.updated_at),
    }
}

/// `GET /api/v1/destinations/{id}/recipients`
pub async fn list_destination_recipients(
    State(state): State<AppState>,
    Path(destination_id): Path<i64>,
) -> AppResult<Json<Vec<SmtpRecipientDto>>> {
    let (_, _, _, email_by_hmac) = load_smtp_destination_and_map(&state, destination_id).await?;
    let rows = state.db().list_smtp_recipients(destination_id).await?;
    let out = rows
        .into_iter()
        .map(|r| {
            let email = email_by_hmac
                .get(&r.address_hmac)
                .cloned()
                .unwrap_or_else(|| "(unknown)".to_string());
            recipient_row_to_dto(r, email)
        })
        .collect();
    Ok(Json(out))
}

/// `POST /api/v1/destinations/{id}/recipients/{rid}/send-verification`
pub async fn send_recipient_verification(
    State(state): State<AppState>,
    Path((destination_id, recipient_id)): Path<(i64, i64)>,
) -> AppResult<Json<SendVerificationResponse>> {
    let (_, dest, channel_cfg, email_by_hmac) =
        load_smtp_destination_and_map(&state, destination_id).await?;
    let ChannelConfig::Smtp(smtp_cfg) = channel_cfg else {
        return Err(AppError::Validation(
            "expected SMTP channel config".to_string(),
        ));
    };

    let now = now_unix();
    let code_num: u32 = rand::random_range(0..1_000_000);
    let code = format!("{code_num:06}");
    let code_sha256 = sha256_hex(code.as_bytes());
    let expires_at = now.saturating_add(SMTP_VERIFY_CODE_TTL_SECS);

    // Atomically check recipient status + 60s resend cooldown + 5/h cap and reserve the slot on
    // `write_pool` BEFORE talking to SMTP, eliminating concurrent double-click rate-limit bypass.
    let (prev_row, reserved_row) = state
        .db()
        .reserve_smtp_recipient_verification_code(
            destination_id,
            recipient_id,
            &code_sha256,
            expires_at,
            now,
            SMTP_VERIFY_RESEND_COOLDOWN_SECS,
            SMTP_VERIFY_HOURLY_CAP,
        )
        .await?;

    let email = match email_by_hmac.get(&reserved_row.address_hmac).cloned() {
        Some(e) => e,
        None => {
            let _ = state
                .db()
                .rollback_smtp_recipient_verification_reservation(&prev_row, &code_sha256)
                .await;
            return Err(AppError::NotFound(
                "Recipient address no longer present in destination".to_string(),
            ));
        }
    };

    let (public_url, _) = match state.db().get_effective_public_url(state.config()).await {
        Ok(res) => res,
        Err(err) => {
            let _ = state
                .db()
                .rollback_smtp_recipient_verification_reservation(&prev_row, &code_sha256)
                .await;
            return Err(err);
        }
    };
    let verify_url = public_url.as_deref().map(|base| {
        let tok = state.master_key().sign_verify_token(
            destination_id,
            recipient_id,
            expires_at,
            &code_sha256,
        );
        format!("{base}/v/{tok}")
    });

    let locale = state.runtime().locale();
    let send_msg = match state
        .runtime()
        .smtp_sender()
        .send_verification_email(
            &smtp_cfg,
            &email,
            &dest.name,
            &code,
            verify_url.as_deref(),
            locale,
        )
        .await
    {
        Ok(msg) => msg,
        Err(err) => {
            let _ = state
                .db()
                .rollback_smtp_recipient_verification_reservation(&prev_row, &code_sha256)
                .await;
            return Err(err);
        }
    };

    Ok(Json(SendVerificationResponse {
        ok: true,
        message: send_msg,
        recipient: recipient_row_to_dto(reserved_row, email),
    }))
}

/// `POST /api/v1/destinations/{id}/recipients/{rid}/verify`
pub async fn verify_recipient_code(
    State(state): State<AppState>,
    Path((destination_id, recipient_id)): Path<(i64, i64)>,
    Json(req): Json<VerifyRecipientCodeRequest>,
) -> AppResult<Json<SmtpRecipientDto>> {
    let (_, _, _, email_by_hmac) = load_smtp_destination_and_map(&state, destination_id).await?;
    let row = state
        .db()
        .get_smtp_recipient(recipient_id)
        .await?
        .filter(|r| r.destination_id == destination_id)
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "SMTP recipient {recipient_id} not found on destination {destination_id}"
            ))
        })?;
    let email = email_by_hmac
        .get(&row.address_hmac)
        .cloned()
        .unwrap_or_else(|| "(unknown)".to_string());

    if row.status == SmtpRecipientStatus::Verified {
        return Ok(Json(recipient_row_to_dto(row, email)));
    }
    if row.status != SmtpRecipientStatus::Pending {
        return Err(AppError::Validation(
            "No active verification code for this recipient; send a verification email first"
                .to_string(),
        ));
    }

    let now = now_unix();
    if row.code_expires_at.is_none_or(|exp| now > exp) {
        return Err(AppError::Validation(
            "Verification code has expired; please send a new verification email".to_string(),
        ));
    }
    if row.code_attempts >= SMTP_VERIFY_MAX_ATTEMPTS {
        return Err(AppError::RateLimited {
            message: format!(
                "Maximum verification attempts ({SMTP_VERIFY_MAX_ATTEMPTS}) exceeded; send a new verification code"
            ),
            retry_after_secs: None,
        });
    }

    let provided_hash = sha256_hex(req.code.trim().as_bytes());
    let expected_hash = row.code_sha256.as_deref().unwrap_or("");
    if !constant_time_eq(provided_hash.as_bytes(), expected_hash.as_bytes()) {
        let attempts = state
            .db()
            .increment_smtp_recipient_code_attempts_and_lock(recipient_id, SMTP_VERIFY_MAX_ATTEMPTS)
            .await?;
        let remaining = SMTP_VERIFY_MAX_ATTEMPTS.saturating_sub(attempts);
        return Err(AppError::Validation(format!(
            "Invalid verification code ({remaining} attempt(s) remaining)"
        )));
    }

    let verified = state
        .db()
        .mark_smtp_recipient_verified(recipient_id, "code", now)
        .await?;
    state
        .db()
        .release_deferred_tasks_for_destination(destination_id, now)
        .await?;
    state.runtime().notify_outbox();

    Ok(Json(recipient_row_to_dto(verified, email)))
}

/// `POST /api/v1/destinations/{id}/recipients/{rid}/reactivate`
pub async fn reactivate_recipient(
    State(state): State<AppState>,
    Path((destination_id, recipient_id)): Path<(i64, i64)>,
) -> AppResult<Json<SmtpRecipientDto>> {
    let (_, _, _, email_by_hmac) = load_smtp_destination_and_map(&state, destination_id).await?;
    let row = state
        .db()
        .get_smtp_recipient(recipient_id)
        .await?
        .filter(|r| r.destination_id == destination_id)
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "SMTP recipient {recipient_id} not found on destination {destination_id}"
            ))
        })?;
    if !matches!(
        row.status,
        SmtpRecipientStatus::Unsubscribed | SmtpRecipientStatus::Bounced
    ) {
        return Err(AppError::Validation(
            "Only unsubscribed or bounced recipients can be reactivated".to_string(),
        ));
    }

    let updated = state
        .db()
        .reactivate_smtp_recipient(recipient_id, now_unix())
        .await?;
    let email = email_by_hmac
        .get(&updated.address_hmac)
        .cloned()
        .unwrap_or_else(|| "(unknown)".to_string());
    Ok(Json(recipient_row_to_dto(updated, email)))
}

/// `POST /api/v1/destinations/{id}/recipients/{rid}/unsubscribe`
pub async fn unsubscribe_recipient_admin(
    State(state): State<AppState>,
    Path((destination_id, recipient_id)): Path<(i64, i64)>,
) -> AppResult<Json<SmtpRecipientDto>> {
    let (_, _, _, email_by_hmac) = load_smtp_destination_and_map(&state, destination_id).await?;
    let existing = state
        .db()
        .get_smtp_recipient(recipient_id)
        .await?
        .filter(|r| r.destination_id == destination_id)
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "SMTP recipient {recipient_id} not found on destination {destination_id}"
            ))
        })?;
    let _ = existing;

    let (updated, _) = state
        .db()
        .mark_smtp_recipient_unsubscribed(recipient_id, "admin", now_unix())
        .await?;
    let email = email_by_hmac
        .get(&updated.address_hmac)
        .cloned()
        .unwrap_or_else(|| "(unknown)".to_string());
    Ok(Json(recipient_row_to_dto(updated, email)))
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
    tracing::info!(
        token_id = created.id,
        token_name = %created.name,
        scope = created.scope.as_str(),
        "issued MCP bearer token"
    );

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
    tracing::info!(token_id, "revoked MCP bearer token");
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
