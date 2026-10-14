# 0001. Структура workspace и слои

**Статус:** Accepted

**Контекст.** Нужны минимум backend/frontend; работу делим между оркестратором и воркерами
(«одна задача — один модуль»), поэтому границы модулей должны быть явными, а компиляция — инкрементальной.

**Решение.** Cargo workspace `crates/*`:
- `laravel-compat` — форматы Laravel/PHP без I/O: Encrypter, PHP serialize, bcrypt, PHP-JSON, Str-хелперы, сообщения валидации.
- `pelican-db` — подключение к СУБД, гибкие типы колонок, сущности SeaORM, установка схемы.
- `pelican-wings` — HTTP-клиент Wings + JWT.
- `pelican-core` — доменная логика (сервисы как в `app/Services`), auth/ACL, активити-лог, настройки, фоновые задачи.
- `pelican-api` — HTTP-слой: Remote/Client/Application API, трансформеры, ошибки, middleware.
- `pelican-web` — UI (SSR) — веха M6.
- `pelican-plugins` — WASM-хост — веха M7.
- `pelican` — бинарь: `serve`, CLI-команды (аналоги `p:*`).
- `difftest` — харнесс дифференциальных тестов и mock Wings.

Зависимости только «вниз»: api/web → core → db/wings/compat.

**Последствия.** Доменная логика не зависит от HTTP и переиспользуется UI, API и плагинами.
