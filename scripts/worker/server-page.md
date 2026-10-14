Задача: перенести страницу панели сервера Filament «{{PAGE}}» в веб-интерфейс Rust-панели (крейт pelican-web).
Рабочая папка — {{WORKDIR}} (git worktree; работай ТОЛЬКО в ней). Сначала прочитай CLAUDE.md и docs/ui-guide.md целиком
(особенно разделы «Панель сервера», «Списки», «Формы»).

ЭТАЛОНЫ: crates/pelican-web/src/server/mod.rs (экстрактор ServerPanel, навигация), crates/pelican-web/src/server/console.rs
(страница панели), crates/pelican-web/src/admin/mounts.rs (список + формы + удаление), crates/pelican-web/src/app/servers.rs
(таблица с действиями, вкладки).

ИСТОЧНИК ИСТИНЫ (только чтение): /Users/tazik/Desktop/Code/claude/pelican/panel/app/Filament/Server/{{SRC}}
(ресурс, Pages/*, RelationManagers/*), политики panel/app/Policies/*, модели panel/app/Models/*, сервисы panel/app/Services/*.
Готовая серверная логика (уже с диф-тестами): crates/pelican-api/src/client/{{CLIENT}} — переиспользуй её функции.
Переводы — ключи оригинала (crates/pelican-web/data/lang.json).
{{EXTRA}}

ЧТО СДЕЛАТЬ (функциональный паритет с оригиналом): все колонки/поля/действия/уведомления/права страницы,
включая модальные действия (создать, изменить, удалить и т.п.), подтверждения и ограничения (лимиты, состояния сервера).
Права — p.can/p.authorize с именами SubuserPermission (как в ресурсе оригинала).

ВЫХОДЫ (только эти файлы):
1. crates/pelican-web/src/server/{{MODULE}}.rs — обработчики и fn routes() (пути /server/{server}/{{SLUG}}…).
2. crates/pelican-web/src/server/mod.rs — ТОЛЬКО строка `mod {{MODULE}};` и `.merge({{MODULE}}::routes())` в routes().
3. crates/pelican-web/templates/server/{{MODULE}}*.html (или каталог templates/server/{{MODULE}}/).
4. Недостающие иконки — новые файлы в crates/pelican-web/assets/icons/; при необходимости свой JS — assets/js/{{MODULE}}.js.
5. Если пришлось открыть функцию в pelican-api — только смена видимости (pub), без изменения поведения.

ПРОВЕРКА: scripts/cargo check -p pelican-web и scripts/cargo test -p pelican-web — без ошибок и без новых
предупреждений. Не запускай сервер и не трогай базу. В отчёте перечисли, что из оригинала НЕ сделано и почему.
