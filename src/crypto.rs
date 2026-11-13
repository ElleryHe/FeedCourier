//! Cryptographic primitives for FeedCourier:
//! - Zero-config `MasterKey` loader (`MASTER_KEY` env or `<data_dir>/master.key` with `0600` mode)
//! - ChaCha20-Poly1305 AEAD encryption/decryption (12-byte random nonce prepended)
//! - Argon2id administrator password hashing and verification
//! - SHA-256 hex hashing for session tokens, MCP bearer tokens, and `credential_group_key`

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use base64::Engine as _;
use chacha20poly1305::aead::{Aead, Generate, Key, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::config::AppConfig;
use crate::error::{AppError, AppResult};

/// Length in bytes of a ChaCha20-Poly1305 symmetric key (256 bits).
pub const MASTER_KEY_LEN: usize = 32;

/// Length in bytes of the prepended ChaCha20-Poly1305 random nonce (96 bits).
pub const NONCE_LEN: usize = 12;

/// Length in bytes of the Poly1305 authentication tag (128 bits).
pub const TAG_LEN: usize = 16;

/// 256-bit symmetric master key used to encrypt channel credentials at rest.
#[derive(Clone)]
pub struct MasterKey {
    bytes: [u8; MASTER_KEY_LEN],
}

impl fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MasterKey([REDACTED])")
    }
}

impl Drop for MasterKey {
    fn drop(&mut self) {
        // Volatile-friendly scrub on drop.
        self.bytes.fill(0);
        std::hint::black_box(&self.bytes);
    }
}

impl MasterKey {
    /// Wrap raw 32-byte key material.
    pub fn from_bytes(bytes: [u8; MASTER_KEY_LEN]) -> Self {
        Self { bytes }
    }

    /// Generate a fresh random 256-bit master key from the OS CSPRNG.
    pub fn generate() -> Self {
        let key = Key::<ChaCha20Poly1305>::generate();
        let mut bytes = [0u8; MASTER_KEY_LEN];
        bytes.copy_from_slice(key.as_slice());
        Self { bytes }
    }

    /// Decode a 32-byte key from a 64-char hex string or standard/URL-safe base64 string.
    pub fn from_encoded(encoded: &str) -> AppResult<Self> {
        let trimmed = encoded.trim();
        if trimmed.is_empty() {
            return Err(AppError::Crypto("master key string is empty".to_string()));
        }

        if trimmed.len() == MASTER_KEY_LEN * 2
            && let Ok(decoded) = hex::decode(trimmed)
        {
            return Self::try_from_slice(&decoded);
        }

        for engine in [
            &base64::engine::general_purpose::STANDARD,
            &base64::engine::general_purpose::STANDARD_NO_PAD,
            &base64::engine::general_purpose::URL_SAFE,
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        ] {
            if let Ok(decoded) = engine.decode(trimmed)
                && decoded.len() == MASTER_KEY_LEN
            {
                return Self::try_from_slice(&decoded);
            }
        }

        Err(AppError::Crypto(
            "invalid master key encoding: expected 64 hex chars or base64-encoded 32 bytes"
                .to_string(),
        ))
    }

    fn try_from_slice(slice: &[u8]) -> AppResult<Self> {
        let bytes: [u8; MASTER_KEY_LEN] = slice.try_into().map_err(|_| {
            AppError::Crypto(format!(
                "invalid master key length: expected {MASTER_KEY_LEN} bytes, got {}",
                slice.len()
            ))
        })?;
        Ok(Self { bytes })
    }

    /// Export key as lowercase 64-character hex string.
    pub fn to_hex(&self) -> String {
        hex::encode(self.bytes)
    }

    /// Load master key from `AppConfig` (`MASTER_KEY` env if set, otherwise `<data_dir>/master.key`).
    pub fn from_config(config: &AppConfig) -> AppResult<Self> {
        Self::load_or_init(config.master_key.as_deref(), &config.master_key_path())
    }

    /// Load master key from explicit string if provided, otherwise read or initialize `key_path`
    /// with strict `0600` Unix file permissions.
    pub fn load_or_init(explicit_key: Option<&str>, key_path: &Path) -> AppResult<Self> {
        if let Some(raw) = explicit_key.map(str::trim).filter(|s| !s.is_empty()) {
            return Self::from_encoded(raw);
        }

        if key_path.exists() {
            return Self::read_key_file(key_path);
        }

        if let Some(parent) = key_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }

        let generated = Self::generate();
        let hex_payload = generated.to_hex();

