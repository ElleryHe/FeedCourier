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
use hmac::{Hmac, KeyInit as HmacKeyInit, Mac};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

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

    /// Derive a domain-separated 32-byte subkey using RFC 5869 HKDF-SHA256 (fixed zero salt,
    /// `ikm = master_key`, `info = context_label`).
    pub fn derive_subkey(&self, info: &[u8]) -> [u8; 32] {
        // HKDF-Extract(salt = 0^32, IKM = master_key)
        let mut extract = <HmacSha256 as HmacKeyInit>::new_from_slice(&[0u8; 32])
            .expect("HMAC-SHA256 accepts any key length");
        extract.update(&self.bytes);
        let prk = extract.finalize().into_bytes();

        // HKDF-Expand(PRK, info, L = 32) -> T(1) = HMAC(PRK, info || 0x01)
        let mut expand = <HmacSha256 as HmacKeyInit>::new_from_slice(&prk)
            .expect("HMAC-SHA256 accepts any key length");
        expand.update(info);
        expand.update(&[0x01]);
        let mut out = [0u8; 32];
        out.copy_from_slice(&expand.finalize().into_bytes());
        out
    }

    /// Compute HMAC-SHA256 under a domain-separated subkey derived from `MasterKey`.
    pub fn hmac_sha256_subkey(&self, info: &[u8], data: &[u8]) -> [u8; 32] {
        let subkey = self.derive_subkey(info);
        let mut mac = <HmacSha256 as HmacKeyInit>::new_from_slice(&subkey)
            .expect("HMAC-SHA256 accepts any key length");
        mac.update(data);
        let mut out = [0u8; 32];
        out.copy_from_slice(&mac.finalize().into_bytes());
        out
    }

    /// Compute the canonical keyed HMAC-SHA256 hex digest for an SMTP recipient email address
    /// so `smtp_recipients` never stores plaintext emails in SQLite.
    pub fn smtp_recipient_hmac(&self, raw_email: &str) -> String {
        let normalized = normalize_email_address(raw_email);
        hex::encode(self.hmac_sha256_subkey(b"fc-smtp-addr-v1", normalized.as_bytes()))
    }

    /// Sign a non-expiring, PII-free RFC 8058 unsubscribe token:
    /// `base64url_no_pad(dest_id_be_8 || recipient_id_be_8 || hmac_sha256(subkey, payload)[..16])`.
    pub fn sign_unsubscribe_token(&self, destination_id: i64, recipient_id: i64) -> String {
        let mut payload = [0u8; 16];
        payload[..8].copy_from_slice(&destination_id.to_be_bytes());
        payload[8..16].copy_from_slice(&recipient_id.to_be_bytes());
        let tag = self.hmac_sha256_subkey(b"fc-unsub-v1", &payload);

        let mut token_bytes = [0u8; 32];
        token_bytes[..16].copy_from_slice(&payload);
        token_bytes[16..32].copy_from_slice(&tag[..16]);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token_bytes)
    }

    /// Verify and decode an opaque `/u/{token}` unsubscribe token using constant-time MAC check.
    pub fn verify_unsubscribe_token(&self, token: &str) -> Option<(i64, i64)> {
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(token.trim())
            .ok()?;
        if decoded.len() != 32 {
            return None;
        }
        let (payload, provided_tag) = decoded.split_at(16);
        let expected_tag = self.hmac_sha256_subkey(b"fc-unsub-v1", payload);
        if !constant_time_eq(provided_tag, &expected_tag[..16]) {
            return None;
        }
        let dest_id = i64::from_be_bytes(payload[..8].try_into().ok()?);
        let recipient_id = i64::from_be_bytes(payload[8..16].try_into().ok()?);
        Some((dest_id, recipient_id))
    }

    /// Sign a time-bounded double-opt-in verification link token bound to the current active
    /// `code_sha256` prefix so issuing a fresh verification email invalidates older links.
    pub fn sign_verify_token(
        &self,
        destination_id: i64,
        recipient_id: i64,
        expires_at: i64,
        code_sha256_hex: &str,
    ) -> String {
        let code_prefix = decode_first_8_hex_bytes(code_sha256_hex);
        let mut payload = [0u8; 32];
        payload[..8].copy_from_slice(&destination_id.to_be_bytes());
        payload[8..16].copy_from_slice(&recipient_id.to_be_bytes());
        payload[16..24].copy_from_slice(&expires_at.to_be_bytes());
        payload[24..32].copy_from_slice(&code_prefix);
        let tag = self.hmac_sha256_subkey(b"fc-verify-v1", &payload);

        let mut token_bytes = [0u8; 48];
        token_bytes[..32].copy_from_slice(&payload);
        token_bytes[32..48].copy_from_slice(&tag[..16]);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token_bytes)
    }

    /// Verify and decode an opaque `/v/{token}` confirmation link token.
    /// Returns `(destination_id, recipient_id, expires_at, code_sha256_prefix_8)`.
    pub fn verify_verify_token(&self, token: &str) -> Option<(i64, i64, i64, [u8; 8])> {
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(token.trim())
            .ok()?;
        if decoded.len() != 48 {
            return None;
        }
        let (payload, provided_tag) = decoded.split_at(32);
        let expected_tag = self.hmac_sha256_subkey(b"fc-verify-v1", payload);
        if !constant_time_eq(provided_tag, &expected_tag[..16]) {
            return None;
        }
        let dest_id = i64::from_be_bytes(payload[..8].try_into().ok()?);
        let recipient_id = i64::from_be_bytes(payload[8..16].try_into().ok()?);
        let expires_at = i64::from_be_bytes(payload[16..24].try_into().ok()?);
        let code_prefix: [u8; 8] = payload[24..32].try_into().ok()?;
        Some((dest_id, recipient_id, expires_at, code_prefix))
    }
}

