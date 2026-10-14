# 0008. Плагины: WASM component model

**Статус:** Proposed (детализация в M7 после spec/plugins.md)

**Решение (направление).** wasmtime + component model, API описан в WIT: события (аналоги Laravel
events), расширяемые реестры оригинала (avatar, captcha, OAuth, backup-адаптеры, типы задач
расписаний, server features), регистрация UI (ресурсы/страницы/виджеты/навигация декларативно,
ADR-0005), HTTP-маршруты, настройки, права, переводы. Возможности хоста выдаются явно
(capabilities: БД только через API, HTTP-исходящие по allowlist). Оригинальные PHP-плагины —
отдельный PHP-sidecar после MVP (M8).
