//! Administrator account management (`/api/v1/account*`): username/password changes and
//! RFC 6238 TOTP two-factor authentication with single-use recovery codes.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use base64::Engine as _;

use crate::api::AppState;
use crate::api::auth::{
    ACCOUNT_LIMITER, MIN_ADMIN_PASSWORD_LEN, SESSION_COOKIE_NAME, extract_cookie,
};
use crate::api::dto::{
    AccountDto, PasswordConfirmRequest, RecoveryCodesResponse, TotpDisableRequest,
    TotpEnableRequest, TotpSetupResponse, UpdateAccountRequest,
};
use crate::config::validate_admin_username;
use crate::crypto::{hash_password, hash_token, verify_password};
use crate::db::models::{
    DEFAULT_ADMIN_USERNAME, SETTING_ADMIN_PASSWORD_HASH, SETTING_ADMIN_RECOVERY_HASHES,
    SETTING_ADMIN_TOTP_LAST_STEP, SETTING_ADMIN_TOTP_PENDING, SETTING_ADMIN_TOTP_SECRET,
    SETTING_ADMIN_USERNAME, now_unix,
};
use crate::error::{AppError, AppResult};
use crate::totp;

/// TOTP issuer label shown in authenticator apps.
pub const TOTP_ISSUER: &str = "FeedCourier";

/// Stored administrator username (falls back to [`DEFAULT_ADMIN_USERNAME`]).
pub async fn load_admin_username(state: &AppState) -> AppResult<String> {
    Ok(state
        .db()
        .get_setting_typed::<String>(SETTING_ADMIN_USERNAME)
        .await?
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_ADMIN_USERNAME.to_string()))
}

async fn load_secret(state: &AppState, key: &str) -> AppResult<Option<Vec<u8>>> {
    let Some(b64) = state.db().get_setting_typed::<String>(key).await? else {
        return Ok(None);
    };
    let blob = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|err| AppError::Crypto(format!("invalid stored TOTP secret encoding: {err}")))?;
    Ok(Some(state.master_key().decrypt(&blob)?))
}

async fn store_secret(state: &AppState, key: &str, secret: &[u8]) -> AppResult<()> {
    let blob = state.master_key().encrypt(secret)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(blob);
    state.db().set_setting_typed(key, &b64).await?;
    Ok(())
}

/// Whether TOTP two-factor authentication is currently enabled.
pub async fn totp_enabled(state: &AppState) -> AppResult<bool> {
    Ok(state
        .db()
        .get_setting(SETTING_ADMIN_TOTP_SECRET)
        .await?
        .is_some())
}

async fn recovery_hashes(state: &AppState) -> AppResult<Vec<String>> {
    Ok(state
        .db()
        .get_setting_typed::<Vec<String>>(SETTING_ADMIN_RECOVERY_HASHES)
        .await?
        .unwrap_or_default())
}

/// Verify a TOTP code or consume a recovery code. Returns `true` when accepted.
pub async fn verify_second_factor(state: &AppState, code: &str) -> AppResult<bool> {
    let trimmed = code.trim();
    if trimmed.is_empty() {
        return Ok(false);
    }

    if totp::looks_like_recovery_code(trimmed) {
        let candidate = totp::hash_recovery_code(trimmed);
        let mut hashes = recovery_hashes(state).await?;
        if let Some(pos) = hashes.iter().position(|h| *h == candidate) {
            hashes.remove(pos);
            state
                .db()
                .set_setting_typed(SETTING_ADMIN_RECOVERY_HASHES, &hashes)
                .await?;
            return Ok(true);
        }
        return Ok(false);
    }

    let Some(secret) = load_secret(state, SETTING_ADMIN_TOTP_SECRET).await? else {
        return Ok(false);
    };
    let last_step = state
        .db()
        .get_setting_typed::<i64>(SETTING_ADMIN_TOTP_LAST_STEP)
        .await?;
    match totp::verify_code(&secret, trimmed, now_unix(), last_step) {
        Some(step) => {
            state
                .db()
                .set_setting_typed(SETTING_ADMIN_TOTP_LAST_STEP, &step)
                .await?;
            Ok(true)
        }
        None => Ok(false),
    }
}

