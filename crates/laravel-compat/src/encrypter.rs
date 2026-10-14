//! `Illuminate\Encryption\Encrypter` (AES-128/256-CBC + HMAC-SHA256).
//!
//! Payload = base64(json{"iv","value","mac","tag"}) where `value` is the base64
//! ciphertext (openssl_encrypt with options 0, PKCS#7), `iv` the base64 IV and
//! `mac` = hex(HMAC-SHA256(key, iv_b64 . value_b64)). `encrypt()` serializes the
//! value with PHP `serialize()` first; `encryptString()` does not.

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::php_serialize::{self, PhpValue};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecryptError {
    #[error("The payload is invalid.")]
    InvalidPayload,
    #[error("The MAC is invalid.")]
    InvalidMac,
    #[error("Could not decrypt the data.")]
    CouldNotDecrypt,
    #[error("Unable to use tag because the cipher algorithm does not support AEAD.")]
    UnexpectedTag,
    #[error("could not unserialize decrypted value: {0}")]
    Unserialize(String),
}

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("Unsupported cipher or incorrect key length. Supported ciphers are: aes-128-cbc, aes-256-cbc.")]
    Unsupported,
    #[error("invalid base64 in key")]
    Base64,
}

#[derive(Clone)]
pub struct Encrypter {
    key: Vec<u8>,
    previous_keys: Vec<Vec<u8>>,
}

impl std::fmt::Debug for Encrypter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Encrypter { .. }")
    }
}

/// Laravel `EncryptionServiceProvider::parseKey`: `base64:` prefix → decoded, else raw bytes.
pub fn parse_key(key: &str) -> Result<Vec<u8>, KeyError> {
    match key.strip_prefix("base64:") {
        Some(b64) => STANDARD.decode(b64.trim()).map_err(|_| KeyError::Base64),
        None => Ok(key.as_bytes().to_vec()),
    }
}

impl Encrypter {
    /// `app_key` as in `APP_KEY`, `previous` as in `APP_PREVIOUS_KEYS` (comma separated).
    pub fn from_env_values(app_key: &str, previous: Option<&str>) -> Result<Self, KeyError> {
        let key = parse_key(app_key)?;
        let previous_keys = previous
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(parse_key)
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(key, previous_keys)
    }

    pub fn new(key: Vec<u8>, previous_keys: Vec<Vec<u8>>) -> Result<Self, KeyError> {
        let ok = |k: &[u8]| k.len() == 16 || k.len() == 32;
        if !ok(&key) || !previous_keys.iter().all(|k| ok(k)) {
            return Err(KeyError::Unsupported);
        }
        Ok(Encrypter { key, previous_keys })
    }

    /// `Crypt::encryptString()` — no serialization.
    pub fn encrypt_string(&self, value: &str) -> String {
        self.encrypt_bytes(value.as_bytes())
    }

    /// `encrypt($value)` for a string value — PHP-serialized first.
    pub fn encrypt_serialized(&self, value: &PhpValue) -> String {
        self.encrypt_bytes(&php_serialize::serialize(value))
    }

    pub fn encrypt_bytes(&self, plaintext: &[u8]) -> String {
        let mut iv = [0u8; 16];
        rand::rng().fill_bytes(&mut iv);
        self.encrypt_with_iv(plaintext, &iv)
    }

