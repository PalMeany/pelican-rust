# pelican-rust

A backwards-compatible reimplementation of the [Pelican](https://pelican.dev) game server panel in Rust.
It is a drop-in replacement for the PHP/Laravel panel: point it at an existing Pelican installation's
database and `.env` and it serves the same panel, with [Wings](https://github.com/pelican-dev/wings)
(Go) unchanged.

## Compatibility goals

- Works on an existing database — MySQL, MariaDB, PostgreSQL or SQLite — **without migrations**.
- Same `APP_KEY`: bcrypt passwords, TOTP 2FA, passkeys, encrypted columns and API keys keep working.
- Byte-compatible Wings Remote API; compatible Application and Client APIs (same JSON, errors, validation messages).
- Egg import/export in the original formats (JSON and YAML).
- Functional parity of the web UI (server-side rendered with [minijinja](https://github.com/mitsuhiko/minijinja) + [htmx](https://htmx.org)).
- Plugins: the original `plugin.json` format and lifecycle with sandboxed WASM components
  (`docs/plugins.md`), and the original PHP plugins through an optional sidecar running the original panel
  (ADR-0020).

Compatibility is checked by **differential tests**: every scenario runs against the original panel and
pelican-rust in Docker on all four databases, and responses, database changes and Wings traffic must match
(`difftest/`, `crates/difftest`).

## Status

Milestones M1–M7 (database layer, auth, Remote/Client/Application APIs, web UI, WASM plugins, model events)
are implemented; M8 (PHP plugin sidecar) is in progress. See `PLAN.md` and `PROGRESS.md` (in Russian), and
the architecture decision records in `docs/decisions/`.

## Layout

| Crate | Purpose |
|---|---|
| `crates/pelican` | the binary: configuration, scheduler, plugin runtime start-up, HTTP server |
| `crates/pelican-db` | sea-orm entities and Laravel-compatible reads/writes for the four databases |
| `crates/pelican-core` | config, encryption (`APP_KEY`), auth, activity log, server configuration |
| `crates/pelican-api` | Application, Client and Remote (Wings) APIs, jobs, model events, PHP sidecar client |
| `crates/pelican-web` | the web UI (admin, app and server panels) |
| `crates/pelican-wings` | Wings HTTP client and JWTs |
| `crates/pelican-plugins`, `crates/pelican-plugin-sdk` | plugin registry and WASM runtime (wasmtime), plugin SDK |
| `crates/laravel-compat`, `crates/egg-format`, `crates/cron-php` | PHP/Laravel formats ported exactly |
| `crates/difftest` | the differential test harness |

## Building

Builds run in a Linux container (`scripts/cargo build -p pelican`, see `scripts/dev-image/`); a plain
`cargo build --release -p pelican` works on Linux with a Rust toolchain. Run with
`pelican --path <pelican data dir> serve --listen 0.0.0.0:8080`.

## License

This project is a derivative work of [Pelican Panel](https://github.com/pelican-dev/panel) — it ports its
behaviour and ships its translations — and is licensed under the GNU Affero General Public License v3.0
only (`LICENSE`), like the original. Pelican is © its contributors.
