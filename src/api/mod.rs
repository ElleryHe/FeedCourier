//! REST API (`/api/v1/*`), HttpOnly session authentication, and `rust-embed` static WebUI router
//! (`feedcourier.md` Sections 二.2 & 六.2).

pub mod account;
pub mod auth;
pub mod dto;
pub mod handlers;
pub mod public;
pub mod spa;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{delete, get, patch, post};
use http as _;
use http_body_util as _;
use tower as _;
use tower_http::trace::TraceLayer;

use crate::config::AppConfig;
use crate::crypto::{MasterKey, hash_password};
use crate::db::Database;
use crate::db::models::{
    SETTING_ADMIN_PASSWORD_HASH, SETTING_ADMIN_USERNAME, SETTING_LOCALE,
    SETTING_OUTBOUND_PROXY_URL, SETTING_PUBLIC_URL, SETTING_SSRF_ALLOWLIST, SETTING_TIMEZONE,
};
use crate::error::AppResult;
use crate::ingest::IngestEngine;
use crate::mcp::{McpState, build_mcp_router_with_state};
use crate::net::ProtectedHttpClient;
use crate::schedule::DEFAULT_TIMEZONE;
use crate::scheduler::DeliveryDispatcher;

/// Shared Axum application state (`Clone` via `DeliveryDispatcher`'s internal `Arc`).
#[derive(Debug, Clone)]
pub struct AppState {
    runtime: DeliveryDispatcher,
}

impl AppState {
    /// Wrap an initialized [`DeliveryDispatcher`] runtime.
    pub fn new(runtime: DeliveryDispatcher) -> Self {
        Self { runtime }
    }

    /// Construct [`AppState`] loading persisted network settings from SQLite `system_settings`.
    pub async fn from_parts(
        config: AppConfig,
        db: Database,
        master_key: MasterKey,
    ) -> AppResult<Self> {
        let runtime = DeliveryDispatcher::from_db_settings(config, db, master_key).await?;
        Ok(Self { runtime })
    }

    pub fn runtime(&self) -> &DeliveryDispatcher {
        &self.runtime
    }

    pub fn config(&self) -> &AppConfig {
        self.runtime.config()
    }

    pub fn db(&self) -> &Database {
        self.runtime.db()
    }

    pub fn master_key(&self) -> &MasterKey {
        self.runtime.master_key()
    }

    pub fn http(&self) -> &ProtectedHttpClient {
        self.runtime.http()
    }

    pub fn ingest(&self) -> &IngestEngine {
        self.runtime.ingest()
    }
}

/// Build a baseline health/metadata router (preserved for lightweight smoke checks).
pub fn baseline_router() -> Router {
    Router::new().route("/api/v1/health", get(handlers::health_check))
}

