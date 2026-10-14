#!/usr/bin/env bash
# Regenerate crates/pelican-db/schema/* from the ORIGINAL panel: boots
# pelican-orig:dev against each database (it runs all migrations on start),
# dumps the resulting schema and sanitizes it. Requires the lima VM "pelican"
# and the difftest compose stack (see difftest/README.md).
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DATA="$(cd "$REPO/.." && pwd)/.difftest"
RAW="$DATA/schema"
mkdir -p "$RAW" "$DATA/sqlite"
vm() { limactl shell pelican -- sh -c "cd '$REPO/difftest' && $1"; }

boot() { # $1 = env assignments
  vm "$1 docker compose up -d --force-recreate panel-orig >/dev/null 2>&1"
  for _ in $(seq 1 60); do
    vm "docker compose logs panel-orig 2>&1 | grep -q 'Starting Supervisord'" && return 0
    sleep 2
  done
  echo "panel-orig did not start" >&2; exit 1
}

vm "docker compose up -d mariadb mysql postgres >/dev/null 2>&1"
vm "docker compose exec -T mariadb mariadb -uroot -proot -e 'DROP DATABASE IF EXISTS panel; CREATE DATABASE panel; GRANT ALL ON panel.* TO pelican'"
boot "DB_CONNECTION=mariadb DB_HOST=mariadb"
vm "docker compose exec -T mariadb mariadb-dump -uroot -proot --no-data --skip-comments --skip-dump-date --skip-add-drop-table panel" > "$RAW/mariadb.schema.sql"

vm "docker compose exec -T mysql mysql -uroot -proot -e 'DROP DATABASE IF EXISTS panel; CREATE DATABASE panel; GRANT ALL ON panel.* TO pelican' 2>/dev/null"
boot "DB_CONNECTION=mysql DB_HOST=mysql"
vm "docker compose exec -T mysql mysqldump -uroot -proot --no-data --skip-comments --skip-dump-date --skip-add-drop-table panel 2>/dev/null" > "$RAW/mysql.schema.sql"

vm "docker compose exec -T postgres psql -U pelican -d postgres -c 'DROP DATABASE IF EXISTS panel' -c 'CREATE DATABASE panel'"
boot "DB_CONNECTION=pgsql DB_HOST=postgres DB_PORT=5432"
vm "docker compose exec -T postgres pg_dump -U pelican --schema-only --no-owner --no-privileges panel" > "$RAW/pgsql.schema.sql"

rm -f "$DATA/sqlite/database.sqlite"
boot "DB_CONNECTION=sqlite DB_DATABASE=database.sqlite"
sqlite3 "$DATA/sqlite/database.sqlite" .schema > "$RAW/sqlite.schema.sql"
sqlite3 "$DATA/sqlite/database.sqlite" "select migration from migrations order by id" > "$RAW/migrations.txt"

python3 "$REPO/scripts/schema/sanitize.py" "$RAW" "$REPO/crates/pelican-db/schema"
python3 "$REPO/scripts/schema/column_order.py" "$REPO/crates/pelican-db/schema" "$REPO/crates/pelican-db/src/column_order.rs"
