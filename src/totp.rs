//! RFC 6238 TOTP (HMAC-SHA1, 6 digits, 30 s step) primitives, Base32 secrets, `otpauth://` URLs,
//! inline SVG QR codes, and single-use recovery codes for the WebUI administrator account.

use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;

use crate::crypto::sha256_hex;
use crate::error::{AppError, AppResult};

/// TOTP time-step in seconds.
pub const TOTP_STEP_SECS: i64 = 30;

/// Number of digits in a TOTP code.
pub const TOTP_DIGITS: u32 = 6;

/// Secret length in bytes (160 bits).
pub const TOTP_SECRET_LEN: usize = 20;

/// Number of recovery codes generated per batch.
pub const RECOVERY_CODE_COUNT: usize = 10;

const BASE32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
const RECOVERY_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Generate a fresh random 160-bit secret.
pub fn generate_secret() -> Vec<u8> {
    let bytes: [u8; TOTP_SECRET_LEN] = rand::random();
    bytes.to_vec()
}

/// RFC 4648 Base32 (no padding).
pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits = 0u32;
    for &byte in data {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            let idx = ((buffer >> (bits - 5)) & 0x1f) as usize;
            out.push(char::from(BASE32_ALPHABET[idx]));
            bits -= 5;
        }
    }
    if bits > 0 {
        let idx = ((buffer << (5 - bits)) & 0x1f) as usize;
        out.push(char::from(BASE32_ALPHABET[idx]));
    }
    out
}

/// Decode Base32 (case-insensitive, ignores spaces/dashes/padding).
pub fn base32_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 5 / 8);
    let mut buffer: u32 = 0;
    let mut bits = 0u32;
    for ch in input.chars() {
        if matches!(ch, ' ' | '-' | '=') {
            continue;
        }
        let upper = ch.to_ascii_uppercase();
        let val = BASE32_ALPHABET
            .iter()
            .position(|&c| char::from(c) == upper)?;
        buffer = (buffer << 5) | u32::try_from(val).ok()?;
        bits += 5;
        if bits >= 8 {
            out.push(u8::try_from((buffer >> (bits - 8)) & 0xff).ok()?);
            bits -= 8;
        }
    }
    Some(out)
}

/// RFC 4226 HOTP value for `counter`.
fn hotp(secret: &[u8], counter: u64) -> u32 {
    let mut mac =
        <Hmac<Sha1> as KeyInit>::new_from_slice(secret).expect("HMAC accepts keys of any length");
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest[digest.len() - 1] & 0x0f);
    let bin = (u32::from(digest[offset] & 0x7f) << 24)
        | (u32::from(digest[offset + 1]) << 16)
        | (u32::from(digest[offset + 2]) << 8)
        | u32::from(digest[offset + 3]);
    bin % 10u32.pow(TOTP_DIGITS)
}

/// Current time-step index for `unix_secs`.
pub fn time_step(unix_secs: i64) -> i64 {
    unix_secs.div_euclid(TOTP_STEP_SECS)
}

