//! WebUI administrator password setup/login/logout handlers and `fc_session` HttpOnly cookie
//! authentication middleware (`feedcourier.md` Section 六.2).

use std::collections::{HashMap, VecDeque};
use std::sync::{LazyLock, Mutex};

use axum::Json;
use axum::extract::{Request, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::api::AppState;
use crate::api::account::{load_admin_username, totp_enabled, verify_second_factor};
use crate::api::dto::{
    ActionAckResponse, AuthLogin2faRequest, AuthLoginRequest, AuthSetupRequest, AuthStatusResponse,
    LoginResponse,
};
use crate::config::validate_admin_username;
use crate::crypto::{
    generate_session_token, hash_password, hash_token, sha256_hex, verify_password,
};
use crate::db::models::{
    DEFAULT_ADMIN_USERNAME, SETTING_ADMIN_PASSWORD_HASH, SETTING_ADMIN_USERNAME, now_unix,
};
use crate::error::{AppError, AppResult};

/// Cookie name storing the WebUI administrator session token.
pub const SESSION_COOKIE_NAME: &str = "fc_session";

/// Session cookie lifetime in seconds (`30 days` = `2,592,000` seconds).
pub const SESSION_TTL_SECS: i64 = 30 * 24 * 3600;

/// Minimum required length (in Unicode scalar values) when setting up the admin password.
pub const MIN_ADMIN_PASSWORD_LEN: usize = 8;

/// Sliding-window failure limiter (`max_failures` within `window_secs`).
pub struct FailureLimiter {
    max_failures: usize,
    window_secs: i64,
    failures: Mutex<VecDeque<i64>>,
}

impl FailureLimiter {
    pub const fn new(max_failures: usize, window_secs: i64) -> Self {
        Self {
            max_failures,
            window_secs,
            failures: Mutex::new(VecDeque::new()),
        }
    }

    fn prune(&self, queue: &mut VecDeque<i64>, now: i64) {
        while queue
            .front()
            .is_some_and(|ts| now.saturating_sub(*ts) >= self.window_secs)
        {
            queue.pop_front();
        }
    }

    /// Fail with `429` when too many recent failures were recorded.
    pub fn check(&self) -> AppResult<()> {
        let now = now_unix();
        let mut queue = self.failures.lock().expect("limiter mutex poisoned");
        self.prune(&mut queue, now);
        if queue.len() >= self.max_failures {
            let oldest = queue.front().copied().unwrap_or(now);
            let retry = oldest
                .saturating_add(self.window_secs)
                .saturating_sub(now)
                .max(1);
            return Err(AppError::RateLimited {
                message: "Too many failed attempts; try again later".to_string(),
                retry_after_secs: u64::try_from(retry).ok(),
            });
        }
        Ok(())
    }

    pub fn record_failure(&self) {
        let now = now_unix();
        let mut queue = self.failures.lock().expect("limiter mutex poisoned");
        self.prune(&mut queue, now);
        queue.push_back(now);
    }

    pub fn reset(&self) {
        self.failures
            .lock()
            .expect("limiter mutex poisoned")
            .clear();
    }
}

/// Limits failed `/auth/login` + `/auth/login/2fa` attempts (10 per 5 minutes).
pub static LOGIN_LIMITER: FailureLimiter = FailureLimiter::new(10, 300);

/// Limits failed re-authentication attempts on `/account*` (10 per 5 minutes).
pub static ACCOUNT_LIMITER: FailureLimiter = FailureLimiter::new(10, 300);

/// Lifetime of a pending two-factor login challenge.
const MFA_CHALLENGE_TTL_SECS: i64 = 300;

/// Maximum wrong second-factor codes per challenge before it is discarded.
const MFA_CHALLENGE_MAX_FAILURES: u32 = 5;

struct MfaChallenge {
    expires_at: i64,
    failures: u32,
}

static MFA_CHALLENGES: LazyLock<Mutex<HashMap<String, MfaChallenge>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn create_mfa_challenge() -> String {
    let random_bytes: [u8; 24] = rand::random();
    let token = format!("fcmfa_{}", hex::encode(random_bytes));
    let now = now_unix();
    let mut map = MFA_CHALLENGES.lock().expect("mfa challenge mutex poisoned");
    map.retain(|_, c| c.expires_at > now);
    map.insert(
        hash_token(&token),
        MfaChallenge {
            expires_at: now + MFA_CHALLENGE_TTL_SECS,
            failures: 0,
        },
    );
    token
}

fn challenge_is_live(token: &str) -> bool {
    let now = now_unix();
    let map = MFA_CHALLENGES.lock().expect("mfa challenge mutex poisoned");
    map.get(&hash_token(token))
        .is_some_and(|c| c.expires_at > now)
}

fn consume_challenge(token: &str) {
    MFA_CHALLENGES
        .lock()
        .expect("mfa challenge mutex poisoned")
        .remove(&hash_token(token));
}

fn record_challenge_failure(token: &str) {
    let key = hash_token(token);
    let mut map = MFA_CHALLENGES.lock().expect("mfa challenge mutex poisoned");
    if let Some(c) = map.get_mut(&key) {
        c.failures += 1;
        if c.failures >= MFA_CHALLENGE_MAX_FAILURES {
            map.remove(&key);
        }
    }
}

fn constant_time_str_eq(a: &str, b: &str) -> bool {
    let (ha, hb) = (sha256_hex(a.as_bytes()), sha256_hex(b.as_bytes()));
    ha.bytes()
        .zip(hb.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

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
    Json(req): Json<AuthSetupRequest>,
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

    let username = match req.username.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => DEFAULT_ADMIN_USERNAME.to_string(),
    };
    validate_admin_username(&username)?;

    let phc_hash = hash_password(&req.password)?;
    state
        .db()
        .set_setting_typed(SETTING_ADMIN_PASSWORD_HASH, &phc_hash)
        .await?;
    // An `ADMIN_USERNAME` env override (applied at bootstrap) always wins over the form value.
    if state.config().admin_username.is_none() {
        state
            .db()
            .set_setting_typed(SETTING_ADMIN_USERNAME, &username)
            .await?;
    }

    issue_session_response(&state).await
}

/// `POST /api/v1/auth/login`
pub async fn auth_login(
    State(state): State<AppState>,
    Json(req): Json<AuthLoginRequest>,
) -> AppResult<Response> {
    LOGIN_LIMITER.check()?;

    let stored_hash = state
        .db()
        .get_setting_typed::<String>(SETTING_ADMIN_PASSWORD_HASH)
        .await?;

    let password_ok = if let Some(phc_hash) = stored_hash {
        verify_password(&req.password, &phc_hash)?
    } else if let Some(env_pw) = state
        .config()
        .admin_password
        .as_deref()
        .filter(|p| !p.is_empty())
    {
        let matched = constant_time_str_eq(&req.password, env_pw);
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

    let expected_username = load_admin_username(&state).await?;
    let username_ok = constant_time_str_eq(req.username.trim(), &expected_username);

    if !(password_ok && username_ok) {
        LOGIN_LIMITER.record_failure();
        return Err(AppError::Unauthorized(
            "Invalid username or password".to_string(),
        ));
    }

    if totp_enabled(&state).await? {
        let token = create_mfa_challenge();
        return Ok(Json(LoginResponse {
            ok: false,
            mfa_required: true,
            mfa_token: Some(token),
        })
        .into_response());
    }

    LOGIN_LIMITER.reset();
    issue_session_response(&state).await
}

/// `POST /api/v1/auth/login/2fa`
pub async fn auth_login_2fa(
    State(state): State<AppState>,
    Json(req): Json<AuthLogin2faRequest>,
) -> AppResult<Response> {
    LOGIN_LIMITER.check()?;

    if !challenge_is_live(&req.mfa_token) {
        return Err(AppError::Unauthorized(
            "Verification session expired; sign in again".to_string(),
        ));
    }

    if !verify_second_factor(&state, &req.code).await? {
        LOGIN_LIMITER.record_failure();
        record_challenge_failure(&req.mfa_token);
        return Err(AppError::Unauthorized(
            "Invalid verification code".to_string(),
        ));
    }

    consume_challenge(&req.mfa_token);
    LOGIN_LIMITER.reset();
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
    let mut resp = Json(LoginResponse {
        ok: true,
        mfa_required: false,
        mfa_token: None,
    })
    .into_response();
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