async fn verify_current_password(state: &AppState, password: &str) -> AppResult<()> {
    ACCOUNT_LIMITER.check()?;
    let stored = state
        .db()
        .get_setting_typed::<String>(SETTING_ADMIN_PASSWORD_HASH)
        .await?;
    let ok = match stored {
        Some(phc) => verify_password(password, &phc)?,
        None => false,
    };
    if !ok {
        ACCOUNT_LIMITER.record_failure();
        // `403` (not `401`) so the WebUI does not mistake a failed re-authentication for an
        // expired session.
        return Err(AppError::Forbidden(
            "Current password is incorrect".to_string(),
        ));
    }
    Ok(())
}

async fn require_second_factor(state: &AppState, code: Option<&str>) -> AppResult<()> {
    ACCOUNT_LIMITER.check()?;
    let Some(code) = code.map(str::trim).filter(|c| !c.is_empty()) else {
        return Err(AppError::Validation(
            "A verification code (TOTP or recovery code) is required".to_string(),
        ));
    };
    if verify_second_factor(state, code).await? {
        Ok(())
    } else {
        ACCOUNT_LIMITER.record_failure();
        Err(AppError::Forbidden("Invalid verification code".to_string()))
    }
}

async fn build_account_dto(state: &AppState) -> AppResult<AccountDto> {
    let enabled = totp_enabled(state).await?;
    let remaining = if enabled {
        recovery_hashes(state).await?.len()
    } else {
        0
    };
    Ok(AccountDto {
        username: load_admin_username(state).await?,
        username_env_managed: state.config().admin_username.is_some(),
        totp_enabled: enabled,
        recovery_codes_remaining: remaining,
    })
}

async fn issue_recovery_codes(state: &AppState) -> AppResult<Vec<String>> {
    let codes = totp::generate_recovery_codes();
    let hashes: Vec<String> = codes.iter().map(|c| totp::hash_recovery_code(c)).collect();
    state
        .db()
        .set_setting_typed(SETTING_ADMIN_RECOVERY_HASHES, &hashes)
        .await?;
    Ok(codes)
}

/// `GET /api/v1/account`
pub async fn get_account(State(state): State<AppState>) -> AppResult<Json<AccountDto>> {
    Ok(Json(build_account_dto(&state).await?))
}

/// `PATCH /api/v1/account`
pub async fn update_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpdateAccountRequest>,
) -> AppResult<Json<AccountDto>> {
    verify_current_password(&state, &req.current_password).await?;

    let new_username = req
        .new_username
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let new_password = req.new_password.as_deref().filter(|s| !s.is_empty());
    if new_username.is_none() && new_password.is_none() {
        return Err(AppError::Validation(
            "Provide new_username and/or new_password".to_string(),
        ));
    }

    if let Some(name) = new_username {
        validate_admin_username(name)?;
    }
    if let Some(pw) = new_password {
        if pw.chars().count() < MIN_ADMIN_PASSWORD_LEN {
            return Err(AppError::Validation(format!(
                "Password must be at least {MIN_ADMIN_PASSWORD_LEN} characters"
            )));
        }
        if totp_enabled(&state).await? {
            require_second_factor(&state, req.code.as_deref()).await?;
        }
    }

    if let Some(name) = new_username {
        state
            .db()
            .set_setting_typed(SETTING_ADMIN_USERNAME, &name.to_string())
            .await?;
    }
    if let Some(pw) = new_password {
        let phc = hash_password(pw)?;
        state
            .db()
            .set_setting_typed(SETTING_ADMIN_PASSWORD_HASH, &phc)
            .await?;
        let keep = extract_cookie(&headers, SESSION_COOKIE_NAME).map(|raw| hash_token(&raw));
        state.db().delete_sessions_except(keep.as_deref()).await?;
    }

    Ok(Json(build_account_dto(&state).await?))
}

