//! Byte-compatible re-implementations of the Laravel/PHP formats pelican-rs
//! shares with databases and clients of the original panel.

pub mod encrypter;
pub mod hashing;
pub mod json;
pub mod php_serialize;

pub use encrypter::{DecryptError, Encrypter};
pub use php_serialize::PhpValue;

#[cfg(test)]
mod fixtures;
