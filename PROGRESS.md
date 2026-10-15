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
  бэкап «висит»); у Rust-панели гонка проявлялась почти всегда (ответ за ~1 мс) и Wings удалял архив —
  R12 теперь ждёт появления записи до 2 с.

- 2026-10-15 (утро): M6 слита в main (полный диф-прогон 73×4 зелёный). M7 в `.worktrees/m7`: операторская часть
  плагинов как в оригинале (диф-тесты совпадают), WASM-рантайм на wasmtime 49 с WIT-миром, SDK и примером плагина;
  плагин из zip импортируется, ставится, получает события, отдаёт маршруты, страницы, настройки, задачи расписаний.
  Dev-панель теперь `pelican-m7-dev` (тот же порт и данные).

- 2026-10-15 (день): прод пользователя `panel.nuvira.su` переведён на pelican-rs (PLAN.md, «Переезд прода»).
- 2026-10-15 (вечер): вид как у оригинала (темы плагинов, режим topbar, глобальный поиск, страницы ошибок, вкладки
  и мастера Filament, ToggleButtons, KeyValue, иконки полей, загрузка иконок сервера/яйца, сворачивание меню,
  графики Chart.js) и скорость (кэш виджетов дашборда, OPcache 512 МБ на проде, кэши `filament:optimize` в sidecar).
  Полный диф-тест 316/316; `main` = `prod`. Всё выложено на прод.

## Где остановился / что дальше
- Репозиторий: https://github.com/PalMeany/pelican-rust (публичный, AGPL-3.0-only как оригинал; автор коммитов —
  PalMeany, noreply-адрес; `gh` переключён на PalMeany). Пушить `main` и рабочие ветки; оригинал не трогать.
- `main` = M1–M9 (полный диф-тест 4 СУБД: 79/79 на каждой). M9: CLI `p:*`, `/docs/api`, упаковка/CI/релиз,
  нагрузочный стенд и оптимизация (`bench/results/README.md`), render hooks/входы/действия консоли/настройки
  PHP-плагинов, вход в API по сессии браузера. Не запланировано без запроса: темы, Redis, SpacetimeDB (ADR-0021).
- Тестовый сервер пользователя `root@195.3.221.43` (ключ `~/.ssh/id_ed25519`, разрешено пользователем): Ryzen 5950X
  32 потока, 125 ГБ. НА НЁМ ЖИВЫЕ WINGS И ИГРОВЫЕ СЕРВЕРЫ — ничего чужого не трогать, порты не публиковать, всё
  своё — в `/opt/pelican-rs-bench` и compose-проектах `pelican-bench*`, CPU только 12–15,28–31 (`PELICAN_CPUSET`,
  `BENCH_CPUSET_*`, buildx-билдер `pelican-rs-builder` с тем же cpuset; см. bench/README.md). Образ оригинала там
  собирается из `../panel` (amd64; образ из VM — arm64 и не годится).
- CLI-тесты: копии dev-данных `.worktrees/<wt>/.cli-test` — `DB_DATABASE` в их `.env` указывать на свою копию
  (по умолчанию там абсолютный путь к базе dev-панели — однажды записали туда тестовые ноды).
- E2E-окружение: контейнер `pelican-sidecar` (образ `pelican-orig:dev`, сеть `pelican-e2e`), dev-панель
  `pelican-m8-dev` с `PANEL_PHP_SIDECAR=http://pelican-sidecar`, данные `.worktrees/.difftest/m6dev`.
- ADR-0021 (SpacetimeDB) — отложено решением пользователя.
- Прод пользователя: `root@77.239.97.118` (тот же ключ), `panel.nuvira.su` на pelican-rs с 2026-10-15. Данные —
  `/var/www/pelican` (SQLite), бинарник `/usr/local/bin/pelican` (собирать под glibc 2.35: `shadow/build-jammy.sh` на
  тестовом сервере), `pelican.service`, sidecar `/var/www/pelican-sidecar` (Caddy 127.0.0.1:8090), скрипты и откат —
  `/root/pelican-rs-deploy/deploy.sh`, бэкап — `/root/pelican-backup-20261015`. Обновление: собрать, скопировать бинарник,
  `systemctl restart pelican`. Копия прода для проверок — `/opt/pelican-rs-bench/shadow` на тестовом сервере (`compare.py`).
  Пока на тестовом сервере идёт диф-тест (он собирает `/opt/pelican-rs-bench/pelican-rs`), dev-сборки для копии —
  из отдельной копии исходников `pelican-rs-ui` (`shadow/build-ui.sh`, свой target-том). На проде добавлен
  `/etc/php/8.5/fpm/conf.d/99-pelican-opcache.ini` (OPcache 512 МБ).

## Диск (просьба пользователя)
На хосте мало места (~15–30 ГБ свободно). Сборки живут в docker-томах VM (`pelican-rs-target*`).
Кэш НЕ беречь: после каждой вехи/пачки воркеров — `scripts/clean.sh` (cargo clean в target-томах,
удаление томов/окружений удалённых worktree, prune кэша docker) и `limactl shell pelican -- sudo fstrim -av`
(иначе образ диска VM на хосте не уменьшится). Профиль dev/test без debug-инфо и incremental.
Проверка: `df -h /Users/tazik`, `du -sh ~/.lima/pelican`. Тяжёлое (бенчмарки, при нехватке места — сборки) — на сервере (см. выше).

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
- zsh: переменная `path` в функциях — это `$PATH`; в shell-хелперах не называть так локальные переменные.
- Livewire повторяет middleware страницы на «поддельном» запросе (метод и путь исходной страницы) —
  подписи прокси не должны зависеть от метода/пути (ADR-0020).
- События моделей: payload'ы — физический порядок колонок СУБД и загруженные связи (`$with`, а также
  подгруженные кодом оригинала до записи) — проверять диф-тестом, а не по коду.
