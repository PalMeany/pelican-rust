Задача: перенести ресурс админки Filament «{{RESOURCE}}» в веб-интерфейс Rust-панели (крейт pelican-web).
Рабочая папка — {{WORKDIR}} (git worktree; работай ТОЛЬКО в ней). Сначала прочитай CLAUDE.md и docs/ui-guide.md целиком.

ЭТАЛОН (повторяй его устройство): crates/pelican-web/src/admin/mounts.rs + templates/admin/list.html +
templates/admin/mounts/form.html; список с вкладками/фильтрами — crates/pelican-web/src/app/servers.rs.

ИСТОЧНИК ИСТИНЫ (только чтение): /Users/tazik/Desktop/Code/claude/pelican/panel/app/Filament/Admin/Resources/{{DIR}}/
(ресурс, Pages/*, RelationManagers/*), модель(и) /Users/tazik/Desktop/Code/claude/pelican/panel/app/Models/{{MODELS}}
($validationRules, booted()-хуки, $attributes по умолчанию), связанные сервисы в panel/app/Services/.
Переводы — ключи оригинала (данные в crates/pelican-web/data/lang.json).
{{EXTRA}}

ЧТО СДЕЛАТЬ (функциональный паритет с оригиналом):
- Список: колонки, сортировка, поиск, фильтры, вкладки, действия строк, бейджи/иконки/цвета, пустое состояние — как в таблице ресурса.
- Создание/редактирование/просмотр: все поля формы (секции, вкладки, подсказки, значения по умолчанию), валидация
  (правила полей Filament + $validationRules модели), хуки модели, связи (sync pivot), уведомления, редиректы.
- Удаление (и ограничения из хуков deleting/исключений), заголовочные действия страниц.
- RelationManagers ресурса — как вкладки/секции на странице редактирования (список + действия).
- Права: authorize/can по именам spatie, как делает ресурс/политика оригинала; can_target_node для нод/серверов.
- Переиспользуй доменную логику из crates/pelican-api (application/*.rs, services.rs) и pelican-core; если функция
  приватная — сделай её pub(crate)/pub без изменения поведения.

ВЫХОДЫ (только эти файлы):
1. crates/pelican-web/src/admin/{{MODULE}}.rs — обработчики и fn routes() (пути /admin/{{SLUG}}…).
2. crates/pelican-web/src/admin/mod.rs — ТОЛЬКО строка `mod {{MODULE}};` и `.merge({{MODULE}}::routes())` в routes().
3. crates/pelican-web/templates/admin/{{MODULE}}/*.html (для списка можно использовать общий admin/list.html).
4. Недостающие иконки — новые файлы в crates/pelican-web/assets/icons/ (см. docs/ui-guide.md).
5. Если пришлось открыть функцию в pelican-api — только смена видимости.

ПРОВЕРКА: scripts/cargo check -p pelican-web и scripts/cargo test -p pelican-web — без ошибок и без новых
предупреждений. Не запускай сервер и не трогай базу. В отчёте перечисли, что из оригинала НЕ сделано и почему.
