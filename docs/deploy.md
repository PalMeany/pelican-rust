# Deploying pelican-rs and moving from the PHP panel

pelican-rs is a drop-in replacement for the Pelican panel: it uses the **same database, `.env`
(`APP_KEY`), `storage/` and `plugins/`**, and Wings keeps talking to it as to the original. It does not
migrate the database, so you can switch back at any time.

## Requirements

- The panel's database schema must be the version pelican-rs supports. Check with
  `pelican --path /var/www/pelican db:status` (`Ready` is fine; `Outdated` means: update with the original
  panel first). Pelican plugins' own migrations are ignored by this check.
- A reverse proxy for HTTPS (nginx, Caddy, Apache), as with php-fpm. pelican-rs serves plain HTTP.

## Binary + systemd (alongside an existing installation)

1. Back up the database and `/var/www/pelican/.env`.
2. Install the binary: download `pelican-linux-x86_64.tar.gz` from the releases (or `cargo build --release -p pelican`)
   to `/usr/local/bin/pelican`.
3. Check the schema: `sudo -u www-data pelican --path /var/www/pelican db:status`.
4. Stop the original's background services — pelican-rs runs the scheduler and the queue itself:
   `systemctl disable --now pelican-queue` (the queue worker) and remove the `php artisan schedule:run` cron entry.
5. Install `packaging/pelican.service` to `/etc/systemd/system/` and `systemctl enable --now pelican`.
6. Point the web server at `http://127.0.0.1:8080` instead of php-fpm, for example nginx:

   ```nginx
   location / {
       proxy_pass http://127.0.0.1:8080;
       proxy_set_header Host $host;
       proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
       proxy_set_header X-Forwarded-Proto $scheme;
       proxy_http_version 1.1;
       client_max_body_size 100m;
   }
   ```

   Keep `TRUSTED_PROXIES` in `.env` matching the proxy (as for the original), so client IPs and https are seen
   correctly.
7. Users have to sign in again (sessions are not shared with PHP). API keys, 2FA, passkeys and Wings tokens keep
   working.

## Docker

The image keeps the original image's layout: mount the existing `/pelican-data` volume.

```bash
docker run -d --name pelican -p 8080:8080 -v pelican-data:/pelican-data ghcr.io/palmeany/pelican-rust:latest
```

## Command line

The original's `php artisan p:*` commands exist with the same names and options:
`pelican --path /var/www/pelican p:user:make`, `p:user:disable2fa`, `p:node:configuration 1`, …
(`pelican --help` lists them). `pelican db:install` creates the schema in an empty database for a fresh
installation (the web installer at `/installer` does the same interactively).

## Rolling back

Stop `pelican`, re-enable `pelican-queue` and the cron entry, and point the web server back at php-fpm. Nothing
in the database needs to change; users sign in again.

## PHP plugins

Original PHP plugins run in an optional sidecar (the original panel next to pelican-rs) — see
[plugins.md](plugins.md#php-плагины-оригинала-sidecar-adr-0020).
