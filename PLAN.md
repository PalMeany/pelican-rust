# PLAN — Pelican panel → Rust (`pelican-rs`)

Исходник: `../panel` (Laravel 13 / Filament 5, только чтение). Wings не трогаем.
Состояние работы — `PROGRESS.md`, вопросы — `QUESTIONS.md`, решения — `docs/decisions/`,
контракты оригинала — `docs/spec/`.

## Критерии приёмки (из задачи)

1. Работает на существующей БД Pelican без миграции данных: MySQL/MariaDB, PostgreSQL, SQLite (все три — Q1).
2. Читает Laravel-шифрованные поля с тем же `APP_KEY`; bcrypt-пароли и 2FA работают.
3. Remote API для Wings совместим байт-в-байт.
4. Application/Client API совместимы: пути, форматы, коды ошибок, пагинация.
5. Eggs импорт/экспорт в оригинальном формате.
6. UI — функциональный паритет (не пиксельный).
7. Плагины: своя WASM-система с API, покрывающим хуки оригинала; PHP-sidecar — после MVP.

**Definition of Done для фичи:** реализована + дифференциальный тест (оригинал vs Rust: ответ API,
изменения БД, исходящие вызовы к Wings) зелёный минимум на одной СУБД, на всех трёх — для вех ≥ M2.
После каждой вехи: `cargo build` + `cargo test` зелёные, коммит.

## Вехи

Порядок по зависимостям: инфраструктура → схема БД → auth → Remote API → Client API →
Application API → UI → плагины → PHP-sidecar.

