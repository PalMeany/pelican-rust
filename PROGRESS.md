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

- 2026-10-14 (вечер): M3 почти закрыта. Remote API R1–R13 совпадает с оригиналом на 4 СУБД
  (конфиг сервера побайтно, SFTP-auth, activity, transfers, backups wings). Клиент Wings + JWT
  (сверено с PHP lcobucci). Запись JSON как в PHP (`pelican_db::write`), запросы формы Eloquent
  (ADR-0011). Валидатор Laravel (`pelican-api/src/validation.rs`), ввод (`input.rs`).

## Где остановился / что дальше
- M3 хвосты: S3-бэкапы (MinIO в compose + presign), удаление сабюзера-владельца (`Server::saved`).
- M4 Client API: начать с каркаса серверных маршрутов (AuthenticateServerAccess, ResourceLimit,
  subject activity), затем GET /api/client (список серверов), ServerData и далее по spec client-api.md §8.
  Механические ресурсы/эндпоинты отдавать воркерам по шаблону (см. scripts/worker/*.md).

## Грабли (важно)
- Python-замены в исходниках после `cargo fmt` молча не срабатывают — всегда `assert old in s`.
- Таблицы, которые засевают миграции (`backup_hosts` «Local»), сбрасываются харнессом — добавлять в фикстуры.
- В PG нельзя биндить text в json-колонку: только через `pelican_db::write` (CAST).
