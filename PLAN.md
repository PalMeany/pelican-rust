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
- [x] «Нулевые» даты MySQL (`0000-00-00 00:00:00`) читаются как NULL (Q5)
- [x] (после MVP, Q1) SpacetimeDB — ADR-0021: основной бэкенд отклонён (нарушает «без миграции»), зеркало реального времени отложено решением пользователя

### M2 — Совместимость Laravel и аутентификация (`laravel-compat`, `pelican-core::auth`)
- [x] Laravel Encrypter (AES-256-CBC + HMAC, `APP_PREVIOUS_KEYS`), PHP `serialize`/`unserialize` — сверено с PHP 8.5
- [x] PHP-совместимый JSON-энкодер (`\/`, `\uXXXX`, float как zend_gcvt) — ADR-0006
- [x] bcrypt (`$2y$`), needs_rehash
- [x] API-ключи: bearer-парсинг Sanctum, `allowed_ips` (IPTools), `last_used_at` (dirty-check), expiry, тип ключа
- [x] TOTP (google2fa, окно 8, защита от повтора), recovery-коды (bcrypt в encrypted:array) — в pelican-web
- [x] Права: spatie roles/permissions, root admin, node restrictions (AuthUser); сабюзеры/политики — M4
- [x] Сессии/логин/троттлинг (для UI) — хранение в таблице `sessions` (ADR-0013)
- [x] Обработчик ошибок в формате оригинала (pretty/compact по типу, redirect без Accept: json)
- [x] Валидация с сообщениями Laravel (`lang/en/validation.php`) + TrimStrings/ConvertEmptyStringsToNull
- [x] Диф-тесты auth/*: 401 (неверный/просроченный/битый токен), 403 (app-ключ на client, IP), 404 маршрута
- [x] Application API ACL (биты ключа, root+account key) — с первыми маршрутами M5
- [x] Логин/2FA/сессии/CSRF — в pelican-web (M6): сессии, CSRF, логин, TOTP/recovery/email-код, passkeys

### M3 — Remote API для Wings (`/api/remote/*`) и клиент Wings
- [x] DaemonAuthenticate (порядок ошибок: binding 404 → 401 → 400 → 404 → 403), 405 до auth
- [x] `GET /servers` (пагинатор Laravel, переиндексация), `GET /servers/{uuid}` (конфигурация побайтно), `POST /servers/reset`
- [x] install GET/POST (+Filament-уведомления), transfer success/failure, container status
- [x] `POST /sftp/auth` (пароль, ключ OpenSSH, троттлинг, права, конфликты состояния)
- [x] `POST /activity`
- [x] backups: status, restore (wings-адаптер)
- [x] backups S3: собственный SigV4 (`pelican_api::s3`, подпись сверена с примером AWS): R11 CreateMultipartUpload + presigned UploadPart, R12 Complete/Abort/ListParts, presigned GET (скачивание, восстановление), DeleteObject; E2E с настоящим Wings и S3-сервером rclone (`scripts/e2e/s3-up`; MinIO-образы больше не публикуются). Wings блокирует восстановление с приватных адресов — для e2e `backups.restore_host_allowlist: [minio]`
  - замечание: при QUEUE_CONNECTION=sync вебхук на ActivityLogged доставляется внутри транзакции восстановления (как в оригинале), и R13 может прийти раньше коммита — статус restoring_backup остаётся; с очередью database гонки нет
- [x] `Server::saved` → удаление сабюзера-владельца (SubuserDeletionService)
- [x] Клиент Wings (семантика throwIf/User-Agent) + JWT (побайтно = PHP lcobucci); остальные вызовы — по мере эндпоинтов M4/M5
- [x] Конфигурация ноды: JSON (`/nodes/{id}/configuration`), YAML — вкладка ноды в админке (порт Symfony Yaml::dump)
- [x] Диф-тесты Remote API на 4 СУБД (remote*.yaml, 9 сценариев)
- [x] Проверка с настоящим Wings 1.0.0-rc.1 (E2E, см. M6)

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
- [x] plugins (M7): list/view/import file+url/hub/install/update/uninstall/enable/disable, диф-тесты app/plugins-*
- [x] Вебхуки: доставка (ProcessWebhook), диспатч по активности (server scope, ActivityLogged)
- [x] Вебхуки на события моделей `eloquent.created/updated/deleted:*` (ADR-0019, M7)
- [x] Фоновые задачи: prune activity/webhooks, prune/clean backups, prune images, почта (уведомления)
- [x] egg check-updates/update-index (планировщик + UI яиц)
- [x] Диф-тесты на каждый маршрут (app/*), 4 СУБД

### M6 — UI (паритет функций)
Ветка `m6` (`.worktrees/m6`). Объём оригинала — ~20 тыс. строк Filament (Admin/App/Server-панели).
Порядок: каркас и эталонные страницы каждого вида пишу сам, остальные страницы — воркерам по одной.
- [x] Каркас (ADR-0013): крейт pelican-web
  - [x] сессии в `sessions` (base64(JSON), ротация id при входе, «запомнить меня», GC-лотерея 2%)
  - [x] CSRF (`_token` / `X-CSRF-TOKEN`, 419), заголовки безопасности (SetSecurityHeaders)
  - [x] шаблоны minijinja, встроенные build.rs, оверрайд `PELICAN_VIEWS_PATH`; фильтры bytes/datetime/ago
  - [x] переводы: дамп всего `lang/*` + неймспейсов Filament (32 локали, `scripts/i18n/dump-lang`), trans/trans_choice (порт MessageSelector)
  - [x] ассеты с хешем в URL: CSS (палитра Filament, тёмная тема), app.js (тосты, модалки, подтверждения, тема), htmx 2.0.4, иконки Tabler (используемые оригиналом)
  - [x] маршрутизация в бинаре: `/api/*` → API, остальное → web; dev-окружение (SQLite + `difftest --seed`)
- [x] Ресурсный фреймворк: таблица (колонки/фильтры/сортировка/пагинация, действия строк, опрос через htmx), формы (поля+валидация Laravel, TagsInput, KeyValue), модалки/подтверждения, вкладки (`?tab=`); руководство `docs/ui-guide.md`
- [x] Массовые действия таблиц: каркас (чекбоксы, «выбрать все», панель действий, частичные уведомления, `checkIfRecordIsSelectableUsing`); mounts, users, roles, database hosts, eggs (delete/update), webhooks (admin и сервера), логи (zip/удаление), аллокации нод и серверов
- [x] Auth: логин (email/username, троттлинг 5/мин, auth:success/auth:fail), MFA-челлендж (TOTP google2fa окно 8 + защита от повтора, recovery-коды bcrypt, email-код 4 мин), сброс пароля (брокер Laravel, подписанная ссылка), OAuth (11 драйверов Socialite, вход/привязка/создание пользователей), капча Turnstile, passkeys (ADR-0017, кросс-проверка с webauthn-lib) — готово
- [x] Профиль: аккаунт, 2FA (TOTP с QR, recovery-коды, email-коды; настройка/отключение/перевыпуск), API-ключи, SSH-ключи, активити, кастомизация
- [x] Профиль: OAuth-привязки, аватары (Gravatar/UI Avatars/загрузка)
- [x] Профиль: passkeys (регистрация/удаление, вход; UI только по https, как в оригинале)
- [x] RequireTwoFactorAuthentication (уровни none/admin/all)
- [x] Admin: каркас (доступ isAdmin, сайдбар с группами и бейджами по правам spatie)
  - [x] mounts (эталон)
  - [x] database hosts, webhooks, api keys (воркер ui-a, ревью + доработки)
  - [x] users, roles (воркер ui-b)
  - [x] backup hosts (схемы wings/s3 с конфигурацией, привязка нод, вкладка бэкапов; воркер B3)
  - [x] servers: список (группировка, inline основная аллокация), редактирование (5 вкладок, переменные, маунты, синхронизация с Wings), действия, создание (htmx-перерисовка по ноде/яйцу, откат при недоступном Wings), отношения allocations/databases
  - [x] servers: перенос на другую ноду (TransferServerService, выбор бэкапов Wings)
  - [x] servers: inline-создание владельца/аллокации в мастере (модалки createOption, воркер B4; новый владелец выбирается сразу — в оригинале поле очищается)
  - [x] nodes: список (здоровье, доступность из браузера), создание, вкладки (обзор с графиками, настройки, YAML-конфиг, авто-деплой, сброс токена, диагностика), аллокации, серверы
  - [x] eggs: список (теги, число серверов), импорт (файлы/URL), 4 вкладки формы, CopyFrom, повторитель переменных, экспорт JSON/YAML, обновление по update_url, копия, удаление
  - [x] eggs: вкладка импорта из индекса GitHub (`eggs.index`, фоновая установка), «Обновить» по флагу ежедневной проверки обновлений
  - [x] dashboard (виджеты версии/разработчика/первой ноды/поддержки/помощи), health (проверки как в оригинале, раз в 5 мин планировщиком, бейдж в меню), logs (файлы Laravel + просмотрщик, ADR-0015)
  - [x] перечитывание `.env` на лету для страницы настроек (ADR-0014)
  - [x] settings (все вкладки, запись .env + перечитывание на лету)
  - [x] plugins (M7): список, импорт, действия с хуками, readme, порядок загрузки, настройки плагина
- [x] App: список серверов (вкладки my/other/all, таблица/сетка по кастомизации, ресурсы из Wings с кешем 15 с, power-действия, фильтры egg/owner, per_page в кастомизации)
- [x] Server: каркас (доступ к тенанту, права сабюзера, навигация, баннер конфликта)
  - [x] консоль (xterm.js 5.5 + WebSocket Wings, power через set state, история команд, графики)
  - [x] файлы (список, массовые действия, права, архивы, загрузка напрямую в Wings, поиск, редактор CodeMirror 5)
  - [x] databases, network, activity (воркер ui-c); блокировка страниц в конфликтном состоянии (BlockAccessInConflict)
  - [x] users (сабюзеры, воркер)
  - [x] startup, settings, backups (воркеры)
  - [x] mounts, schedules (воркеры; расписания проверены с настоящим Wings)
  - [x] webhooks сервера (`/server/{id}/webhook`, как slug оригинала; воркер B3)
- [x] Инсталлятор (ADR-0016): режим без БД при `APP_INSTALLED=false`, 7 шагов как в оригинале, запись `.env` по шагам, проверка БД/Redis, схема + Root Admin + админ, яйца из индекса, автологин и передача слушателя панели без рестарта
- [x] E2E с настоящим Wings 1.0.0-rc.1 в VM (`scripts/e2e/wings-up`): нода → Wings берёт конфиг и серверы через Remote API, создание сервера, установка (скрипт/статус через Remote API), консоль (WS + JWT, команды, статистика), power start/stop, файлы (список, правка, загрузка в Wings), создание бэкапа
- [x] E2E: расписания (run now → команда в Wings)
- [x] E2E: бэкапы (создание, скачивание по подписанной ссылке, восстановление), SFTP (пароль, ключ, отказ), перенос между двумя Wings туда и обратно с бэкапом (`scripts/e2e/wings-up <cfg> wings2 /var/lib/pelican2`)
  - найдено и исправлено: уведомление Wings о готовом бэкапе приходило до коммита → 404 → Wings удалял архив; R12 теперь ждёт появления записи до 2 с

### M7 — Плагины (WASM) — ветка `m7` (`.worktrees/m7`), ADR-0018
- [x] Операторская часть как в оригинале (крейт pelican-plugins): `plugins/<id>/plugin.json`, строки-ошибки, статусы,
  проверка panel_version (caret), безопасный импорт zip/URL (traversal, лимиты, `.import-*`, откат `.<id>.bak`,
  перенос meta), фид обновлений + sha256, Hub-ключ только своему origin, порядок загрузки, проход статусов
  loadPlugins/loadPanelPlugins на каждом запросе (PHP-классы без файла → errored)
- [x] Application API `/plugins` (диф-тесты app/plugins-* совпадают с оригиналом, включая перезаписанные plugin.json)
- [x] Админка `/admin/plugins` (воркер C4): вкладки, статусы, README, действия, импорт, порядок; настройки плагина
- [x] WIT-мир `pelican:plugin@1.0.0` (crates/pelican-plugins/wit/plugin.wit): регистрация (события, маршруты,
  страницы, навигация, типы задач, features, аватары, права, настройки) и хуки
- [x] Хост wasmtime 49: экземпляр на плагин, epoch-таймаут, лимит памяти, WASI без возможностей, KV-хранилище
  (`storage/plugins/<id>/`), HTTP по allowlist, trans с `lang/` плагина, активити; кэш компиляции; перезагрузка
  после изменений; install/uninstall-хуки
- [x] Интеграция: события активности → on-event; маршруты client/application/web; страницы admin/server/app с
  пунктами меню; настройки в админке; типы задач в форме расписаний и раннере (client API — 4 типа, как оригинал)
- [x] SDK `pelican-plugin-sdk` + пример `examples/plugins/hello` (`scripts/build-plugin`), тест рантайма, E2E в dev-панели
- [x] Аватары от плагинов (провайдер в настройках, avatar-url с кэшем 10 мин)
- [x] Права от плагинов: модели ролей (только свои префиксы), группы прав сабюзеров, ресурсы API-ключей
- [x] Server features: listeners плагинов в конфиг Wings; модалки на консоли по `feature match` (eula/java_version/gsl_token с валидацией и рестартом, pid_limit/steam_disk_space, плагинные `feature-<id>`), E2E с Wings
- [x] Возможность `api`: собственный application-ключ плагина (виден в админке), обычный конвейер Application API
- [x] События моделей `eloquent.*`: все пути записи (Application/Client/Remote API, задачи, админка, панель сервера, профиль), payload'ы сверены диф-тестом `webhooks/model-events` на 4 СУБД; доставка WASM- и PHP-плагинам
- [x] Документация для авторов плагинов (docs/plugins.md); шаблон — examples/plugins/hello

### M8 (после MVP) — PHP-sidecar для оригинальных плагинов
- [x] Дизайн (ADR-0020, Proposed): sidecar = оригинал той же версии (общие БД/APP_KEY/plugins), мост-пакет, прокси путей плагинов
- [x] Плагин-мост `pelican-rs-bridge` (PHP): manifest (маршруты, навигация, слушатели, реестры, маршруты Livewire), event, task, lifecycle; HMAC; вход пользователя без событий `Login`; расписание ядра отфильтровано
- [x] pelican-rs: прокси путей sidecar (страницы Filament + Livewire), навигация admin/server, события (eloquent, ActivityLogged), типы задач, права ролей/сабюзеров, ресурсы API-ключей, аватары, проверка версии (features PHP-плагинов — нет, см. docs/plugins.md)
- [x] Жизненный цикл PHP-плагинов через sidecar (install/update/uninstall/enable/disable, API и админка); языковые плагины — переводы из sidecar поверх встроенных
- [x] E2E с реальными плагинами из pelican-dev/plugins: tickets (миграции, ресурсы admin/server, создание тикета через Livewire), player-counter (composer-пакеты, доступ к странице по `canAccess`), robo-avatars, pirate-language — импорт и установка через pelican-rs

### M9 — полная замена оригинала (порядок согласован с пользователем 2026-10-15)
1. Слияние и проверки
   - [x] Полный диф-тест m8 на 4 СУБД (77/0/2 на каждой) → слияние m8 в main, push
   - [x] Включить пропущенные smoke-сценарии (`smoke/client-unauthenticated`, `smoke/application-unauthenticated`) — проходят на 4 СУБД
2. Функциональный паритет
   - [x] CLI как `php artisan p:*` (все команды; запущенная панель подхватывает изменения плагинов из CLI за ~5 с; индекс яиц и флаги обновлений из CLI живут только в процессе команды — кэш внутрипроцессный, ADR-0009): user (make/delete/disable2fa), node (make/list/configuration), environment
         (setup/database/mail/cache/session/queue/queue-service), redis:setup, info, server:bulk-power,
         egg (check-updates/update-index/normalize), maintenance (clean-service-backups/prune-backups/prune-images),
         plugin (list/install/update/uninstall/disable/composer/make), schedule:process; имена и опции как в оригинале
   - [x] Документация API `/docs/api` (Scramble): индекс, UI (Stoplight Elements) и JSON-спецификации application/client —
         документы оригинала (`scripts/api-docs/export.sh`), сервер = APP_URL; гостям 403 как в оригинале
3. Готовность к продакшену, скорость и нагрузка
   - [x] Упаковка: `Dockerfile` (раскладка данных как у образа оригинала), `packaging/pelican.service`, `docs/deploy.md`
         (переезд с PHP и откат), GitHub Actions: CI (fmt, сборка, тесты на SQLite) и релиз по тегу `v*` (бинарник + образ GHCR)
   - [x] Нагрузочный стенд `bench/`: одинаковые данные (1000 пользователей, 5000 серверов, 100k записей активности),
         оригинал (php-fpm + opcache, prod) и pelican-rs (release) по 2 CPU / 1 ГиБ, oha, отчёт с проверкой статуса/размера
         ответа. Гоняется на тестовом сервере пользователя (закреплённые ядра, `bench/README.md`). Базовый замер
         (`bench/results/baseline/report.md`): pelican-rs быстрее в 26–39× на одиночных объектах и страницах, но только в
         2–10× на списках по 50 (связи грузятся по одной на элемент); память 47–51 МиБ против 216–258 МиБ
   - [ ] Профилирование и оптимизация горячих путей (N+1-запросы, пул соединений, шаблоны, JSON) с повторным
         прогоном стенда и диф-тестов после каждой оптимизации
4. PHP-плагины (sidecar)
   - [x] Render hooks Filament в разметке pelican-rs (head/body/page/footer/login): только хуки плагинов, для пользователя и
         панели/сервера страницы, кэш 30 с; проверено на `snowflakes` и `legal-pages` (футер + страница /imprint)
   - [x] Расширения входа: передача входа из sidecar (подписанный заголовок ответа → сессия pelican-rs), регистрация
         (`register`: ссылка на странице входа, вход после регистрации — проверено), OAuth-провайдеры плагинов (кнопки входа/
         привязки, redirect/callback через sidecar; без реального IdP не проверено)
   - [x] Действия консоли (`Console::registerCustomHeaderActions`) и server features PHP-плагинов: кнопки на консоли
         pelican-rs (видимость по `hidden()` для пользователя/сервера), выполнение на странице-хосте моста
         (`/server/{id}/rs-console-action?action=|feature=`), listeners фич PHP-плагинов уходят в Wings; проверено на
         действии примера greeter (`mclogs-uploader` в dev-стенде скрыт: libcurl не достаёт `*.localhost` до Wings)
5. Не планируется без запроса: темы Filament для UI pelican-rs, Redis-драйверы (несколько экземпляров панели)

## Делегирование
Механика — `../glm-worker.sh` (одна задача = один модуль/файл, явные входы/выходы).
Сам: архитектура, БД-типизация, крипто/форматы, auth, Remote API, конкурентность, неочевидная
семантика Eloquent. После каждого воркера: сборка, тесты, просмотр diff.
