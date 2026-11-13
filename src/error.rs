//! Unified application error handling and HTTP response mapping.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Convenient result alias across FeedCourier modules.
pub type AppResult<T> = Result<T, AppError>;

/// Unified domain, storage, crypto, network, and API error type.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),

    #[error("{0}")]
    Validation(String),

    #[error("{0}")]
    Unauthorized(String),

    #[error("{0}")]
    Forbidden(String),

    #[error("{0}")]
    NotFound(String),

    #[error("{0}")]
    Conflict(String),

    #[error("{message}")]
    RateLimited {
        message: String,
        retry_after_secs: Option<u64>,
    },

    #[error("SSRF guard blocked target: {0}")]
    SsrfBlocked(String),

    #[error("upstream error: {0}")]
    Upstream(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("cryptography error: {0}")]
    Crypto(String),

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("database migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("invalid URL: {0}")]
    Url(#[from] url::ParseError),

    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl AppError {
    /// Returns true when the underlying error is a SQLite UNIQUE / PRIMARY KEY constraint violation.
    pub fn is_unique_violation(&self) -> bool {
        match self {
            Self::Conflict(_) => true,
            Self::Database(sqlx::Error::Database(db_err)) => db_err.is_unique_violation(),
            _ => false,
        }
    }

    /// Map this error to `(StatusCode, machine_code, client_safe_message)`.
    pub fn status_and_code(&self) -> (StatusCode, &'static str, String) {
        match self {
            Self::BadRequest(msg) => (StatusCode::BAD_REQUEST, "bad_request", msg.clone()),
            Self::Validation(msg) => (StatusCode::BAD_REQUEST, "validation_error", msg.clone()),
            Self::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, "unauthorized", msg.clone()),
            Self::Forbidden(msg) => (StatusCode::FORBIDDEN, "forbidden", msg.clone()),
            Self::NotFound(msg) => (StatusCode::NOT_FOUND, "not_found", msg.clone()),
            Self::Conflict(msg) => (StatusCode::CONFLICT, "conflict", msg.clone()),
            Self::RateLimited { message, .. } => (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                message.clone(),
            ),
            Self::SsrfBlocked(_) => (StatusCode::FORBIDDEN, "ssrf_blocked", self.to_string()),
            Self::Upstream(msg) => (StatusCode::BAD_GATEWAY, "upstream_error", msg.clone()),
            Self::Json(err) => (
                StatusCode::BAD_REQUEST,
                "invalid_json",
                format!("invalid JSON payload: {err}"),
            ),
            Self::Url(err) => (
                StatusCode::BAD_REQUEST,
                "invalid_url",
                format!("invalid URL: {err}"),
            ),
            Self::Database(sqlx::Error::RowNotFound) => (
                StatusCode::NOT_FOUND,
                "not_found",
                "requested record was not found".to_string(),
            ),
            Self::Database(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => (
                StatusCode::CONFLICT,
                "conflict",
                "resource already exists".to_string(),
            ),
            Self::Config(_)
            | Self::Crypto(_)
            | Self::Database(_)
            | Self::Migration(_)
            | Self::Io(_)
            | Self::Internal(_) => {
                tracing::error!(error = ?self, "internal server error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "internal server error".to_string(),
                )
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope<'a> {
    error: String,
    code: &'a str,
    message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let retry_after = match &self {
            Self::RateLimited {
                retry_after_secs, ..
            } => *retry_after_secs,
            _ => None,
        };
        let (status, code, message) = self.status_and_code();
        let body = ErrorEnvelope {
            error: message.clone(),
            code,
            message,
        };
        let mut resp = (status, Json(body)).into_response();
        if let Some(secs) = retry_after
            && let Ok(val) = HeaderValue::from_str(&secs.to_string())
        {
            resp.headers_mut().insert(header::RETRY_AFTER, val);
        }
        resp
    }
}
