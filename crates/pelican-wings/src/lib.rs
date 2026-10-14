//! Talking to Wings exactly like the original panel (remote-api.md §4–§5).

pub mod client;
pub mod jwt;

pub use client::{Node, WingsClient, WingsError};