        // Write via a 0600 temporary sibling or exclusive create so the key is never world-readable.
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }

        match opts.open(key_path) {
            Ok(mut file) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    // Explicitly override process umask to guarantee exact 0600 permissions.
                    file.set_permissions(fs::Permissions::from_mode(0o600))?;
                }
                file.write_all(hex_payload.as_bytes())?;
                file.write_all(b"\n")?;
                file.sync_all()?;
                Ok(generated)
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                Self::read_key_file(key_path)
            }
            Err(err) => Err(AppError::Io(err)),
        }
    }

    fn read_key_file(key_path: &Path) -> AppResult<Self> {
        let raw_bytes = fs::read(key_path)?;
        if raw_bytes.len() == MASTER_KEY_LEN {
            return Self::try_from_slice(&raw_bytes);
        }
        let as_str = std::str::from_utf8(&raw_bytes).map_err(|_| {
            AppError::Crypto(format!(
                "master key file '{}' is neither 32 raw bytes nor valid UTF-8",
                key_path.display()
            ))
        })?;
        Self::from_encoded(as_str)
    }

    /// Encrypt `plaintext` using ChaCha20-Poly1305, returning `[12-byte nonce || ciphertext || 16-byte tag]`.
    pub fn encrypt(&self, plaintext: &[u8]) -> AppResult<Vec<u8>> {
        let key = Key::<ChaCha20Poly1305>::try_from(self.bytes.as_slice())
            .map_err(|_| AppError::Crypto("invalid key slice length".to_string()))?;
        let cipher = ChaCha20Poly1305::new(&key);
        let nonce = Nonce::generate();
        let ciphertext = cipher
            .encrypt(&nonce, plaintext)
            .map_err(|_| AppError::Crypto("ChaCha20-Poly1305 encryption failed".to_string()))?;

        let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        out.extend_from_slice(nonce.as_slice());
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Decrypt and authenticate a `[12-byte nonce || ciphertext || 16-byte tag]` blob.
    pub fn decrypt(&self, blob: &[u8]) -> AppResult<Vec<u8>> {
        if blob.len() < NONCE_LEN + TAG_LEN {
            return Err(AppError::Crypto(format!(
                "encrypted payload too short: expected at least {} bytes, got {}",
                NONCE_LEN + TAG_LEN,
                blob.len()
            )));
        }

        let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);
        let key = Key::<ChaCha20Poly1305>::try_from(self.bytes.as_slice())
            .map_err(|_| AppError::Crypto("invalid key slice length".to_string()))?;
        let nonce = Nonce::try_from(nonce_bytes)
            .map_err(|_| AppError::Crypto("invalid nonce slice length".to_string()))?;
        let cipher = ChaCha20Poly1305::new(&key);

        cipher.decrypt(&nonce, ciphertext).map_err(|_| {
            AppError::Crypto(
                "ChaCha20-Poly1305 authentication failed (tampered ciphertext or wrong master key)"
                    .to_string(),
            )
        })
    }

    /// Serialize `value` as JSON and encrypt it with ChaCha20-Poly1305.
    pub fn encrypt_json<T: Serialize>(&self, value: &T) -> AppResult<Vec<u8>> {
        let json_bytes = serde_json::to_vec(value)?;
        self.encrypt(&json_bytes)
    }

    /// Decrypt `blob` with ChaCha20-Poly1305 and deserialize JSON into `T`.
    pub fn decrypt_json<T: DeserializeOwned>(&self, blob: &[u8]) -> AppResult<T> {
        let plaintext = self.decrypt(blob)?;
        let parsed = serde_json::from_slice(&plaintext)?;
        Ok(parsed)
    }
}

/// Hash an administrator password using Argon2id (v19, random CSPRNG salt) into a PHC string.
pub fn hash_password(password: &str) -> AppResult<String> {
    if password.is_empty() {
        return Err(AppError::Validation(
            "password must not be empty".to_string(),
        ));
    }
    let hashed = Argon2::default()
        .hash_password(password.as_bytes())
        .map_err(|err| AppError::Crypto(format!("Argon2id hashing failed: {err}")))?;
    Ok(hashed.to_string())
}

/// Verify an administrator password candidate against a stored Argon2id PHC string.
pub fn verify_password(password: &str, phc_hash: &str) -> AppResult<bool> {
    let parsed = PasswordHash::new(phc_hash)
        .map_err(|err| AppError::Crypto(format!("invalid stored password hash: {err}")))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

/// Compute lowercase 64-character SHA-256 hex digest of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    hex::encode(digest)
}

/// Compute SHA-256 hex digest of a bearer/session token string.
pub fn hash_token(token: &str) -> String {
    sha256_hex(token.as_bytes())
}

/// Compute deterministic `credential_group_key` so multiple channels backed by the same
/// SMTP account, Telegram Bot token, or Notion integration secret share rate-limiting locks.
pub fn compute_credential_group_key(channel_kind: &str, canonical_identity: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"feedcourier:credential-group:v1:");
    hasher.update(channel_kind.trim().to_ascii_lowercase().as_bytes());
    hasher.update(b":");
    hasher.update(canonical_identity.trim().as_bytes());
    hex::encode(hasher.finalize())
}

/// Generate a high-entropy random WebUI session token (`(plaintext_token, token_sha256_hex)`).
pub fn generate_session_token() -> (String, String) {
    let random_bytes: [u8; 32] = rand::random();
    let token = format!("fcs_{}", hex::encode(random_bytes));
    let token_hash = hash_token(&token);
    (token, token_hash)
}

/// Generate a high-entropy MCP bearer token (`(plaintext_token, token_sha256_hex, token_prefix)`).
pub fn generate_mcp_token(scope: &str) -> (String, String, String) {
    let random_bytes: [u8; 24] = rand::random();
    let tag = match scope {
        "admin" => "adm",
        _ => "ro",
    };
    let token = format!("fcm_{tag}_{}", hex::encode(random_bytes));
    let token_hash = hash_token(&token);
    let prefix_len = token.len().min(12);
    let token_prefix = token[..prefix_len].to_string();
    (token, token_hash, token_prefix)
}
