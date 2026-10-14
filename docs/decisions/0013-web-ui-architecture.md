# 0013. Веб-интерфейс: сессии, CSRF, шаблоны, ассеты, ресурсы

**Статус:** Accepted (уточняет ADR-0005 и ADR-0009)

**Контекст.** Нужен функциональный паритет с тремя панелями Filament (`/`, `/admin`, `/server/{id}`),
без PHP-форматов сессий (ADR-0009) и без node-сборки (ADR-0005).

**Решение.**
- Крейт `pelican-web`: всё, что не `/api/*`, монтируется в общий axum-роутер бинаря.
- **Сессии** — таблица `sessions` (как у Laravel: id, user_id, ip_address, user_agent, payload,
  last_activity); payload — base64(JSON) со своим форматом: `user_id`, `csrf`, `flash`, `intended`,
  состояние MFA-челленджа. Cookie — `SESSION_COOKIE` (по умолчанию `pelican_session`), HttpOnly,
  SameSite=Lax, Secure при https `APP_URL`, срок `SESSION_LIFETIME` минут; «запомнить меня» = cookie
  на 5 лет. Старые PHP-сессии не читаются — пользователи перелогиниваются.
- **CSRF** — токен на сессию; все не-GET запросы проверяются по полю `_token` или заголовку
  `X-CSRF-TOKEN` (htmx получает его из `<meta>`), ответ 419 как у Laravel.
- **Шаблоны** — minijinja; встроены в бинарь (build.rs собирает каталог `templates/`), каталог
  `PELICAN_VIEWS_PATH` может переопределять их (темы/плагины). Переводы — дамп `lang/*` оригинала
  в JSON, функция `trans()` в шаблонах; локаль = язык пользователя.
- **Ассеты** — вендоренные файлы (htmx, xterm.js, CodeMirror, собственные CSS/JS) в
  `crates/pelican-web/assets`, встроены в бинарь, отдаются с хешем в URL и долгим кешем. Никаких CDN.
- **Интерактивность** — htmx (формы, таблицы, модалки, опрос), отдельные модули JS для консоли
  (WebSocket Wings), редактора файлов и графиков.
- **Ресурсы** — декларативные описания в Rust (`Table{columns, filters, actions}`,
  `Form{fields → правила crate::validation}`), общие шаблоны; доменная логика — те же сервисы, что
  у API (`pelican_api::services`, client/application модули), без HTTP-прослойки.
- **Права**: админка — spatie-разрешения пользователя (`viewList user`, `update node`, …, root admin
  — всё); серверная панель — права сабюзера (как Client API).

**Последствия.** Один процесс, один язык; UI проверяется e2e-сценариями в браузере (не побайтно).
Сессии оригинала не переносятся — допустимо по ADR-0009.