/// Build the `/api/v1/*` REST API router without attaching the SPA fallback handler.
pub fn build_api_only_router(state: AppState) -> Router {
    let public_routes = Router::new()
        .route("/api/v1/health", get(handlers::health_check))
        .route("/api/v1/auth/status", get(auth::auth_status))
        .route("/api/v1/auth/setup", post(auth::auth_setup))
        .route("/api/v1/auth/login", post(auth::auth_login))
        .route("/api/v1/auth/login/2fa", post(auth::auth_login_2fa))
        .route("/api/v1/auth/logout", post(auth::auth_logout))
        // Public SMTP compliance endpoints (RFC 8058 unsubscribe + double opt-in confirmation).
        // Empty `/u/` and `/v/` paths return a clean public 404 rather than falling through to SPA.
        .route(
            "/u",
            get(public::public_not_found).post(public::public_not_found),
        )
        .route(
            "/u/",
            get(public::public_not_found).post(public::public_not_found),
        )
        .route(
            "/v",
            get(public::public_not_found).post(public::public_not_found),
        )
        .route(
            "/v/",
            get(public::public_not_found).post(public::public_not_found),
        )
        .route(
            "/u/{token}",
            get(public::get_unsubscribe_page)
                .post(public::post_unsubscribe)
                .layer(DefaultBodyLimit::max(4096)),
        )
        .route(
            "/v/{token}",
            get(public::get_verify_page)
                .post(public::post_verify)
                .layer(DefaultBodyLimit::max(4096)),
        );

    let protected_routes = Router::new()
        // Overview & Maintenance
        .route("/api/v1/overview", get(handlers::get_overview))
        .route("/api/v1/maintenance/gc", post(handlers::run_maintenance_gc))
        .route("/api/v1/maintenance/backup", get(handlers::download_backup))
        // Feeds & OPML
        .route(
            "/api/v1/feeds",
            get(handlers::list_feeds).post(handlers::create_feed),
        )
        .route("/api/v1/feeds/preview", post(handlers::preview_feed))
        .route(
            "/api/v1/feeds/preview/push",
            post(handlers::debug_push_feed),
        )
        .route("/api/v1/feeds/opml/import", post(handlers::import_opml))
        .route("/api/v1/feeds/opml/export", get(handlers::export_opml))
        .route(
            "/api/v1/feeds/{id}",
            patch(handlers::update_feed).delete(handlers::delete_feed),
        )
        .route("/api/v1/feeds/{id}/poll", post(handlers::poll_feed_now))
        .route(
            "/api/v1/feeds/{id}/clear-cache",
            post(handlers::clear_feed_cache),
        )
        // Trusted Channels (Accounts) & Destinations + Notion Onboarding
        .route(
            "/api/v1/channels",
            get(handlers::list_channels).post(handlers::create_channel),
        )
        .route(
            "/api/v1/channels/notion/init-db",
            post(handlers::notion_init_db),
        )
        .route(
            "/api/v1/channels/notion/inspect-schema",
            post(handlers::notion_inspect_schema),
        )
        .route(
            "/api/v1/channels/{id}",
            patch(handlers::update_channel).delete(handlers::delete_channel),
        )
        .route("/api/v1/channels/{id}/test", post(handlers::test_channel))
        .route(
            "/api/v1/channels/{id}/destinations",
            get(handlers::list_channel_destinations).post(handlers::create_destination),
        )
        .route(
            "/api/v1/destinations/{id}",
            patch(handlers::update_destination).delete(handlers::delete_destination),
        )
        .route(
            "/api/v1/destinations/{id}/test",
            post(handlers::test_destination),
        )
        // SMTP Per-Recipient Compliance
        .route(
            "/api/v1/destinations/{id}/recipients",
            get(handlers::list_destination_recipients),
        )
        .route(
            "/api/v1/destinations/{id}/recipients/{rid}/send-verification",
            post(handlers::send_recipient_verification),
        )
        .route(
            "/api/v1/destinations/{id}/recipients/{rid}/verify",
            post(handlers::verify_recipient_code),
        )
        .route(
            "/api/v1/destinations/{id}/recipients/{rid}/reactivate",
            post(handlers::reactivate_recipient),
        )
        .route(
            "/api/v1/destinations/{id}/recipients/{rid}/unsubscribe",
            post(handlers::unsubscribe_recipient_admin),
        )
        // Plugins & Marketplace
        .route("/api/v1/plugins", get(handlers::list_plugins))
        .route("/api/v1/plugins/market", get(handlers::get_plugin_market))
        .route("/api/v1/plugins/{id}", patch(handlers::update_plugin))
        .route(
            "/api/v1/plugins/{id}/instances",
            get(handlers::list_plugin_instances).post(handlers::create_plugin_instance),
        )
        .route(
            "/api/v1/plugins/{id}/instances/{iid}",
            patch(handlers::update_plugin_instance).delete(handlers::delete_plugin_instance),
        )
        .route(
            "/api/v1/plugins/{id}/instances/{iid}/test",
            post(handlers::test_plugin_instance),
        )
        .route(
            "/api/v1/plugins/{id}/instances/{iid}/cache",
            delete(handlers::clear_plugin_instance_cache),
        )
        // Routes
        .route(
            "/api/v1/routes",
            get(handlers::list_routes).post(handlers::create_route),
        )
        .route(
            "/api/v1/routes/{id}",
            patch(handlers::update_route).delete(handlers::delete_route),
        )
        // Outbox & Dead-Letter Tasks
        .route("/api/v1/tasks", get(handlers::list_tasks))
        .route("/api/v1/tasks/retry", post(handlers::retry_tasks))
        .route(
            "/api/v1/tasks/queue",
            get(handlers::get_queue_state).put(handlers::update_queue_state),
        )
        // Account & Security
        .route(
            "/api/v1/account",
            get(account::get_account).patch(account::update_account),
        )
        .route("/api/v1/account/2fa/setup", post(account::totp_setup))
        .route("/api/v1/account/2fa/enable", post(account::totp_enable))
        .route("/api/v1/account/2fa/disable", post(account::totp_disable))
        .route(
            "/api/v1/account/2fa/recovery-codes",
            post(account::regenerate_recovery_codes),
        )
        // System Settings & MCP Bearer Tokens
        .route(
            "/api/v1/settings",
            get(handlers::get_settings).put(handlers::update_settings),
        )
        .route(
            "/api/v1/mcp-tokens",
            get(handlers::list_mcp_tokens).post(handlers::create_mcp_token),
        )
        .route(
            "/api/v1/mcp-tokens/{id}",
            delete(handlers::delete_mcp_token),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_session,
        ));

    public_routes.merge(protected_routes).with_state(state)
}