/// Zero-padded TOTP code for `step`.
pub fn code_for_step(secret: &[u8], step: i64) -> String {
    let counter = u64::try_from(step).unwrap_or(0);
    format!(
        "{:0width$}",
        hotp(secret, counter),
        width = TOTP_DIGITS as usize
    )
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Verify `code` against `secret` within ±1 step of `now`. Returns the matched step if it is
/// strictly newer than `last_used_step` (replay protection).
pub fn verify_code(
    secret: &[u8],
    code: &str,
    now_unix: i64,
    last_used_step: Option<i64>,
) -> Option<i64> {
    let cleaned: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.len() != TOTP_DIGITS as usize || !cleaned.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let current = time_step(now_unix);
    let mut matched: Option<i64> = None;
    for step in [current - 1, current, current + 1] {
        let expected = code_for_step(secret, step);
        if constant_time_eq(expected.as_bytes(), cleaned.as_bytes()) {
            matched = Some(step);
        }
    }
    matched.filter(|step| last_used_step.is_none_or(|last| *step > last))
}

/// `otpauth://totp/...` provisioning URL.
pub fn otpauth_url(issuer: &str, account: &str, secret_base32: &str) -> String {
    let label = format!("{}:{}", percent_encode(issuer), percent_encode(account));
    format!(
        "otpauth://totp/{label}?secret={secret_base32}&issuer={}&algorithm=SHA1&digits={TOTP_DIGITS}&period={TOTP_STEP_SECS}",
        percent_encode(issuer)
    )
}

fn percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Render `data` as an inline SVG QR code.
pub fn qr_svg(data: &str) -> AppResult<String> {
    let code = qrcode::QrCode::new(data.as_bytes())
        .map_err(|err| AppError::Internal(anyhow::anyhow!("failed to render QR code: {err}")))?;
    Ok(code
        .render::<qrcode::render::svg::Color<'_>>()
        .min_dimensions(220, 220)
        .quiet_zone(true)
        .dark_color(qrcode::render::svg::Color("#111827"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build())
}

/// Generate [`RECOVERY_CODE_COUNT`] single-use recovery codes formatted `XXXX-XXXX-XXXX`.
pub fn generate_recovery_codes() -> Vec<String> {
    (0..RECOVERY_CODE_COUNT)
        .map(|_| {
            let raw: String = (0..12)
                .map(|_| {
                    let idx = rand::random_range(0..RECOVERY_ALPHABET.len());
                    char::from(RECOVERY_ALPHABET[idx])
                })
                .collect();
            format!("{}-{}-{}", &raw[0..4], &raw[4..8], &raw[8..12])
        })
        .collect()
}

/// Normalize a user-supplied recovery code (strip dashes/spaces, uppercase).
pub fn normalize_recovery_code(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Whether `raw` looks like a recovery code attempt rather than a 6-digit TOTP.
pub fn looks_like_recovery_code(raw: &str) -> bool {
    normalize_recovery_code(raw).len() == 12
}

/// SHA-256 digest used for at-rest storage of a normalized recovery code.
pub fn hash_recovery_code(raw: &str) -> String {
    sha256_hex(format!("feedcourier:recovery:v1:{}", normalize_recovery_code(raw)).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6238_sha1_vectors() {
        let secret = b"12345678901234567890";
        // RFC 6238 Appendix B (SHA-1), truncated to 6 digits.
        assert_eq!(code_for_step(secret, time_step(59)), "287082");
        assert_eq!(code_for_step(secret, time_step(1_111_111_109)), "081804");
        assert_eq!(code_for_step(secret, time_step(1_234_567_890)), "005924");
        assert_eq!(code_for_step(secret, time_step(20_000_000_000)), "353130");
    }

    #[test]
    fn base32_roundtrip() {
        let data = generate_secret();
        let enc = base32_encode(&data);
        assert_eq!(base32_decode(&enc).unwrap(), data);
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32_decode("mzxw 6ytb-oi").unwrap(), b"foobar");
    }

    #[test]
    fn replay_and_window() {
        let secret = b"12345678901234567890";
        let now = 1_111_111_109;
        let code = code_for_step(secret, time_step(now));
        let step = verify_code(secret, &code, now, None).unwrap();
        assert_eq!(step, time_step(now));
        assert!(verify_code(secret, &code, now, Some(step)).is_none());
        assert!(verify_code(secret, &code, now + 30, Some(step - 1)).is_some());
        assert!(verify_code(secret, &code, now + 120, None).is_none());
        assert!(verify_code(secret, "12345", now, None).is_none());
    }

    #[test]
    fn recovery_codes_format_and_hash() {
        let codes = generate_recovery_codes();
        assert_eq!(codes.len(), RECOVERY_CODE_COUNT);
        for c in &codes {
            assert_eq!(c.len(), 14);
            assert!(looks_like_recovery_code(c));
            assert_eq!(
                hash_recovery_code(c),
                hash_recovery_code(&c.to_lowercase().replace('-', ""))
            );
        }
    }

    #[test]
    fn qr_and_url() {
        let url = otpauth_url("FeedCourier", "Courier", "JBSWY3DPEHPK3PXP");
        assert!(url.starts_with("otpauth://totp/FeedCourier:Courier?secret=JBSWY3DPEHPK3PXP"));
        assert!(qr_svg(&url).unwrap().contains("<svg"));
    }
}
