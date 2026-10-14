//! `NodeJWTService` (lcobucci/jwt 5 + `TimestampDates`), remote-api.md §5.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum JwtError {
    /// `Hmac::sign` rejects keys shorter than 256 bits (`InvalidKeyProvided`).
    #[error("Key provided is shorter than 256 bits, only 0 bits provided")]
    KeyTooShort,
}

pub struct JwtBuilder {
    pub issuer: String,
    pub audience: String,
    /// Pre-hash identifier (`identifiedBy`).
    pub identified_by: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub subject: Option<String>,
    pub claims: Vec<(String, Value)>,
    pub user_uuid: Option<String>,
    pub scope: String,
    /// `Str::random(16)` — injectable for tests.
    pub unique_id: String,
}

/// `Str::random($n)`: `[A-Za-z0-9]`.
pub fn str_random(n: usize) -> String {
    use rand::Rng;
    const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..n).map(|_| ALPHA[rng.random_range(0..ALPHA.len())] as char).collect()
}

impl JwtBuilder {
    pub fn sign(&self, key: &str) -> Result<String, JwtError> {
        if key.len() < 32 {
            return Err(JwtError::KeyTooShort);
        }
        let jti = hex::encode(Sha256::digest(self.identified_by.as_bytes()));
        let mut header = Map::new();
        header.insert("typ".into(), "JWT".into());
        header.insert("alg".into(), "HS256".into());
        header.insert("jti".into(), jti.clone().into());
        if let Some(sub) = &self.subject {
            header.insert("sub".into(), sub.clone().into());
        }
        let mut payload = Map::new();
        payload.insert("iss".into(), self.issuer.clone().into());
        payload.insert("aud".into(), Value::Array(vec![self.audience.clone().into()]));
        payload.insert("jti".into(), jti.into());
        payload.insert("iat".into(), self.issued_at.into());
        payload.insert("nbf".into(), (self.issued_at - 300).into());
        payload.insert("exp".into(), self.expires_at.into());
        if let Some(sub) = &self.subject {
            payload.insert("sub".into(), sub.clone().into());
        }
        for (k, v) in &self.claims {
            payload.insert(k.clone(), v.clone());
        }
        if let Some(u) = &self.user_uuid {
            payload.insert("user_uuid".into(), u.clone().into());
        }
        payload.insert("scope".into(), self.scope.clone().into());
        payload.insert("unique_id".into(), self.unique_id.clone().into());
        let flags = laravel_compat::json::JSON_UNESCAPED_SLASHES | laravel_compat::json::JSON_UNESCAPED_UNICODE;
        let enc = |v: &Map<String, Value>| {
            URL_SAFE_NO_PAD.encode(
                laravel_compat::json::to_string_with_flags(&Value::Object(v.clone()), flags).unwrap_or_default(),
            )
        };
        let signing_input = format!("{}.{}", enc(&header), enc(&payload));
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key.as_bytes()).map_err(|_| JwtError::KeyTooShort)?;
        mac.update(signing_input.as_bytes());
        let sig = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        Ok(format!("{signing_input}.{sig}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test vector from remote-api.md §5.4.
    #[test]
    fn spec_vector() {
        let b = JwtBuilder {
            issuer: "https://panel.example.com".into(),
            audience: "https://node.example.com:8080".into(),
            identified_by: "10b5c6a0e-3f0e-4c1e-9d55-6c1d7c0a2d11".into(),
            issued_at: 1760000000,
            expires_at: 1760000600,
            subject: None,
            claims: vec![
                ("server_uuid".into(), "0b5c6a0e-3f0e-4c1e-9d55-6c1d7c0a2d11".into()),
                ("permissions".into(), serde_json::json!(["*"])),
            ],
            user_uuid: Some("5d3c9a1b-7e2f-4b8a-9c0d-1e2f3a4b5c6d".into()),
            scope: "websocket".into(),
            unique_id: "AbCdEfGhIjKlMnOp".into(),
        };
        let t = b.sign("0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ01").unwrap();
        assert_eq!(
            t,
            "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiIsImp0aSI6IjI3OTQ2ZDQ3M2EyN2Q2ZGM2ZjVhN2JkOGIyMzUxNjkwMmFlMmVmMjg5MTJhOTVjNmU3ZWUxOTYyMzEyNzc0YmUifQ.eyJpc3MiOiJodHRwczovL3BhbmVsLmV4YW1wbGUuY29tIiwiYXVkIjpbImh0dHBzOi8vbm9kZS5leGFtcGxlLmNvbTo4MDgwIl0sImp0aSI6IjI3OTQ2ZDQ3M2EyN2Q2ZGM2ZjVhN2JkOGIyMzUxNjkwMmFlMmVmMjg5MTJhOTVjNmU3ZWUxOTYyMzEyNzc0YmUiLCJpYXQiOjE3NjAwMDAwMDAsIm5iZiI6MTc1OTk5OTcwMCwiZXhwIjoxNzYwMDAwNjAwLCJzZXJ2ZXJfdXVpZCI6IjBiNWM2YTBlLTNmMGUtNGMxZS05ZDU1LTZjMWQ3YzBhMmQxMSIsInBlcm1pc3Npb25zIjpbIioiXSwidXNlcl91dWlkIjoiNWQzYzlhMWItN2UyZi00YjhhLTljMGQtMWUyZjNhNGI1YzZkIiwic2NvcGUiOiJ3ZWJzb2NrZXQiLCJ1bmlxdWVfaWQiOiJBYkNkRWZHaElqS2xNbk9wIn0.lOQBrookVmRHAOSo1A2TDgYUWo9BQ3xj-Val2j74xzE"
        );
    }
}
