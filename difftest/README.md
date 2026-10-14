# Дифференциальное тестирование

`compose.yml` поднимает ОРИГИНАЛЬНУЮ панель (`pelican-orig:dev`) и базы данных внутри lima VM `pelican`.
Порты пробрасываются на хост: панель `localhost:18000`, MariaDB `13306`, MySQL `13307`, PostgreSQL `15432`.

```bash
limactl shell pelican -- sh -c 'cd /Users/tazik/Desktop/Code/claude/pelican/pelican-rs/difftest && docker compose up -d'
```

Смена СУБД оригинала: `DB_CONNECTION=pgsql DB_HOST=postgres DB_PORT=5432 docker compose up -d --force-recreate panel-orig`
(sqlite: `DB_CONNECTION=sqlite DB_DATABASE=database.sqlite`, файл — `../../.difftest/sqlite/database.sqlite`).

Mock Wings для оригинала доступен как `wings.test` (IP хоста lima `192.168.5.2`); харнесс — крейт `difftest`.
