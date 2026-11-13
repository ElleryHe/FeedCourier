//! WebUI administrator password setup/login/logout handlers and `fc_session` HttpOnly cookie
//! authentication middleware (`feedcourier.md` Section 六.2).

use axum::Json;
use axum::extract::{Request, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::api::AppState;
use crate::api::dto::{ActionAckResponse, AuthPasswordRequest, AuthStatusResponse};
use crate::crypto::{
    generate_session_token, hash_password, hash_token, sha256_hex, verify_password,
};
use crate::db::models::{SETTING_ADMIN_PASSWORD_HASH, now_unix};
use crate::error::{AppError, AppResult};

/// Cookie name storing the WebUI administrator session token.
pub const SESSION_COOKIE_NAME: &str = "fc_session";

/// Session cookie lifetime in seconds (`30 days` = `2,592,000` seconds).
pub const SESSION_TTL_SECS: i64 = 30 * 24 * 3600;

/// Minimum required length (in Unicode scalar values) when setting up the admin password.
pub const MIN_ADMIN_PASSWORD_LEN: usize = 8;

/// `GET /api/v1/auth/status`
pub async fn auth_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<AuthStatusResponse>> {
    let stored_hash = state
        .db()
        .get_setting_typed::<String>(SETTING_ADMIN_PASSWORD_HASH)
        .await?;
    let has_env_password = state
        .config()
        .admin_password
        .as_deref()
        .is_some_and(|p| !p.is_empty());

    let setup_required = stored_hash.is_none() && !has_env_password;
    let authenticated = if setup_required {
        false
    } else {
        has_valid_session(&state, &headers).await?
    };

    Ok(Json(AuthStatusResponse::new(setup_required, authenticated)))
}

/// `POST /api/v1/auth/setup`
pub async fn auth_setup(
    State(state): State<AppState>,
    Json(req): Json<AuthPasswordRequest>,
) -> AppResult<Response> {
    let stored_hash = state
        .db()
        .get_setting_typed::<String>(SETTING_ADMIN_PASSWORD_HASH)
        .await?;
    let has_env_password = state
        .config()
        .admin_password
        .as_deref()
        .is_some_and(|p| !p.is_empty());

    if stored_hash.is_some() || has_env_password {
        return Err(AppError::Conflict(
            "Administrator password has already been configured".to_string(),
        ));
    }

    if req.password.chars().count() < MIN_ADMIN_PASSWORD_LEN {
        return Err(AppError::Validation(format!(
            "Password must be at least {MIN_ADMIN_PASSWORD_LEN} characters"
        )));
    }

    let phc_hash = hash_password(&req.password)?;
    state
        .db()
        .set_setting_typed(SETTING_ADMIN_PASSWORD_HASH, &phc_hash)
        .await?;

    issue_session_response(&state).await
}

/// `POST /api/v1/auth/login`
pub async fn auth_login(
    State(state): State<AppState>,
    Json(req): Json<AuthPasswordRequest>,
) -> AppResult<Response> {
    let stored_hash = state
        .db()
        .get_setting_typed::<String>(SETTING_ADMIN_PASSWORD_HASH)
        .await?;

    let is_valid = if let Some(phc_hash) = stored_hash {
        verify_password(&req.password, &phc_hash)?
    } else if let Some(env_pw) = state
        .config()
        .admin_password
        .as_deref()
        .filter(|p| !p.is_empty())
    {
        let matched = sha256_hex(req.password.as_bytes()) == sha256_hex(env_pw.as_bytes());
        if matched {
            let phc_hash = hash_password(env_pw)?;
            state
                .db()
                .set_setting_typed(SETTING_ADMIN_PASSWORD_HASH, &phc_hash)
                .await?;
        }
        matched
    } else {
        return Err(AppError::BadRequest(
            "Administrator password has not been set up yet".to_string(),
        ));
    };

    if !is_valid {
        return Err(AppError::Unauthorized("Invalid password".to_string()));
    }

    issue_session_response(&state).await
}

/// `POST /api/v1/auth/logout`
pub async fn auth_logout(State(state): State<AppState>, headers: HeaderMap) -> AppResult<Response> {
    if let Some(raw_token) = extract_cookie(&headers, SESSION_COOKIE_NAME) {
        let token_hash = hash_token(&raw_token);
        let _ = state.db().delete_session(&token_hash).await?;
    }

    let clear_cookie = format!("{SESSION_COOKIE_NAME}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0");
    let mut resp = Json(ActionAckResponse::ok()).into_response();
    resp.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&clear_cookie).expect("clear session cookie must be valid ASCII"),
    );
    Ok(resp)
}

/// Axum middleware enforcing a valid unexpired `fc_session` cookie on `/api/v1/*` routes.
pub async fn require_session(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    if !has_valid_session(&state, req.headers()).await? {
        return Err(AppError::Unauthorized(
            "Authentication required".to_string(),
        ));
    }
    Ok(next.run(req).await)
}

async fn has_valid_session(state: &AppState, headers: &HeaderMap) -> AppResult<bool> {
    let Some(raw_token) = extract_cookie(headers, SESSION_COOKIE_NAME) else {
        return Ok(false);
    };
    let token_hash = hash_token(&raw_token);
    let session = state
        .db()
        .get_valid_session(&token_hash, now_unix())
        .await?;
    Ok(session.is_some())
}

async fn issue_session_response(state: &AppState) -> AppResult<Response> {
    let (plaintext_token, token_hash) = generate_session_token();
    let expires_at = now_unix().saturating_add(SESSION_TTL_SECS);
    state.db().create_session(&token_hash, expires_at).await?;

    let cookie_val = format!(
        "{SESSION_COOKIE_NAME}={plaintext_token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={SESSION_TTL_SECS}"
    );
    let mut resp = Json(ActionAckResponse::ok()).into_response();
    resp.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie_val).map_err(|err| {
            AppError::Internal(anyhow::anyhow!("failed to encode Set-Cookie header: {err}"))
        })?,
    );
    Ok(resp)
}

/// Extract a named cookie value from HTTP `Cookie` request headers.
pub fn extract_cookie(headers: &HeaderMap, cookie_name: &str) -> Option<String> {
    for val in headers.get_all(header::COOKIE) {
        let Ok(raw) = val.to_str() else {
            continue;
        };
        for pair in raw.split(';') {
            let trimmed = pair.trim();
            if let Some((k, v)) = trimmed.split_once('=')
                && k.trim() == cookie_name
            {
                let clean = v.trim().trim_matches('"');
                if !clean.is_empty() {
                    return Some(clean.to_string());
                }
            }
        }
    }
    None
}