/// `POST /api/v1/account/2fa/setup`
pub async fn totp_setup(
    State(state): State<AppState>,
    Json(req): Json<PasswordConfirmRequest>,
) -> AppResult<Json<TotpSetupResponse>> {
    verify_current_password(&state, &req.current_password).await?;
    if totp_enabled(&state).await? {
        return Err(AppError::Conflict(
            "Two-factor authentication is already enabled".to_string(),
        ));
    }

    let secret = totp::generate_secret();
    store_secret(&state, SETTING_ADMIN_TOTP_PENDING, &secret).await?;

    let secret_b32 = totp::base32_encode(&secret);
    let username = load_admin_username(&state).await?;
    let otpauth_url = totp::otpauth_url(TOTP_ISSUER, &username, &secret_b32);
    let qr_svg = totp::qr_svg(&otpauth_url)?;
    Ok(Json(TotpSetupResponse {
        secret: secret_b32,
        otpauth_url,
        qr_svg,
    }))
}

/// `POST /api/v1/account/2fa/enable`
pub async fn totp_enable(
    State(state): State<AppState>,
    Json(req): Json<TotpEnableRequest>,
) -> AppResult<(StatusCode, Json<RecoveryCodesResponse>)> {
    ACCOUNT_LIMITER.check()?;
    if totp_enabled(&state).await? {
        return Err(AppError::Conflict(
            "Two-factor authentication is already enabled".to_string(),
        ));
    }
    let Some(pending) = load_secret(&state, SETTING_ADMIN_TOTP_PENDING).await? else {
        return Err(AppError::BadRequest(
            "No pending 2FA setup; start the setup again".to_string(),
        ));
    };
    let Some(step) = totp::verify_code(&pending, &req.code, now_unix(), None) else {
        ACCOUNT_LIMITER.record_failure();
        return Err(AppError::Forbidden("Invalid verification code".to_string()));
    };

    store_secret(&state, SETTING_ADMIN_TOTP_SECRET, &pending).await?;
    state
        .db()
        .set_setting_typed(SETTING_ADMIN_TOTP_LAST_STEP, &step)
        .await?;
    state
        .db()
        .delete_setting(SETTING_ADMIN_TOTP_PENDING)
        .await?;
    let codes = issue_recovery_codes(&state).await?;
    tracing::info!("administrator enabled TOTP two-factor authentication");
    Ok((
        StatusCode::OK,
        Json(RecoveryCodesResponse {
            recovery_codes: codes,
        }),
    ))
}

/// `POST /api/v1/account/2fa/disable`
pub async fn totp_disable(
    State(state): State<AppState>,
    Json(req): Json<TotpDisableRequest>,
) -> AppResult<Json<AccountDto>> {
    verify_current_password(&state, &req.current_password).await?;
    if !totp_enabled(&state).await? {
        return Err(AppError::BadRequest(
            "Two-factor authentication is not enabled".to_string(),
        ));
    }
    require_second_factor(&state, Some(&req.code)).await?;
    clear_totp_state(&state).await?;
    tracing::info!("administrator disabled TOTP two-factor authentication");
    Ok(Json(build_account_dto(&state).await?))
}

/// `POST /api/v1/account/2fa/recovery-codes`
pub async fn regenerate_recovery_codes(
    State(state): State<AppState>,
    Json(req): Json<PasswordConfirmRequest>,
) -> AppResult<Json<RecoveryCodesResponse>> {
    verify_current_password(&state, &req.current_password).await?;
    if !totp_enabled(&state).await? {
        return Err(AppError::BadRequest(
            "Two-factor authentication is not enabled".to_string(),
        ));
    }
    let codes = issue_recovery_codes(&state).await?;
    Ok(Json(RecoveryCodesResponse {
        recovery_codes: codes,
    }))
}

/// Remove every TOTP-related setting (disable flow and `RESET_2FA` emergency reset).
pub async fn clear_totp_state_db(db: &crate::db::Database) -> AppResult<()> {
    for key in [
        SETTING_ADMIN_TOTP_SECRET,
        SETTING_ADMIN_TOTP_PENDING,
        SETTING_ADMIN_TOTP_LAST_STEP,
        SETTING_ADMIN_RECOVERY_HASHES,
    ] {
        db.delete_setting(key).await?;
    }
    Ok(())
}

async fn clear_totp_state(state: &AppState) -> AppResult<()> {
    clear_totp_state_db(state.db()).await
}
