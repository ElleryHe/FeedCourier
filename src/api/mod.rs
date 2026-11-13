//! REST API (`/api/v1/*`), HttpOnly session authentication, and `rust-embed` static WebUI router
//! (`feedcourier.md` Sections 二.2 & 六.2).

pub mod auth;
pub mod dto;
pub mod handlers;
pub mod spa;

use axum::Router;
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
    SETTING_ADMIN_PASSWORD_HASH, SETTING_OUTBOUND_PROXY_URL, SETTING_SSRF_ALLOWLIST,
};
use crate::error::AppResult;
use crate::ingest::IngestEngine;
use crate::mcp::{McpState, build_mcp_router_with_state};
use crate::net::ProtectedHttpClient;
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
        .route("/api/v1/auth/logout", post(auth::auth_logout));

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
        .route("/api/v1/feeds/opml/import", post(handlers::import_opml))
        .route("/api/v1/feeds/opml/export", get(handlers::export_opml))
        .route(
            "/api/v1/feeds/{id}",
            patch(handlers::update_feed).delete(handlers::delete_feed),
        )
        .route("/api/v1/feeds/{id}/poll", post(handlers::poll_feed_now))
        // Trusted Channels & Notion Onboarding
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
