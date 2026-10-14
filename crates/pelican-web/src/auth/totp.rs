//! RFC 6238 TOTP compatible with pragmarx/google2fa (SHA-1, 6 digits, 30 s,
//! base32 secrets) as used by Filament's `AppAuthentication`.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use hmac::{Hmac, Mac};
use sha1::Sha1;

const PERIOD: i64 = 30;
const DIGITS: u32 = 6;
/// `AppAuthentication::$codeWindow`.
pub const WINDOW: i64 = 8;
const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut bits: u64 = 0;
    let mut n = 0;
    let mut out = Vec::new();
    for c in s.trim_end_matches('=').bytes() {
        let v = ALPHABET.iter().position(|&a| a == c.to_ascii_uppercase())? as u64;
        bits = (bits << 5) | v;
        n += 5;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
            bits &= (1 << n) - 1;
        }
    }
    Some(out)
}

/// `generateSecretKey(16)`: 16 random base32 characters.
pub fn generate_secret() -> String {
    (0..16).map(|_| ALPHABET[rand::random_range(0..32)] as char).collect()
}

pub fn timestep(unix: i64) -> i64 {
    unix.div_euclid(PERIOD)
}

pub fn code_at(key: &[u8], step: i64) -> String {
    let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(key) else { return String::new() };
    mac.update(&step.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let off = (h[19] & 0x0f) as usize;
    let bin = ((u32::from(h[off]) & 0x7f) << 24)
        | (u32::from(h[off + 1]) << 16)
        | (u32::from(h[off + 2]) << 8)
        | u32::from(h[off + 3]);
    format!("{:0width$}", bin % 10u32.pow(DIGITS), width = DIGITS as usize)
}

/// Matching timestep within `±WINDOW` newer than `after`, if any (`verifyKeyNewer`).
pub fn find_step(secret: &str, code: &str, now: i64, after: Option<i64>) -> Option<i64> {
    let key = base32_decode(secret)?;
    let code = code.trim();
    if code.len() != DIGITS as usize {
        return None;
    }
    let t = timestep(now);
    let from = after.map_or(t - WINDOW, |a| (a + 1).max(t - WINDOW));
    (from..=t + WINDOW)
        .find(|&s| laravel_compat::hashing::constant_time_eq(code_at(&key, s).as_bytes(), code.as_bytes()))
}

/// Last accepted timestep per secret, to reject reuse (Filament caches this).
static LAST: LazyLock<Mutex<HashMap<String, i64>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// `verifyCode($code, $secret, shouldPreventCodeReuse: true)`.
pub fn verify_once(secret: &str, code: &str, now: i64) -> bool {
    let mut last = LAST.lock().unwrap_or_else(|p| p.into_inner());
    let prev = last.get(secret).copied();
    match find_step(secret, code, now, prev) {
        Some(step) => {
            last.insert(secret.to_string(), step);
            true
        }
        None => false,
    }
}

/// `verifyKey($secret, $code, window)` (no reuse protection; used while setting up).
pub fn verify(secret: &str, code: &str, now: i64) -> bool {
    find_step(secret, code, now, None).is_some()
}

/// `otpauth://` provisioning URI (google2fa `getQRCodeUrl`).
pub fn provisioning_uri(issuer: &str, holder: &str, secret: &str) -> String {
    let enc = |s: &str| form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>().replace('+', "%20");
    format!(
        "otpauth://totp/{}:{}?secret={secret}&issuer={}&algorithm=SHA1&digits=6&period=30",
        enc(issuer),
        enc(holder),
        enc(issuer)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6238_vector() {
        // RFC 6238 SHA-1 secret "12345678901234567890", T=59 → 94287082 (8 digits) → 287082.
        let key = b"12345678901234567890";
        assert_eq!(code_at(key, timestep(59)), "287082");
        assert_eq!(code_at(key, timestep(1111111109)), "081804");
    }

    #[test]
    fn base32_roundtrip() {
        assert_eq!(base32_decode("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ").as_deref(), Some(&b"12345678901234567890"[..]));
    }

    #[test]
    fn reuse_rejected() {
        let secret = "GEZDGNBVGY3TQOJQ";
        let key = base32_decode(secret).unwrap_or_default();
        let now = 1_700_000_000;
        let code = code_at(&key, timestep(now));
        assert!(verify_once(secret, &code, now));
        assert!(!verify_once(secret, &code, now));
    }
}
