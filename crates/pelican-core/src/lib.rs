//! Domain logic of pelican-rs (ADR-0001): configuration, application state,
//! authentication/authorization and the services mirroring `app/Services`.

pub mod activity;
pub mod auth;
pub mod config;
pub mod env;
pub mod net;
pub mod rate_limit;
pub mod state;
pub mod time;
