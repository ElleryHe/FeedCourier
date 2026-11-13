//! FeedCourier binary entrypoint.

use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use feedcourier::api::baseline_router;
use feedcourier::config::{APP_NAME, AppConfig, LICENSE_SPDX, REPO_URL, VERSION};
use feedcourier::crypto::{MasterKey, hash_password};
use feedcourier::db::Database;
use feedcourier::db::models::{
    SETTING_ADMIN_PASSWORD_HASH, SETTING_OUTBOUND_PROXY_URL, SETTING_SSRF_ALLOWLIST,
};
use feedcourier::error::AppResult;

#[tokio::main]
async fn main() -> AppResult<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("feedcourier=info,tower_http=info")),
        )
        .init();

    let config = AppConfig::from_env()?;
    let _master_key = MasterKey::from_config(&config)?;
    let db = Database::from_config(&config).await?;

    // Bootstrap optional environment presets into `system_settings` on startup.
    if let Some(ref admin_pw) = config.admin_password {
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

    tracing::info!(
        app = APP_NAME,
        version = VERSION,
        license = LICENSE_SPDX,
        repo = REPO_URL,
        listen_addr = %config.listen_addr,
        data_dir = %config.data_dir.display(),
        "starting FeedCourier"
    );

    let app = baseline_router();
    let listener = TcpListener::bind(config.listen_addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    db.close().await;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received, draining connections");
}
