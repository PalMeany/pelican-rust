Задача: реализовать группу эндпоинтов Application API «{{GROUP}}» в Rust-панели и доказать совместимость диф-тестами.
Рабочая папка — {{WORKDIR}} (git worktree; работай ТОЛЬКО в ней). Сначала прочитай CLAUDE.md.

ЧТО РЕАЛИЗОВАТЬ: маршруты {{ROUTES}} (префикс /api/application).
Спецификация (обязательно прочитай целиком): docs/spec/application-api.md — §1 (общие правила: ACL, includes, пагинация, фильтры, ошибки) и {{SPEC}}.
Исходник оригинала (только чтение, это истина): /Users/tazik/Desktop/Code/claude/pelican/panel/routes/api-application.php,
/Users/tazik/Desktop/Code/claude/pelican/panel/app/Http/Controllers/Api/Application/{{CONTROLLER}}, FormRequest в
/Users/tazik/Desktop/Code/claude/pelican/panel/app/Http/Requests/Api/Application/, ресурсы /Users/tazik/Desktop/Code/claude/pelican/panel/app/Data/Api/Application/,
модели /Users/tazik/Desktop/Code/claude/pelican/panel/app/Models/ (правила валидации, хуки creating/saving/deleting, $attributes по умолчанию).
Если поведение Laravel/spatie неочевидно — смотри /Users/tazik/Desktop/Code/claude/pelican/.ref/vendor.

ВЫХОДЫ (только эти файлы):
1. crates/pelican-api/src/application/{{MODULE}}.rs — новый модуль с обработчиками (и ресурсом, если его нет в application/data.rs).
2. В crates/pelican-api/src/application/mod.rs — `mod {{MODULE}};` и строки `.route(...)` своих маршрутов (больше ничего не меняй).
3. difftest/scenarios/app-{{MODULE}}.yaml — сценарии: каждый маршрут, успех + ошибки (403 ACL: ключ papp_staffread01… только user:R,
   404, 422 валидация, 400 фильтр/сортировка, пагинация per_page/page, include с null_resource). Фикстура при необходимости —
   difftest/fixtures/app-{{MODULE}}.yaml (существующие не меняй). Ключи см. шапки difftest/fixtures/base.yaml и app-extra.yaml.

ОБРАЗЦЫ (повторяй стиль, используй готовое):
- crates/pelican-api/src/application/users.rs — список (filters/sorts/Page), view, store (201 + meta.resource), update, delete, ACL.
- crates/pelican-api/src/application/data.rs — ресурсы с include (user_item, role_item, server_item, node_item, egg_item, database_item,
  database_host_item); include-проверки через ctx.allows_admin("resource"); порядок include = $availableIncludes.
- ACL: ctx.acl("resource", READ|WRITE) первым делом в обработчике (порядок: ACL 403 → валидация 422 → действие).
- Ошибки: crate::error::ApiError (DisplayException → ApiError::display(code, 400, msg); 404 модели → ApiError::model_not_found()).
- Ввод/валидация: crate::input::In, crate::validation::Validator; unique/exists — CustomRule::precomputed после строковых правил.
- Запись: pelican_db::write::{insert, update}; JSON-колонки только через pelican_db::write. Время: pelican_core::time::now().
- Списки без ORDER BY в оригинале → без ORDER BY у нас (ADR-0011, pelican_db::raw::stmt).

ПРОВЕРКА (обязательно, до зелёного):
  scripts/cargo build -p difftest
  DIFFTEST_ENV={{ENV}} scripts/difftest --backend mariadb --filter app/{{MODULE}}
затем --backend pgsql и --backend sqlite (тот же DIFFTEST_ENV). Если красное — исправляй КОД, не сценарий.
Не меняй pipeline.rs, envelope.rs, binding.rs (кроме добавления своих привязок в ветку Group::Application — опиши в отчёте),
error.rs, validation.rs, data.rs, crates/difftest.