### M0 — Инфраструктура и спецификации
- [x] Docker-окружение (lima VM `pelican`), образ оригинала `pelican-orig:dev` из `../panel/Dockerfile.dev`
- [x] `difftest/compose.yml`: оригинал + MariaDB + PostgreSQL (+ SQLite через общий каталог)
- [x] Эталонные схемы всех трёх СУБД сняты с оригинала
- [x] Спеки `docs/spec/*.md` (remote/client/application API, auth-crypto, models-services, eggs, background, plugins, ui)
- [x] ADR 0001–0010
- [x] Cargo workspace, сборка/тесты в Linux-контейнере (`scripts/cargo`)
- [x] `CLAUDE.md` с конвенциями для воркеров
- [x] Mock Wings (запись входящих запросов, заготовленные ответы)
- [x] Харнесс диф-тестов: сброс БД + фикстуры, сценарии (YAML), нормализация, сравнение ответа + изменений БД + вызовов Wings; `--self-check`
- [x] Первые диф-тесты зелёные на 4 СУБД (auth/*)

### M1 — Схема БД и слой данных (`pelican-db`)
- [x] Подключение к MySQL/MariaDB, PostgreSQL, SQLite по `DB_*` из `.env` (как в `config/database.php`)
- [x] Гибкие типы колонок (bool/int/json/timestamp расходятся между СУБД) — ADR-0004
- [x] Сущности SeaORM для всех 42 таблиц (+ порядок колонок по СУБД `column_order.rs`)
- [x] Установка схемы «с нуля» из эталонных дампов + заполнение `migrations` (`pelican db:install`)
- [x] Проверка схемы при старте: все 255 миграций оригинала применены, иначе понятная ошибка
- [x] Тесты: запись/чтение и SELECT всех сущностей на 4 СУБД; БД оригинала читается (диф-тесты)

### M2 — Совместимость Laravel и аутентификация (`laravel-compat`, `pelican-core::auth`)
- [x] Laravel Encrypter (AES-256-CBC + HMAC, `APP_PREVIOUS_KEYS`), PHP `serialize`/`unserialize` — сверено с PHP 8.5
- [x] PHP-совместимый JSON-энкодер (`\/`, `\uXXXX`, float как zend_gcvt) — ADR-0006
- [x] bcrypt (`$2y$`), needs_rehash
- [x] API-ключи: bearer-парсинг Sanctum, `allowed_ips` (IPTools), `last_used_at` (dirty-check), expiry, тип ключа
- [ ] TOTP (секрет, окно, recovery-коды), старые форматы 2FA
- [~] Права: spatie roles/permissions, root admin, node restrictions (AuthUser) — сабюзеры/политики с M4
- [ ] Сессии/логин/троттлинг (для UI) — хранение в таблице `sessions` (ADR)
- [x] Обработчик ошибок в формате оригинала (pretty/compact по типу, redirect без Accept: json)
- [ ] Валидация с сообщениями Laravel (`lang/en/validation.php`) + TrimStrings/ConvertEmptyStringsToNull
- [x] Диф-тесты auth/*: 401 (неверный/просроченный/битый токен), 403 (app-ключ на client, IP), 404 маршрута
- [ ] Application API ACL (биты ключа, root+account key) — с первыми маршрутами M5
- [ ] Логин/2FA/сессии/CSRF — вместе с UI (M6)

### M3 — Remote API для Wings (`/api/remote/*`) и клиент Wings
- [ ] DaemonAuthenticate
- [ ] `GET /servers`, `GET /servers/{uuid}` (полная конфигурация сервера), `POST /servers/reset`
- [ ] install GET/POST, transfer success/failure, container status
- [ ] `POST /sftp/auth` (пароль, ключ, троттлинг)
- [ ] `POST /activity`
- [ ] backups: remote upload (S3 multipart), status, restore
- [ ] Клиент Wings (DaemonRepository-аналоги) + JWT (websocket, download, upload, backup)
- [ ] Конфигурация ноды (YAML)
- [ ] Диф-тесты байт-в-байт для каждого эндпоинта на трёх СУБД
- [ ] Ручная проверка с настоящим Wings (контейнер), см. QUESTIONS

### M4 — Client API (`/api/client/*`)
- [ ] account, api-keys, ssh-keys, activity, permissions
- [ ] servers: index, view, websocket, resources, command, power
- [ ] databases, files (+ upload/download URL), schedules + tasks, allocations, subusers, backups, startup, settings
- [ ] Лимиты ресурсов (ResourceLimit), rate limiting, активити-лог
- [ ] Диф-тесты на каждый маршрут (успех + основные ошибки)

### M5 — Application API (`/api/application/*`)
- [ ] users, nodes (+configuration, deployable), allocations, servers (+details/build/startup/suspend/reinstall/transfer/delete), databases
- [ ] eggs (view/export/import/delete) — формат eggs (ADR, спека eggs.md)
- [ ] database-hosts, mounts, roles, webhooks, plugins
- [ ] Фоновые задачи: планировщик расписаний, вебхуки, очистка (spec background.md)
- [ ] Диф-тесты на каждый маршрут

### M6 — UI (паритет функций)
- [ ] Каркас: SSR (minijinja) + htmx, декларативные ресурсы (таблица/форма/действия) — ADR-0005
- [ ] Auth: логин, 2FA, passkeys, OAuth, сброс пароля, профиль
- [ ] Admin: dashboard, nodes, servers, eggs, users, roles, db hosts, mounts, webhooks, api keys, settings, health, plugins
- [ ] App: список серверов
- [ ] Server: консоль (xterm.js + websocket Wings), файлы (CodeMirror), БД, расписания, сабюзеры, бэкапы, сеть, startup, настройки, активити
- [ ] Инсталлятор
- [ ] E2E-проверки ключевых сценариев в браузере

### M7 — Плагины (WASM)
- [ ] WIT-API, покрывающий хуки оригинала (spec plugins.md): события, расширения (avatar, captcha, oauth, backup, tasks, features), UI-регистрация (ресурсы/страницы/виджеты/навигация), роуты, настройки, права, переводы
- [ ] Хост на wasmtime (component model), изоляция, лимиты
- [ ] Жизненный цикл: import/install/enable/disable/update/uninstall + Application API `/plugins`
- [ ] SDK для авторов плагинов + пример плагина

### M8 (после MVP) — PHP-sidecar для оригинальных плагинов
- [ ] Дизайн (ADR): процесс PHP с урезанным Laravel, мост событий/рендеринга

## Делегирование
Механика — `../glm-worker.sh` (одна задача = один модуль/файл, явные входы/выходы).
Сам: архитектура, БД-типизация, крипто/форматы, auth, Remote API, конкурентность, неочевидная
семантика Eloquent. После каждого воркера: сборка, тесты, просмотр diff.
