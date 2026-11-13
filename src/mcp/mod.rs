//! Built-in Model Context Protocol (`/mcp`) Streamable HTTP endpoint, dual-scope Bearer token
//! guard (`readonly` / `admin`), AGPLv3 `ServerConfig` (`ServerInfo`) metadata, prompt-injection
//! framing (`<untrusted_feed_content>`), and all 7 operational tools (`feedcourier.md` Section 六).

pub mod auth;
pub mod sanitize;
pub mod tools;

use std::sync::Arc;

use axum::{Router, middleware};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::db::Database;
use crate::ingest::IngestEngine;

pub use self::auth::{
    AuthenticatedMcpToken, CURRENT_MCP_TOKEN, MCP_WWW_AUTHENTICATE, mcp_bearer_auth_middleware,
    parse_bearer_token,
};
pub use self::sanitize::{
    MAX_UNTRUSTED_SHORT_CHARS, MAX_UNTRUSTED_SUMMARY_CHARS, UNTRUSTED_WARNING_ATTR,
    wrap_optional_untrusted_text, wrap_untrusted_feed_text,
};
pub use self::tools::{McpServer, build_mcp_instructions};

/// Canonical HTTP mount path for the Streamable HTTP MCP server.
pub const MCP_MOUNT_PATH: &str = "/mcp";

/// Shared application state injected into the `/mcp` Streamable HTTP service.
#[derive(Debug, Clone)]
pub struct McpState {
    db: Database,
    ingest: IngestEngine,
    outbox_notify: Option<Arc<Notify>>,
    cancellation_token: CancellationToken,
}

impl McpState {
    /// Construct an [`McpState`] backed by `db` and `ingest`.
    pub fn new(db: Database, ingest: IngestEngine) -> Self {
        Self {
            db,
            ingest,
            outbox_notify: None,
            cancellation_token: CancellationToken::new(),
        }
    }

    /// Attach an outbox dispatcher wakeup handle so `reset_and_poll_feed`, `retry_dead_tasks`,
    /// and `manage_feed` (`add` with `push_latest_one`) wake the outbox worker immediately.
    pub fn with_outbox_notify(mut self, notify: Arc<Notify>) -> Self {
        self.outbox_notify = Some(notify);
        self
    }

    /// Attach an optional outbox dispatcher wakeup handle.
    pub fn with_optional_outbox_notify(mut self, notify: Option<Arc<Notify>>) -> Self {
        self.outbox_notify = notify;
        self
    }

    /// Attach a cooperative shutdown [`CancellationToken`] for active Streamable HTTP SSE streams.
    pub fn with_cancellation_token(mut self, token: CancellationToken) -> Self {
        self.cancellation_token = token;
        self
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    pub fn ingest(&self) -> &IngestEngine {
        &self.ingest
    }

    pub fn outbox_notify(&self) -> Option<&Arc<Notify>> {
        self.outbox_notify.as_ref()
    }

    /// Wake the background outbox dispatcher if an `outbox_notify` handle is configured.
    pub fn notify_outbox(&self) {
        if let Some(notify) = &self.outbox_notify {
            notify.notify_one();
        }
    }
}

/// Build an Axum [`Router`] mounting the authenticated Streamable HTTP MCP endpoint at
/// [`MCP_MOUNT_PATH`] (`"/mcp"`).
pub fn build_mcp_router(db: Database, ingest: IngestEngine) -> Router {
    build_mcp_router_with_state(McpState::new(db, ingest))
}

/// Build an Axum [`Router`] mounting `/mcp` using an explicit [`McpState`] (including optional
/// outbox [`Notify`] wakeup and shutdown [`CancellationToken`]).
pub fn build_mcp_router_with_state(state: McpState) -> Router {
    let config = StreamableHttpServerConfig::default()
        .disable_allowed_hosts()
        .with_json_response(true)
        .with_cancellation_token(state.cancellation_token.clone());

    let session_manager = Arc::new(LocalSessionManager::default());
    let auth_db = state.db.clone();
    let mcp_service = StreamableHttpService::new(
        move || Ok(McpServer::new(state.clone())),
        session_manager,
        config,
    );

    Router::new()
        .route_service(MCP_MOUNT_PATH, mcp_service.clone())
        .route_service("/mcp/", mcp_service)
        .route_layer(middleware::from_fn_with_state(
            auth_db,
            mcp_bearer_auth_middleware,
        ))
}
