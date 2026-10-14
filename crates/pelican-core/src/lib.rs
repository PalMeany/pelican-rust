//! Domain logic of pelican-rs (ADR-0001): configuration, application state,
//! authentication/authorization and the services mirroring `app/Services`.

pub mod activity;
pub mod auth;
pub mod config;
pub mod env;
pub mod events;
pub mod net;
pub mod permissions;
pub mod php;
pub mod rate_limit;
pub mod server_config;
pub mod server_state;
pub mod state;
pub mod time;
