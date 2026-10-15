# pelican-rs release image. Data layout matches the original's image: the panel directory
# (`.env`, `storage/`, `plugins/`, `database/` for SQLite) is /pelican-data, so an existing
# Pelican volume can be mounted as is. Serves plain HTTP on :8080 — put a TLS proxy
# (Caddy, nginx) in front, as with the original's php-fpm.
FROM rust:1-trixie AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p pelican \
 && strip target/release/pelican

FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tzdata \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 82 --home /pelican-data pelican \
 && mkdir -p /pelican-data && chown pelican /pelican-data
COPY --from=build /src/target/release/pelican /usr/local/bin/pelican
USER pelican
WORKDIR /pelican-data
VOLUME /pelican-data
EXPOSE 8080
ENTRYPOINT ["pelican", "--path", "/pelican-data"]
CMD ["serve", "--listen", "0.0.0.0:8080"]
