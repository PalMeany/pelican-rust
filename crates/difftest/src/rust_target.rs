//! Starting pelican-rs as the second target.

/// Start pelican-rs in-process against the difftest database and return its base URL.
pub async fn start(_backend: &str, _app_key: &str) -> anyhow::Result<String> {
    anyhow::bail!("pelican-rs target is not wired yet: use --self-check or --rust-url")
}
