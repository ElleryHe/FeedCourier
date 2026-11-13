//! FeedCourier binary entrypoint.

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use feedcourier::api::{AppState, bootstrap_env_settings, build_full_router};
use feedcourier::config::{APP_NAME, AppConfig, LICENSE_SPDX, REPO_URL, VERSION};
use feedcourier::crypto::MasterKey;
use feedcourier::db::Database;
use feedcourier::error::AppResult;
use feedcourier::mcp::McpState;
use feedcourier::scheduler::DeliveryDispatcher;

#[tokio::main]
async fn main() -> AppResult<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("feedcourier=info,tower_http=info")),
        )
        .init();

    let config = AppConfig::from_env()?;
    let master_key = MasterKey::from_config(&config)?;
    let db = Database::from_config(&config).await?;

    // Bootstrap optional environment presets (`ADMIN_PASSWORD`, `SSRF_ALLOWLIST`,
    // `OUTBOUND_PROXY`) into `system_settings` BEFORE initializing `DeliveryDispatcher`
    // so persisted DB settings are active immediately on startup.
    bootstrap_env_settings(&config, &db).await?;

    let runtime =
        DeliveryDispatcher::from_db_settings(config.clone(), db.clone(), master_key.clone())
            .await?;
    let scheduler_handle = runtime.spawn();

    let mcp_cancel = scheduler_handle.cancel_token().child_token();
    let mcp_state = McpState::new(db.clone(), runtime.ingest().clone())
        .with_outbox_notify(runtime.outbox_notify())
        .with_cancellation_token(mcp_cancel.clone());
    let app_state = AppState::new(runtime);
    let app = build_full_router(app_state, mcp_state);

    let listener = TcpListener::bind(config.listen_addr).await?;
    let bound_addr = listener.local_addr()?;
    tracing::info!(
        app = APP_NAME,
        version = VERSION,
        license = LICENSE_SPDX,
        repo = REPO_URL,
        listen_addr = %bound_addr,
        data_dir = %config.data_dir.display(),
        "starting FeedCourier full-stack server"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(mcp_cancel))
        .await?;

    tracing::info!("HTTP server drained; stopping background scheduler");
    scheduler_handle.shutdown().await;
    db.close().await;
    tracing::info!("FeedCourier shut down cleanly");

    Ok(())
}

async fn shutdown_signal(mcp_cancel: CancellationToken) {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %err, "failed to install SIGINT handler");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(err) => {
                tracing::error!(error = %err, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {
            tracing::info!("SIGINT received, draining HTTP connections");
        }
        () = terminate => {
            tracing::info!("SIGTERM received, draining HTTP connections");
        }
    }

    // Cancel open Streamable HTTP `/mcp` SSE streams so `axum::serve` drains without hanging.
    mcp_cancel.cancel();
}
