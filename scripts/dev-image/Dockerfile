# Build/test environment for pelican-rs (runs inside the lima VM "pelican").
FROM rust:1-trixie
RUN apt-get update \
 && apt-get install -y --no-install-recommends mariadb-client postgresql-client sqlite3 jq \
 && rm -rf /var/lib/apt/lists/* \
 && rustup component add rustfmt clippy \
 && rustup target add wasm32-wasip2
ENV CARGO_TARGET_DIR=/target
