//! FeedCourier binary entrypoint.

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use feedcourier::api::{AppState, bootstrap_env_settings, build_full_router};
use feedcourier::config::{APP_NAME, AppConfig, LICENSE_SPDX, REPO_URL, VERSION};
use feedcourier::crypto::MasterKey;
use feedcourier::db::Database;
use feedcourier::error::AppResult;
use feedcourier::logging::init_logging;
use feedcourier::mcp::McpState;
use feedcourier::scheduler::DeliveryDispatcher;

#[tokio::main]
async fn main() -> AppResult<()> {
    let config = AppConfig::from_env()?;
    let log_store = init_logging(&config).await;

    let master_key = MasterKey::from_config(&config)?;
    let db = Database::from_config(&config).await?;

    // Bootstrap optional environment presets (`ADMIN_PASSWORD`, `SSRF_ALLOWLIST`,
    // `OUTBOUND_PROXY`) into `system_settings` BEFORE initializing `DeliveryDispatcher`
    // so persisted DB settings are active immediately on startup.
    bootstrap_env_settings(&config, &db).await?;

    // Upgrade pre-v0.2.0 single-layer channels into the two-layer Account + Destinations model
    // atomically (taking a `pre-v0.2.0-backup.db` snapshot first when legacy rows exist).
    db.migrate_channels_to_v2(&master_key).await?;

    // Backfill existing pre-v0.3.0 SMTP destination recipients as `verified (legacy)` idempotently.
    db.backfill_legacy_smtp_recipients(&master_key).await?;

    // Immediately expire any orphaned `delivering` leases left by an unclean previous shutdown
    // before spawning the outbox loop so pending deliveries resume right away with checkpoints intact.
    let reclaimed = db
        .reclaim_orphaned_deliveries(feedcourier::db::models::now_unix())
        .await?;
    if reclaimed > 0 {
        tracing::info!(
            reclaimed_tasks = reclaimed,
            "reclaimed orphaned delivering outbox tasks on startup"
        );
    }

    let runtime =
        DeliveryDispatcher::from_db_settings(config.clone(), db.clone(), master_key.clone())
            .await?
            .with_log_store(log_store.clone());
    let scheduler_handle = runtime.spawn();
    if reclaimed > 0 {
        runtime.outbox_notify().notify_one();
    }

    let root_cancel = scheduler_handle.cancel_token().clone();
    let mcp_cancel = root_cancel.child_token();
    let mcp_state = McpState::new(db.clone(), runtime.ingest().clone())
        .with_outbox_notify(runtime.outbox_notify())
        .with_runtime(runtime.clone())
        .with_log_store(log_store.clone())
        .with_cancellation_token(mcp_cancel);
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
        log_store = log_store.backend_name(),
        log_format = config.logging.format.as_str(),
        "starting FeedCourier full-stack server"
    );

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(root_cancel))
    .await?;

    tracing::info!("HTTP server drained; waiting for background scheduler shutdown");
    scheduler_handle.shutdown().await;
    db.close().await;
    tracing::info!("FeedCourier shut down cleanly");
    log_store.shutdown().await;

    Ok(())
}

async fn shutdown_signal(root_cancel: CancellationToken) {
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
            tracing::info!("SIGINT received, draining background scheduler and HTTP connections");
        }
        () = terminate => {
            tracing::info!("SIGTERM received, draining background scheduler and HTTP connections");
        }
    }

    // Cancel the root scheduler token so background loops AND child `/mcp` SSE streams drain concurrently.
    root_cancel.cancel();
}
