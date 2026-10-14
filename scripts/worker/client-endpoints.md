Задача: реализовать группу эндпоинтов Client API «{{GROUP}}» в Rust-панели и доказать совместимость диф-тестами.
Рабочая папка — {{WORKDIR}} (git worktree; работай ТОЛЬКО в ней). Сначала прочитай CLAUDE.md.

ЧТО РЕАЛИЗОВАТЬ: маршруты {{ROUTES}}
Спецификация (обязательно прочитай целиком нужные разделы): docs/spec/client-api.md — {{SPEC}}.
Исходник оригинала (только чтение, это истина): /Users/tazik/Desktop/Code/claude/pelican/panel/routes/api-client.php, /Users/tazik/Desktop/Code/claude/pelican/panel/app/Http/Controllers/Api/Client/Servers/{{CONTROLLER}},
соответствующие FormRequest в /Users/tazik/Desktop/Code/claude/pelican/panel/app/Http/Requests/Api/Client/Servers/, сервисы из /Users/tazik/Desktop/Code/claude/pelican/panel/app/Services/, ресурсы /Users/tazik/Desktop/Code/claude/pelican/panel/app/Data/Api/Client/.
Если поведение Laravel неочевидно — смотри /Users/tazik/Desktop/Code/claude/pelican/.ref/vendor (исходники фреймворка).

ВЫХОДЫ (только эти файлы):
1. crates/pelican-api/src/client/{{MODULE}}.rs — новый модуль с обработчиками.
2. В crates/pelican-api/src/client/mod.rs — добавь `mod {{MODULE}};` и строки `.route(...)` для своих маршрутов (больше ничего там не меняй).
3. difftest/scenarios/client-{{MODULE}}.yaml — диф-сценарии: каждый маршрут, успех + главные ошибки (403 без права у сабюзера, 404 чужой ресурс, 422 валидация, конфликты).
   При необходимости — новая фикстура difftest/fixtures/client-{{MODULE}}.yaml (существующие фикстуры не меняй).

ОБРАЗЦЫ (повторяй стиль и используй готовую инфраструктуру, не изобретай свою):
- crates/pelican-api/src/client/servers.rs (power/command: ctx.authorize, Validator, Wings, activity, no_content), client/activity.rs (списки, фильтры, пагинация)
- crates/pelican-api/src/client/data.rs — ресурсы (если нужен новый ресурс — добавь функцию в СВОЙ модуль, data.rs не трогай)
- Модели: crates/pelican-api/src/binding.rs (ctx.bound.database/schedule/task/allocation/backup/subuser уже привязаны и scoped к серверу)
- Права: ctx.authorize(&st, "perm") (= FormRequest permission, 403), ctx.can (HAS), ctx.gate (GATE, include-проверки)
- Ошибки: crate::error::ApiError (DisplayException → ApiError::display(code, 400, msg) — compact; HttpException → pretty)
- Ввод/валидация: crate::input::In, crate::validation::Validator (правила строками как в PHP)
- Запись в БД: pelican_db::write::{insert, insert_id, update} или update_many с col_expr; JSON-колонки только через pelican_db::write (на PG нельзя биндить text в json).
- Время: pelican_core::time::now(); activity: ctx.activity("event").property(..).subject(..).save(&st.db) (дефолтный subject server добавляется сам).
- Wings: st.wings (crates/pelican-wings/src/client.rs); если нужного метода нет — добавь его ТУДА же по образцу (S*/F*/B* из docs/spec/remote-api.md §4.4).
- Списки без ORDER BY в оригинале → без ORDER BY у нас и SQL той же формы (ADR-0011, pelican_db::raw::stmt).

ПРОВЕРКА (обязательно, итеративно, до зелёного):
  scripts/cargo build -p difftest
  DIFFTEST_ENV={{ENV}} scripts/difftest --backend mariadb --filter client-{{MODULE}}     # твои сценарии (DIFFTEST_ENV обязателен!)
затем то же для --backend pgsql и --backend sqlite (с тем же DIFFTEST_ENV={{ENV}}). Сравнивается ответ (байты), изменения БД и вызовы Wings с ОРИГИНАЛЬНОЙ панелью.
Если тест красный — исправляй КОД (не сценарий), пока ответ не совпадёт с оригиналом. Не подгоняй сценарии под свою реализацию.
Не меняй pipeline.rs, envelope.rs, binding.rs, error.rs, validation.rs, data.rs, crates/difftest — если без этого никак, опиши в отчёте, что нужно.