/// Constant-time byte-slice equality check with an optimization barrier
/// (prevents timing oracles on HMAC/code digests).
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= std::hint::black_box(x ^ y);
    }
    std::hint::black_box(diff) == 0
}

/// Extract the first 8 raw bytes from a hex-encoded SHA-256 digest (zero-padded if malformed).
pub fn decode_first_8_hex_bytes(hex_str: &str) -> [u8; 8] {
    let mut out = [0u8; 8];
    if let Ok(raw) = hex::decode(hex_str.trim()) {
        let n = raw.len().min(8);
        out[..n].copy_from_slice(&raw[..n]);
    }
    out
}

/// Normalize an SMTP mailbox/email address (`"Name <Alice@Bücher.DE>"` -> `"alice@xn--bcher-kva.de"`),
/// applying ASCII lowercasing and UTS-46 IDNA Punycode folding on the domain portion.
pub fn normalize_email_address(raw: &str) -> String {
    let trimmed = raw.trim();
    let addr = if let Some(start) = trimmed.rfind('<')
        && let Some(end) = trimmed[start + 1..].find('>')
    {
        trimmed[start + 1..start + 1 + end].trim()
    } else {
        trimmed
    };

    if let Some((local, domain)) = addr.rsplit_once('@') {
        let local_lower = local.trim().to_ascii_lowercase();
        let domain_trimmed = domain.trim().trim_end_matches('.');
        let domain_norm = match url::Host::parse(domain_trimmed) {
            Ok(url::Host::Domain(ascii_domain)) => ascii_domain.to_ascii_lowercase(),
            Ok(other) => other.to_string().to_ascii_lowercase(),
            Err(_) => domain_trimmed.to_ascii_lowercase(),
        };
        format!("{local_lower}@{domain_norm}")
    } else {
        addr.to_ascii_lowercase()
    }
}

