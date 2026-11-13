//! Dual-scope (`readonly` / `admin`) static Bearer token middleware for `/mcp`.
//!
//! Every HTTP request targeting `/mcp` MUST carry `Authorization: Bearer <plaintext_token>`.
//! Plaintext tokens are never persisted; the middleware computes `SHA-256` hex via
//! [`crate::crypto::sha256_hex`] and looks up `mcp_tokens.token_hash` in SQLite.

use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use tracing::{error, warn};

use crate::crypto::sha256_hex;
use crate::db::Database;
use crate::db::models::{McpTokenScope, now_unix};

/// `WWW-Authenticate` challenge header returned on `401 Unauthorized` responses from `/mcp`.
pub const MCP_WWW_AUTHENTICATE: &str = r#"Bearer realm="FeedCourier MCP""#;

/// Minimum interval in seconds between `mcp_tokens.last_used_at` SQLite writes for the same token.
const LAST_USED_TOUCH_MIN_INTERVAL_SECS: i64 = 5;

/// Verified MCP token identity attached to HTTP request extensions (propagated by `rmcp` into
/// `http::request::Parts` inside tool contexts) and `CURRENT_MCP_TOKEN`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedMcpToken {
    pub id: i64,
    pub name: String,
    pub scope: McpTokenScope,
}

tokio::task_local! {
    /// Task-local fallback carrying the authenticated MCP token for synchronous/direct invocation.
    pub static CURRENT_MCP_TOKEN: AuthenticatedMcpToken;
}

#[derive(Debug, Serialize)]
struct AuthErrorBody<'a> {
    error: &'a str,
    message: &'a str,
}

/// Extract a non-empty trimmed Bearer token from an `Authorization` header value.
pub fn parse_bearer_token(header_val: &str) -> Option<&str> {
    let trimmed = header_val.trim();
    let (scheme, rest) = trimmed.split_once(char::is_whitespace)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = rest.trim();
    if token.is_empty() { None } else { Some(token) }
}

/// Axum middleware enforcing `Authorization: Bearer <token>` on `/mcp` and propagating
/// [`AuthenticatedMcpToken`] (`readonly` vs `admin`) into `rmcp` request extensions.
pub async fn mcp_bearer_auth_middleware(
    State(db): State<Database>,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(raw_auth) = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return unauthorized_response("missing Authorization: Bearer <token> header");
    };

    let Some(plaintext_token) = parse_bearer_token(raw_auth) else {
        return unauthorized_response("malformed Authorization header; expected Bearer <token>");
    };

    let token_hash = sha256_hex(plaintext_token.trim().as_bytes());
    let token_row = match db.get_mcp_token_by_hash(&token_hash).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            return unauthorized_response("invalid or revoked MCP bearer token");
        }
        Err(err) => {
            error!(error = %err, "database failure during MCP bearer token lookup");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AuthErrorBody {
                    error: "internal_error",
                    message: "internal database error verifying MCP token",
                }),
            )
                .into_response();
        }
    };

    // Best-effort throttled update of `last_used_at`.
    let now = now_unix();
    let should_touch = token_row
        .last_used_at
        .is_none_or(|prev| now.saturating_sub(prev) >= LAST_USED_TOUCH_MIN_INTERVAL_SECS);
    if should_touch && let Err(err) = db.touch_mcp_token_last_used(token_row.id, now).await {
        warn!(
            token_id = token_row.id,
            error = %err,
            "failed to update mcp_tokens.last_used_at"
        );
    }

    let auth_token = AuthenticatedMcpToken {
        id: token_row.id,
        name: token_row.name,
        scope: token_row.scope,
    };

    // Attach to HTTP request extensions so `rmcp`'s `StreamableHttpService` preserves it inside
    // `http::request::Parts` across spawned session/stateless tasks.
    req.extensions_mut().insert(auth_token.clone());

    // Ensure synthetic in-process test requests without an explicit `Host` header or URI authority
    // still satisfy `rmcp`'s `Host` parser.
    if !req.headers().contains_key(header::HOST) && req.uri().authority().is_none() {
        req.headers_mut()
            .insert(header::HOST, HeaderValue::from_static("localhost"));
    }

    CURRENT_MCP_TOKEN.scope(auth_token, next.run(req)).await
}

fn unauthorized_response(message: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static(MCP_WWW_AUTHENTICATE),
        )],
        Json(AuthErrorBody {
            error: "unauthorized",
            message,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_and_rejects_invalid_bearer_headers() {
        assert_eq!(
            parse_bearer_token("Bearer fcm_ro_abc123"),
            Some("fcm_ro_abc123")
        );
        assert_eq!(
            parse_bearer_token("  bearer   fcm_adm_xyz  "),
            Some("fcm_adm_xyz")
        );
        assert_eq!(parse_bearer_token("Basic dXNlcjpwYXNz"), None);
        assert_eq!(parse_bearer_token("Bearer"), None);
        assert_eq!(parse_bearer_token("Bearer    "), None);
        assert_eq!(parse_bearer_token(""), None);
    }
}
