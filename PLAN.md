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
- [ ] «Нулевые» даты MySQL (`0000-00-00 00:00:00`) читать как NULL (Q5)
- [ ] (после MVP, Q1) SpacetimeDB как отдельный бэкенд хранения — сначала ADR с оценкой

### M2 — Совместимость Laravel и аутентификация (`laravel-compat`, `pelican-core::auth`)
- [x] Laravel Encrypter (AES-256-CBC + HMAC, `APP_PREVIOUS_KEYS`), PHP `serialize`/`unserialize` — сверено с PHP 8.5
- [x] PHP-совместимый JSON-энкодер (`\/`, `\uXXXX`, float как zend_gcvt) — ADR-0006
- [x] bcrypt (`$2y$`), needs_rehash
- [x] API-ключи: bearer-парсинг Sanctum, `allowed_ips` (IPTools), `last_used_at` (dirty-check), expiry, тип ключа
- [x] TOTP (google2fa, окно 8, защита от повтора), recovery-коды (bcrypt в encrypted:array) — в pelican-web
- [~] Права: spatie roles/permissions, root admin, node restrictions (AuthUser) — сабюзеры/политики с M4
- [x] Сессии/логин/троттлинг (для UI) — хранение в таблице `sessions` (ADR-0013)
- [x] Обработчик ошибок в формате оригинала (pretty/compact по типу, redirect без Accept: json)
- [x] Валидация с сообщениями Laravel (`lang/en/validation.php`) + TrimStrings/ConvertEmptyStringsToNull
- [x] Диф-тесты auth/*: 401 (неверный/просроченный/битый токен), 403 (app-ключ на client, IP), 404 маршрута
- [x] Application API ACL (биты ключа, root+account key) — с первыми маршрутами M5
- [~] Логин/2FA/сессии/CSRF — вместе с UI (M6): сессии, CSRF, логин, TOTP/recovery/email-код готовы в pelican-web

### M3 — Remote API для Wings (`/api/remote/*`) и клиент Wings
- [x] DaemonAuthenticate (порядок ошибок: binding 404 → 401 → 400 → 404 → 403), 405 до auth
- [x] `GET /servers` (пагинатор Laravel, переиндексация), `GET /servers/{uuid}` (конфигурация побайтно), `POST /servers/reset`
- [x] install GET/POST (+Filament-уведомления), transfer success/failure, container status
- [x] `POST /sftp/auth` (пароль, ключ OpenSSH, троттлинг, права, конфликты состояния)
- [x] `POST /activity`
- [x] backups: status, restore (wings-адаптер)
- [ ] backups S3: presigned multipart (R11), complete/abort (R12) — нужен MinIO в диф-окружении
- [x] `Server::saved` → удаление сабюзера-владельца (SubuserDeletionService)
- [x] Клиент Wings (семантика throwIf/User-Agent) + JWT (побайтно = PHP lcobucci); остальные вызовы — по мере эндпоинтов M4/M5
- [ ] Конфигурация ноды (JSON/YAML) — вместе с Application API nodes (M5)
- [x] Диф-тесты Remote API на 4 СУБД (remote*.yaml, 9 сценариев)
- [ ] Ручная проверка с настоящим Wings (контейнер), см. QUESTIONS

### M4 — Client API (`/api/client/*`)
- [x] account, api-keys, ssh-keys, activity, permissions
- [x] servers: index, view, websocket, resources, command, power
- [x] databases (реальный DDL на хосте БД), files (+ upload/download URL), schedules + tasks (+ RunTaskJob, sync-очередь), allocations, subusers, backups, startup
- [x] settings (воркер)
- [x] Лимиты ресурсов (ResourceLimit), rate limiting (семантика CACHE_STORE=array), активити-лог
- [x] Диф-тесты на каждый маршрут (успех + основные ошибки), 4 СУБД
- [x] Раннер расписаний (`p:schedule:process`, тикер в serve), диф-тест через шаг `command` харнесса

### M5 — Application API (`/api/application/*`)
- [x] Каркас: ACL (биты ключа, root+account), includes с allowsAdmin, пагинация (per_page/page как в PHP), привязки моделей
- [x] users (+roles assign/remove, spatie-семантика), nodes (+configuration, deployable, порядок ключей Eloquent), allocations (CIDR)
- [x] servers: list/view/external/details/build/startup/suspend/reinstall/transfer(+cancel)/delete(+force), создание (deploy, выбор аллокации), databases
- [x] eggs: list/view/delete, экспорт JSON, импорт (legacy-форматы, зарезервированные переменные)
- [x] eggs: экспорт YAML (порт Symfony YAML Dumper — воркер, 528 эталонных векторов)
- [x] database-hosts, mounts, roles (воркеры + свои)
- [x] webhooks (воркер)
- [ ] plugins — привязать к собственной системе плагинов в M7 (оригинальные — PHP/Composer)
- [x] Вебхуки: доставка (ProcessWebhook), диспатч по активности (server scope, ActivityLogged)
- [ ] Вебхуки на события моделей `eloquent.created/updated/deleted:*`
- [x] Фоновые задачи: prune activity/webhooks, prune/clean backups, prune images, почта (уведомления)
- [ ] egg check-updates/update-index (вместе с UI яиц)
- [x] Диф-тесты на каждый маршрут (app/*), 4 СУБД

### M6 — UI (паритет функций)
Ветка `m6` (`.worktrees/m6`). Объём оригинала — ~20 тыс. строк Filament (Admin/App/Server-панели).
Порядок: каркас и эталонные страницы каждого вида пишу сам, остальные страницы — воркерам по одной.
- [~] Каркас (ADR-0013): крейт pelican-web
  - [x] сессии в `sessions` (base64(JSON), ротация id при входе, «запомнить меня», GC-лотерея 2%)
  - [x] CSRF (`_token` / `X-CSRF-TOKEN`, 419), заголовки безопасности (SetSecurityHeaders)
  - [x] шаблоны minijinja, встроенные build.rs, оверрайд `PELICAN_VIEWS_PATH`; фильтры bytes/datetime/ago
  - [x] переводы: дамп всего `lang/*` + неймспейсов Filament (32 локали, `scripts/i18n/dump-lang`), trans/trans_choice (порт MessageSelector)
  - [x] ассеты с хешем в URL: CSS (палитра Filament, тёмная тема), app.js (тосты, модалки, подтверждения, тема), htmx 2.0.4, иконки Tabler (используемые оригиналом)
  - [x] маршрутизация в бинаре: `/api/*` → API, остальное → web; dev-окружение (SQLite + `difftest --seed`)
- [x] Ресурсный фреймворк: таблица (колонки/фильтры/сортировка/пагинация, действия строк, опрос через htmx), формы (поля+валидация Laravel, TagsInput, KeyValue), модалки/подтверждения, вкладки (`?tab=`); руководство `docs/ui-guide.md`
- [ ] Массовые действия таблиц (bulk delete)
- [~] Auth: логин (email/username, троттлинг 5/мин, auth:success/auth:fail), MFA-челлендж (TOTP google2fa окно 8 + защита от повтора, recovery-коды bcrypt, email-код 4 мин), сброс пароля (брокер Laravel, подписанная ссылка) — готово; passkeys, OAuth, капча — нет
- [x] Профиль: аккаунт, 2FA (TOTP с QR, recovery-коды, email-коды; настройка/отключение/перевыпуск), API-ключи, SSH-ключи, активити, кастомизация
- [ ] Профиль: OAuth-привязки, passkeys, загрузка аватара
- [x] RequireTwoFactorAuthentication (уровни none/admin/all)
- [~] Admin: каркас (доступ isAdmin, сайдбар с группами и бейджами по правам spatie)
  - [x] mounts (эталон)
  - [x] database hosts, webhooks, api keys (воркер ui-a, ревью + доработки)
  - [x] users, roles (воркер ui-b)
  - [ ] backup hosts (воркер, очередь B2)
  - [x] servers: список (группировка, inline основная аллокация), редактирование (5 вкладок, переменные, маунты, синхронизация с Wings), действия, создание (htmx-перерисовка по ноде/яйцу, откат при недоступном Wings), отношения allocations/databases
  - [x] servers: перенос на другую ноду (TransferServerService, выбор бэкапов Wings)
  - [ ] servers: inline-создание владельца/аллокации в мастере, массовые действия
  - [x] nodes: список (здоровье, доступность из браузера), создание, вкладки (обзор с графиками, настройки, YAML-конфиг, авто-деплой, сброс токена, диагностика), аллокации, серверы
  - [x] eggs: список (теги, число серверов), импорт (файлы/URL), 4 вкладки формы, CopyFrom, повторитель переменных, экспорт JSON/YAML, обновление по update_url, копия, удаление
  - [ ] eggs: вкладка импорта из индекса GitHub (`eggs.index`), видимость «Обновить» по флагу проверки обновлений (сейчас — при наличии update_url), массовые delete/update
  - [x] dashboard (виджеты версии/разработчика/первой ноды/поддержки/помощи), health (проверки как в оригинале, раз в 5 мин планировщиком, бейдж в меню), logs (файлы Laravel + просмотрщик, ADR-0015)
  - [x] перечитывание `.env` на лету для страницы настроек (ADR-0014)
  - [ ] settings (воркер, очередь C2), plugins (после M7)
- [x] App: список серверов (вкладки my/other/all, таблица/сетка по кастомизации, ресурсы из Wings с кешем 15 с, power-действия, фильтры egg/owner, per_page в кастомизации)
- [~] Server: каркас (доступ к тенанту, права сабюзера, навигация, баннер конфликта)
  - [x] консоль (xterm.js 5.5 + WebSocket Wings, power через set state, история команд, графики)
  - [x] файлы (список, массовые действия, права, архивы, загрузка напрямую в Wings, поиск, редактор CodeMirror 5)
  - [x] databases, network, activity (воркер ui-c); блокировка страниц в конфликтном состоянии (BlockAccessInConflict)
  - [x] users (сабюзеры, воркер)
  - [ ] startup, settings, mounts (очередь C2); backups, schedules, webhooks (очередь B3)
- [ ] Инсталлятор
- [ ] E2E-проверки ключевых сценариев в браузере (нужен настоящий Wings в VM: консоль, файлы, power, установка)

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