/// Convert any internationalized domain name (IDN) inside a raw SMTP address or RFC 5322 mailbox
/// (`"Name <alice@bücher.de>"` -> `"Name <alice@xn--bcher-kva.de>"`) to ASCII Punycode via UTS-46
/// so `lettre` accepts it and non-`SMTPUTF8` mail servers deliver it reliably.
pub fn email_to_ascii_punycode(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_ascii() {
        return trimmed.to_string();
    }

    let convert_bare = |bare: &str| -> String {
        let b = bare.trim();
        let Some((local, domain)) = b.rsplit_once('@') else {
            return b.to_string();
        };
        let domain_clean = domain.trim().trim_end_matches('.');
        if domain_clean.is_ascii() {
            return b.to_string();
        }
        match url::Host::parse(domain_clean) {
            Ok(url::Host::Domain(ascii_domain)) => format!("{}@{ascii_domain}", local.trim()),
            _ => b.to_string(),
        }
    };

    if let Some(start) = trimmed.rfind('<')
        && let Some(rel_end) = trimmed[start + 1..].find('>')
    {
        let end = start + 1 + rel_end;
        let converted = convert_bare(&trimmed[start + 1..end]);
        return format!("{}<{converted}>{}", &trimmed[..start], &trimmed[end + 1..]);
    }

    convert_bare(trimmed)
}

/// Mask an email address for public confirmation pages and diagnostics
/// (`alice@example.com` -> `a***e@example.com`).
pub fn mask_email_address(raw: &str) -> String {
    let norm = normalize_email_address(raw);
    let Some((local, domain)) = norm.split_once('@') else {
        return "***".to_string();
    };
    let chars: Vec<char> = local.chars().collect();
    let masked_local = match chars.len() {
        0 => "***".to_string(),
        1 | 2 => format!("{}***", chars[0]),
        _ => format!("{}***{}", chars[0], chars[chars.len() - 1]),
    };
    format!("{masked_local}@{domain}")
}

/// Redact any email addresses embedded in free-form SMTP server diagnostics (such as
/// `550 5.1.1 <alice@example.com>: User unknown`) so plaintext recipient PII never reaches
/// SQLite diagnostic columns or readonly MCP tools.
pub fn mask_emails_in_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        let ch = text[i..].chars().next().expect("valid char boundary");
        if ch.is_whitespace() {
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        let start = i;
        while i < bytes.len() {
            let c = text[i..].chars().next().expect("valid char boundary");
            if c.is_whitespace() {
                break;
            }
            i += c.len_utf8();
        }
        let token = &text[start..i];
        let leading_len = token
            .bytes()
            .take_while(|b| matches!(b, b'<' | b'(' | b'[' | b'"' | b'\''))
            .count();
        let after_lead = &token[leading_len..];
        let trailing_len = after_lead
            .bytes()
            .rev()
            .take_while(|b| {
                matches!(
                    b,
                    b'>' | b')' | b']' | b'"' | b'\'' | b':' | b';' | b',' | b'.'
                )
            })
            .count();
        let core_end = after_lead.len().saturating_sub(trailing_len);
        let core = &after_lead[..core_end];
        if let Some((local, domain)) = core.rsplit_once('@')
            && !local.is_empty()
            && domain.contains('.')
            && !domain.starts_with('.')
            && !domain.ends_with('.')
        {
            out.push_str(&token[..leading_len]);
            out.push_str(&mask_email_address(core));
            out.push_str(&after_lead[core_end..]);
        } else {
            out.push_str(token);
        }
    }
    out
}

/// Pure-Rust RFC 1321 MD5 lowercase hex digest (used exclusively for YOURLS `timed_signature`
/// API authentication `md5(timestamp + signature_token)` — never for password or security hashing).
pub fn md5_hex(input: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut msg = input.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0x00);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    let mut a0: u32 = 0x6745_2301;
    let mut b0: u32 = 0xefcd_ab89;
    let mut c0: u32 = 0x98ba_dcfe;
    let mut d0: u32 = 0x1032_5476;

    for chunk in msg.as_chunks::<64>().0 {
        let mut m = [0u32; 16];
        for (i, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            m[i] = u32::from_le_bytes(*word);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | ((!b) & d), i),
                16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | (!d)), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut digest = [0u8; 16];
    digest[0..4].copy_from_slice(&a0.to_le_bytes());
    digest[4..8].copy_from_slice(&b0.to_le_bytes());
    digest[8..12].copy_from_slice(&c0.to_le_bytes());
    digest[12..16].copy_from_slice(&d0.to_le_bytes());
    hex::encode(digest)
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
