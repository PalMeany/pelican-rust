#!/usr/bin/env bash
# Re-export the original's OpenAPI documents (Scramble) for /docs/api after an upstream
# update: runs `php artisan scramble:export` in a running original-panel container
# (image pelican-orig:dev) and stores them with an `__APP_URL__` placeholder.
#   scripts/api-docs/export.sh [container]      (default: pelican-sidecar)
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
C="${1:-pelican-sidecar}"
OUT="$REPO/crates/pelican-web/data/api-docs"
URL="$(limactl shell pelican -- docker exec "$C" php -r 'echo getenv("APP_URL");')"
for api in application client; do
  limactl shell pelican -- docker exec "$C" php artisan scramble:export --api="$api" --stdout \
    | sed "s#${URL%/}#__APP_URL__#g" > "$OUT/$api.json"
  echo "$api: $(wc -c < "$OUT/$api.json") bytes"
done
