//! Embedded React 19 single-page WebUI router powered by `rust-embed` (`web/dist/`).

use axum::body::Body;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use rust_embed::RustEmbed;

use crate::error::AppError;

/// Static WebUI bundle compiled directly into the single Rust binary.
#[derive(RustEmbed)]
#[folder = "web/dist/"]
pub struct WebAssets;

const FALLBACK_DEV_INDEX_HTML: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"UTF-8\"/><title>FeedCourier</title></head><body><div id=\"root\">FeedCourier WebUI</div></body></html>";

/// Explicit catch-all handler for unmatched `/api/*` routes returning JSON `404 Not Found`.
pub async fn api_not_found(uri: Uri) -> AppError {
    AppError::NotFound(format!("API endpoint '{}' not found", uri.path()))
}

/// Serve exact embedded static assets (`/assets/*`, `/favicon.svg`, etc.) with `mime_guess`,
/// falling back unmatched `GET`/`HEAD` SPA client routes (`/`, `/routes`, `/channels`, ...)
/// to `index.html`.
pub async fn serve_spa(method: Method, uri: Uri) -> Response {
    let path = uri.path();

    // Never serve SPA HTML for unmatched `/api` or `/mcp` endpoints.
    if path == "/api" || path.starts_with("/api/") || path == "/mcp" || path.starts_with("/mcp/") {
        return AppError::NotFound(format!("endpoint '{path}' not found")).into_response();
    }

    if !matches!(method, Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    let rel_path = path.trim_start_matches('/');
    if !rel_path.is_empty() {
        if let Some(asset) = WebAssets::get(rel_path) {
            let cache_control = if rel_path.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "public, max-age=3600"
            };
            return asset_response(&method, rel_path, asset.data.into_owned(), cache_control);
        }

        // Missing hashed bundle under `/assets/*` must 404 rather than returning HTML as JS/CSS.
        if rel_path.starts_with("assets/") {
            return StatusCode::NOT_FOUND.into_response();
        }
    }

    // SPA fallback: serve `index.html` for `/` and client-side deep links (`/routes`, etc.).
    let html_bytes = match WebAssets::get("index.html") {
        Some(index) => index.data.into_owned(),
        None => FALLBACK_DEV_INDEX_HTML.as_bytes().to_vec(),
    };
    asset_response(&method, "index.html", html_bytes, "no-cache")
}

fn asset_response(
    method: &Method,
    path_for_mime: &str,
    data: Vec<u8>,
    cache_control: &'static str,
) -> Response {
    let mime = mime_guess::from_path(path_for_mime).first_or_octet_stream();
    let content_type_str = format_content_type(mime.as_ref());

    let body = if *method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(Bytes::from(data))
    };

    let mut resp = Response::new(body);
    *resp.status_mut() = StatusCode::OK;
    if let Ok(ct) = HeaderValue::from_str(&content_type_str) {
        resp.headers_mut().insert(CONTENT_TYPE, ct);
    }
    resp.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static(cache_control));
    resp
}

fn format_content_type(mime_str: &str) -> String {
    if (mime_str.starts_with("text/")
        || mime_str == "application/javascript"
        || mime_str == "application/json"
        || mime_str == "image/svg+xml")
        && !mime_str.contains("charset")
    {
        format!("{mime_str}; charset=utf-8")
    } else {
        mime_str.to_string()
    }
}
