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

- 2026-10-14 (ночь): M4 почти закрыта. Client API: account/keys/ssh/activity, servers, subusers,
  backups, startup, files (Wings-прокси, подписанные URL, правило `url` = Str::isUrl), databases
  (реальный DDL на хосте MariaDB из difftest-сети), network (воркер), schedules+tasks (RunTaskJob,
  sync-очередь как SyncQueue, ADR-0012), cron-php (воркер, 1840/1840 векторов PHP). Всё зелёное на 4 СУБД.

- 2026-10-14 (поздний вечер): M5 в основном готова (ветка `m5`, worktree `.worktrees/m5`): Application API
  users/roles/nodes(+allocations, configuration, deployable)/servers(все операции, создание с deploy)/
  database-hosts/mounts/eggs (импорт + экспорт JSON/YAML побайтно: порт Symfony YAML Dumper, 528 векторов).
  Фон: планировщик с задачами обслуживания, почта (lettre), хук Server::saved. Харнесс: шаг `command`
  (artisan через Docker API), PG без autovacuum, `distinct_seconds`.

- 2026-10-14 (ночь, поздно): M6 старт. ADR-0013 (сессии/CSRF/шаблоны/ассеты/ресурсы). Крейт pelican-web:
  сессии в таблице `sessions`, CSRF, minijinja со встраиванием, дамп переводов оригинала (32 локали вместе
  с Filament), htmx и иконки Tabler вендорены, вход с MFA (TOTP/recovery/email).

- 2026-10-15: M5 слита в main (полный прогон 73×4 СУБД зелёный). M6 в `.worktrees/m6`: ресурсный фреймворк
  (таблицы, формы, TagsInput/KeyValue, вкладки), профиль с 2FA, app-список серверов, admin: mounts, servers
  (+создание, аллокации/БД), nodes (графики, YAML-конфиг, деплой), database hosts, eggs, webhooks, api keys;
  server: консоль (xterm+WS), файлы, базы данных. Воркеры GLM: ui-a готов, ui-b (users/roles/backup hosts) и
  ui-c (network/activity/startup/settings/mounts) в работе.

- 2026-10-15 (ночь): E2E с настоящим Wings (v1.0.0-rc.1, бинарник в alpine-контейнере, rootless Docker в VM):
  установка, консоль, питание, файлы, бэкап — работают с Rust-панелью без доработок. Наблюдение: уведомление
  Wings о готовом бэкапе может прийти раньше коммита транзакции InitiateBackupService (так же в оригинале → 404,
  бэкап «висит»); оставлено как в оригинале.

## Где остановился / что дальше
- Хвосты M5: вебхуки на события моделей eloquent.*, egg check-updates/update-index (с UI), plugins API (M7),
  S3-бэкапы (MinIO), конфигурация ноды YAML через API.
- M6 (`.worktrees/m6`, dev-панель — контейнер `pelican-m6-dev` на :8088, SQLite из `.worktrees/.difftest/m6dev`):
  готовы admin dashboard/settings/health/logs, перенос сервера, server users/backups/schedules, сброс пароля,
  OAuth/капча/аватары, веб-инсталлятор (ADR-0016). Интегрированы B3 (webhooks сервера, backup hosts) и C3
  (массовые действия), passkeys (ADR-0017). Inline-создание владельца/аллокации в мастере. Дальше — E2E бэкапов/переноса/SFTP,
  слияние m6 в main.
- Запись в SQLite dev-окружения с хоста не видна контейнеру (кэш ФС VM) — менять данные только через панель.

## Диск (просьба пользователя)
На хосте мало места (~15–30 ГБ свободно). Сборки живут в docker-томах VM (`pelican-rs-target*`).
Кэш НЕ беречь: после каждой вехи/пачки воркеров — `scripts/clean.sh` (cargo clean в target-томах,
удаление томов/окружений удалённых worktree, prune кэша docker) и `limactl shell pelican -- sudo fstrim -av`
(иначе образ диска VM на хосте не уменьшится). Профиль dev/test без debug-инфо и incremental.
Проверка: `df -h /Users/tazik`, `du -sh ~/.lima/pelican`. Пользователь предлагал отдельный сервер для сборок.

## Грабли (важно)
- Каждый checkout (main, `.worktrees/<name>`) обязан иметь свой target-том: cargo хеширует path-крейты
  относительно корня воркспейса, и общий target приводит к перезаписи rlib чужой версией
  (`scripts/cargo` выбирает том по пути сам). Воркеры запускаются через `env -i` — env-переменные не доходят.
- Даты «ближайшего запуска» cron в сценариях держать дальше 3 дней от «сейчас»: нормализатор
  записывает близкие времена относительно старта прогона, а панели стартуют в разное время.
- Оригинал в difftest работает с CACHE_STORE=array и QUEUE_CONNECTION=sync — у Rust-панели те же
  значения (`rust_target.rs`), иначе троттлинг и очередь расходятся.
- Python-замены в исходниках после `cargo fmt` молча не срабатывают — всегда `assert old in s`.
- Таблицы, которые засевают миграции (`backup_hosts` «Local»), сбрасываются харнессом — добавлять в фикстуры.
- В PG нельзя биндить text в json-колонку: только через `pelican_db::write` (CAST).