    fn encrypt_with_iv(&self, plaintext: &[u8], iv: &[u8; 16]) -> String {
        let ct = match self.key.len() {
            16 => cbc::Encryptor::<aes::Aes128>::new_from_slices(&self.key, iv)
                .expect("key length checked")
                .encrypt_padded_vec_mut::<Pkcs7>(plaintext),
            _ => cbc::Encryptor::<aes::Aes256>::new_from_slices(&self.key, iv)
                .expect("key length checked")
                .encrypt_padded_vec_mut::<Pkcs7>(plaintext),
        };
        let iv_b64 = STANDARD.encode(iv);
        let value_b64 = STANDARD.encode(ct);
        let mac = hmac_hex(&self.key, &iv_b64, &value_b64);
        // json_encode([...], JSON_UNESCAPED_SLASHES): base64 never needs escaping.
        let json = format!(r#"{{"iv":"{iv_b64}","value":"{value_b64}","mac":"{mac}","tag":""}}"#);
        STANDARD.encode(json)
    }

    /// `Crypt::decryptString()` — raw bytes as UTF-8 string.
    pub fn decrypt_string(&self, payload: &str) -> Result<String, DecryptError> {
        let bytes = self.decrypt_bytes(payload)?;
        String::from_utf8(bytes).map_err(|_| DecryptError::CouldNotDecrypt)
    }

    /// `decrypt($payload)` — decrypt then `unserialize()`.
    pub fn decrypt_serialized(&self, payload: &str) -> Result<PhpValue, DecryptError> {
        let bytes = self.decrypt_bytes(payload)?;
        php_serialize::unserialize(&bytes).map_err(|e| DecryptError::Unserialize(e.to_string()))
    }

    /// Decrypt a value that may have been written either by `encrypt()` (serialized)
    /// or by `encryptString()` (raw), returning the string. Old Pterodactyl-era rows
    /// use the former, current casts the latter.
    pub fn decrypt_string_lenient(&self, payload: &str) -> Result<String, DecryptError> {
        let bytes = self.decrypt_bytes(payload)?;
        if let Ok(PhpValue::String(s)) = php_serialize::unserialize(&bytes) {
            if let Ok(s) = String::from_utf8(s) {
                return Ok(s);
            }
        }
        String::from_utf8(bytes).map_err(|_| DecryptError::CouldNotDecrypt)
    }

    pub fn decrypt_bytes(&self, payload: &str) -> Result<Vec<u8>, DecryptError> {
        let json = php_base64_decode(payload).ok_or(DecryptError::InvalidPayload)?;
        let v: serde_json::Value = serde_json::from_slice(&json).map_err(|_| DecryptError::InvalidPayload)?;
        let field = |k: &str| v.get(k).and_then(|x| x.as_str());
        let (Some(iv_b64), Some(value_b64), Some(mac)) = (field("iv"), field("value"), field("mac")) else {
            return Err(DecryptError::InvalidPayload);
        };
        if v.get("tag").is_some_and(|t| !t.is_null() && !t.is_string()) {
            return Err(DecryptError::InvalidPayload);
        }
        let iv = STANDARD.decode(iv_b64).map_err(|_| DecryptError::InvalidPayload)?;
        if iv.len() != 16 {
            return Err(DecryptError::InvalidPayload);
        }
        // ensureTagIsValid: a non-empty tag is not allowed for CBC.
        if field("tag").is_some_and(|t| !t.is_empty()) {
            return Err(DecryptError::UnexpectedTag);
        }
        let key = std::iter::once(&self.key)
            .chain(self.previous_keys.iter())
            .find(|k| hmac_hex(k, iv_b64, value_b64).as_bytes().ct_eq(mac.as_bytes()).into())
            .ok_or(DecryptError::InvalidMac)?;
        // openssl_decrypt(options 0) base64-decodes the value itself.
        let ct = php_base64_decode(value_b64).ok_or(DecryptError::CouldNotDecrypt)?;
        let pt = match key.len() {
            16 => cbc::Decryptor::<aes::Aes128>::new_from_slices(key, &iv)
                .map_err(|_| DecryptError::CouldNotDecrypt)?
                .decrypt_padded_vec_mut::<Pkcs7>(&ct),
            _ => cbc::Decryptor::<aes::Aes256>::new_from_slices(key, &iv)
                .map_err(|_| DecryptError::CouldNotDecrypt)?
                .decrypt_padded_vec_mut::<Pkcs7>(&ct),
        };
        pt.map_err(|_| DecryptError::CouldNotDecrypt)
    }
}

fn hmac_hex(key: &[u8], iv_b64: &str, value_b64: &str) -> String {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(iv_b64.as_bytes());
    mac.update(value_b64.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// PHP `base64_decode($s)` in non-strict mode: characters outside the base64
/// alphabet are skipped, padding is optional.
pub fn php_base64_decode(s: &str) -> Option<Vec<u8>> {
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '/')
        .collect();
    base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    )
    .decode(cleaned)
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "base64:Rm9yRGlmZmVyZW50aWFsVGVzdGluZ09ubHkhIDMyQiE=";

    #[test]
    fn roundtrip_string_and_serialized() {
        let e = Encrypter::from_env_values(KEY, None).unwrap();
        let p = e.encrypt_string("secret/token");
        assert_eq!(e.decrypt_string(&p).unwrap(), "secret/token");
        let p = e.encrypt_serialized(&PhpValue::String(b"hello".to_vec()));
        assert_eq!(e.decrypt_bytes(&p).unwrap(), b"s:5:\"hello\";");
        assert_eq!(e.decrypt_string_lenient(&p).unwrap(), "hello");
    }

    #[test]
    fn previous_keys_and_bad_mac() {
        let old = Encrypter::from_env_values("base64:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", None).unwrap();
        let p = old.encrypt_string("x");
        let cur = Encrypter::from_env_values(KEY, None).unwrap();
        assert_eq!(cur.decrypt_string(&p), Err(DecryptError::InvalidMac));
        let both =
            Encrypter::from_env_values(KEY, Some("base64:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")).unwrap();
        assert_eq!(both.decrypt_string(&p).unwrap(), "x");
    }

    /// Payload produced by the original panel (PHP 8.5, Laravel 13) with the
    /// difftest APP_KEY: `Crypt::encryptString('wings-token/+=')` and `encrypt('wings-token/+=')`.
    #[test]
    fn decrypts_php_payloads() {
        use crate::fixtures::*;
        let e = Encrypter::from_env_values(KEY, None).unwrap();
        assert_eq!(e.decrypt_string(ENCRYPT_STRING).unwrap(), "wings-token/+=");
        assert_eq!(e.decrypt_string_lenient(ENCRYPT_STRING).unwrap(), "wings-token/+=");
        assert_eq!(e.decrypt_string_lenient(ENCRYPT).unwrap(), "wings-token/+=");
        assert_eq!(e.decrypt_serialized(ENCRYPT_INT_42).unwrap(), PhpValue::Int(42));
        let arr = e.decrypt_serialized(ENCRYPT_ARRAY).unwrap();
        assert_eq!(arr.get("a"), Some(&PhpValue::Int(1)));
    }
}
