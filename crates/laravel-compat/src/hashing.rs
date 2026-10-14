//! `Illuminate\Hashing\BcryptHasher` (PHP `password_hash(PASSWORD_BCRYPT)`).

/// PHP `hash_equals` (constant time for equal lengths).
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

/// Default `hashing.bcrypt.rounds` (`BCRYPT_ROUNDS`), as in Laravel 13.
pub const DEFAULT_ROUNDS: u32 = 12;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HashError {
    /// Laravel throws when `hashing.bcrypt.verify` is on and the stored hash is not bcrypt.
    #[error("This password does not use the Bcrypt algorithm.")]
    NotBcrypt,
    #[error("bcrypt failure: {0}")]
    Bcrypt(String),
}

/// `Hash::make()` → `$2y$<rounds>$...`.
pub fn make(password: &str, rounds: u32) -> Result<String, HashError> {
    bcrypt::hash_with_result(password, rounds)
        .map(|h| h.format_for_version(bcrypt::Version::TwoY))
        .map_err(|e| HashError::Bcrypt(e.to_string()))
}

fn is_bcrypt(hash: &str) -> bool {
    hash.len() == 60 && (hash.starts_with("$2y$") || hash.starts_with("$2a$") || hash.starts_with("$2b$"))
}

/// `Hash::check()`. An empty stored hash never matches (Laravel returns false).
pub fn check(password: &str, hash: &str) -> Result<bool, HashError> {
    if hash.is_empty() {
        return Ok(false);
    }
    if !is_bcrypt(hash) {
        return Err(HashError::NotBcrypt);
    }
    bcrypt::verify(password, hash).map_err(|e| HashError::Bcrypt(e.to_string()))
}

/// `password_needs_rehash($hash, PASSWORD_BCRYPT, ['cost' => $rounds])`.
pub fn needs_rehash(hash: &str, rounds: u32) -> bool {
    if !hash.starts_with("$2y$") || hash.len() != 60 {
        return true;
    }
    hash.get(4..6).and_then(|c| c.parse::<u32>().ok()) != Some(rounds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_php_hash() {
        assert_eq!(check("password", crate::fixtures::BCRYPT_PASSWORD), Ok(true));
        assert_eq!(check("nope", crate::fixtures::BCRYPT_PASSWORD), Ok(false));
        assert!(!needs_rehash(crate::fixtures::BCRYPT_PASSWORD, 12));
        assert!(needs_rehash(crate::fixtures::BCRYPT_PASSWORD, 10));
    }

    #[test]
    fn make_is_2y() {
        let h = make("x", 4).unwrap();
        assert!(h.starts_with("$2y$04$"));
        assert_eq!(check("x", &h), Ok(true));
    }
}
