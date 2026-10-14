# PROGRESS

Журнал для продолжения работы в новой сессии. Сначала прочитать: этот файл → `PLAN.md` →
`docs/decisions/` → нужные `docs/spec/*.md`.

## Окружение (как поднять с нуля)
- lima VM `pelican` (docker rootless): `limactl start pelican`. Конфиг: `~/.lima/pelican/lima.yaml`
  (rw-маунты `.difftest` и `pelican-rs`). Docker-команды: `limactl shell pelican -- docker …`
  или `LIMA_INSTANCE=pelican docker.lima …`.
- Образ оригинала: `limactl shell pelican -- sh -c 'cd <repo>/../panel && docker build -f Dockerfile.dev -t pelican-orig:dev .'`
- Dev-образ Rust: `scripts/dev-image/Dockerfile` → `pelican-rs-dev`.
- Сборка/тесты: `scripts/cargo build|test|clippy …` (в контейнере, в сети диф-тестов).
- Диф-окружение: `limactl shell pelican -- sh -c 'cd <repo>/difftest && docker compose up -d'`
  (`DB_CONNECTION=mariadb|mysql|pgsql|sqlite` выбирает БД для оригинала; для sqlite ещё `DB_DATABASE=database.sqlite`).
- Справочно: `../.ref/vendor` — vendor оригинала (Laravel и пр.) извлечён из образа.
- Эталонные дампы схем: `../.difftest/schema/*` (сырые), `crates/pelican-db/schema/*` (очищенные).
  Перегенерация: `scripts/schema/refresh.sh`.

## Сделано
- 2026-10-14: M0 частично — VM, образ оригинала, compose (MariaDB/MySQL/PG/SQLite), дампы схем;
  ADR 0001–0010; workspace; `pelican-db`: подключение (семантика Laravel), гибкие типы
  (Int/Bool/SmallBool/Json/Timestamp), установка схемы с нуля, проверка миграций, сущность `nodes`;
  тест на 4 СУБД зелёный. Спеки `docs/spec/*` пишутся агентами.

- 2026-10-14: спеки готовы (9 шт., ~12k строк). `laravel-compat` (Encrypter, serialize, bcrypt,
  json_encode) сверен с PHP 8.5 из контейнера оригинала. Все 42 сущности (GLM-воркеры, ревью).
  Харнесс диф-тестов + mock Wings (`scripts/difftest --backend all`). Каркас: `pelican-core`
  (env/config как Laravel, лимитер, client IP/TrustProxies, IpRange, auth/AuthUser, activity),
  `pelican-api` (ошибки, PhpJson, конвейер middleware, bindings), бинарь `pelican`.
  Диф-тесты auth/* зелёные на MariaDB, MySQL, PostgreSQL, SQLite.

## Как запускать диф-тесты
`scripts/difftest --backend mariadb|mysql|pgsql|sqlite|all [--filter <name>] [--show] [--self-check]`.
Сценарии `difftest/scenarios/*.yaml`, фикстуры `difftest/fixtures/*.yaml` (токены ключей — в шапке base.yaml).
`--self-check` сравнивает оригинал сам с собой (проверка нормализации).

## Где остановился / что дальше
- M2 хвосты: валидация (сообщения Laravel, TrimStrings/ConvertEmptyStringsToNull).
- M3: Remote API (spec remote-api.md): DaemonAuthenticate уже есть; GET /servers и /servers/{uuid}
  (конфигурация сервера), install, sftp/auth, activity, transfer, backups; клиент Wings + JWT.
  Нужна фикстура `infra` (нода wings.test:8080, egg, сервер, аллокации).
