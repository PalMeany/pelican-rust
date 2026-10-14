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

## Где остановился / что дальше
- Дождаться спек, вычитать.
- `laravel-compat`: Encrypter, PHP serialize, PHP-JSON.
- Сущности для всех 40 таблиц (по эталонной PG-схеме → выбор Bool/SmallBool).
- Mock Wings + харнесс диф-тестов + smoke-тест.