/// Build the full Axum router combining `/api/v1/*` endpoints, `fc_session` authentication,
/// JSON `404` responses for unknown `/api/*` paths, and `rust-embed` static SPA WebUI serving.
pub fn build_api_router(state: AppState) -> Router {
    build_api_only_router(state).fallback(spa::serve_spa)
}

/// Bootstrap optional environment variable presets (`ADMIN_PASSWORD`, `SSRF_ALLOWLIST`,
/// `OUTBOUND_PROXY`) into SQLite `system_settings` before initializing [`DeliveryDispatcher`].
pub async fn bootstrap_env_settings(config: &AppConfig, db: &Database) -> AppResult<()> {
    if let Some(ref admin_pw) = config.admin_password
        && !admin_pw.is_empty()
    {
        let phc = hash_password(admin_pw)?;
        db.set_setting_typed(SETTING_ADMIN_PASSWORD_HASH, &phc)
            .await?;
    }
    if !config.ssrf_allowlist.is_empty() && db.get_setting(SETTING_SSRF_ALLOWLIST).await?.is_none()
    {
        db.set_setting_typed(SETTING_SSRF_ALLOWLIST, &config.ssrf_allowlist)
            .await?;
    }
    if let Some(ref proxy_url) = config.outbound_proxy
        && db.get_setting(SETTING_OUTBOUND_PROXY_URL).await?.is_none()
    {
        db.set_setting_typed(SETTING_OUTBOUND_PROXY_URL, proxy_url)
            .await?;
    }
    if let Some(locale) = config.locale
        && db.get_setting(SETTING_LOCALE).await?.is_none()
    {
        db.set_setting_typed(SETTING_LOCALE, &locale.as_str())
            .await?;
    }
    if let Some(ref username) = config.admin_username {
        db.set_setting_typed(SETTING_ADMIN_USERNAME, &username.trim().to_string())
            .await?;
    }
    if db.get_setting(SETTING_TIMEZONE).await?.is_none() {
        let tz = config
            .timezone
            .clone()
            .unwrap_or_else(|| DEFAULT_TIMEZONE.to_string());
        db.set_setting_typed(SETTING_TIMEZONE, &tz).await?;
    }
    if let Some(ref public_url) = config.public_url
        && db.get_setting(SETTING_PUBLIC_URL).await?.is_none()
    {
        db.set_setting_typed(SETTING_PUBLIC_URL, public_url).await?;
    }
    if config.reset_2fa {
        account::clear_totp_state_db(db).await?;
        tracing::warn!(
            "RESET_2FA is set: the administrator TOTP secret and recovery codes were removed; \
             unset RESET_2FA after this start and re-enable 2FA in Settings"
        );
    }
    Ok(())
}

/// Build the unified production Axum router combining:
/// - `/mcp` Streamable HTTP MCP server (`Bearer` token guard via `route_layer`)
/// - `/api/v1/*` REST API (`fc_session` HttpOnly cookie guard on protected routes)
/// - JSON `404` responses for unknown `/api/*` and `/mcp/*` paths
/// - `rust-embed` static React 19 single-page WebUI (`/` and client-side deep links)
pub fn build_full_router(app_state: AppState, mcp_state: McpState) -> Router {
    build_api_only_router(app_state)
        .merge(build_mcp_router_with_state(mcp_state))
        .fallback(spa::serve_spa)
        .layer(TraceLayer::new_for_http())
}
