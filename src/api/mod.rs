//! REST API (`/api/v1/*`), HttpOnly session authentication, and `rust-embed` static WebUI router.
//!
//! Implemented in Wave 2 (`feedcourier.md` Section 二.2 & 六.2).

use axum::Router;
use axum::routing::get;
use bytes as _;
use http as _;
use http_body_util as _;
use mime_guess as _;
use rust_embed as _;
use tower as _;
use tower_http as _;

use crate::config::{APP_NAME, LICENSE_SPDX, REPO_URL, VERSION};

/// Build a baseline health/metadata router (extended with full `/api/v1` and WebUI routes in Wave 2).
pub fn baseline_router() -> Router {
    Router::new().route(
        "/api/v1/health",
        get(|| async {
            axum::Json(serde_json::json!({
                "name": APP_NAME,
                "version": VERSION,
                "license": LICENSE_SPDX,
                "repo_url": REPO_URL,
                "status": "ok",
            }))
        }),
    )
}
