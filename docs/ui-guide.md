# UI: как писать страницы (крейт `pelican-web`)

Архитектура — ADR-0005, ADR-0013. Эталоны, с которых копировать подход:
- ресурс админки (список + создание/редактирование + удаление): `crates/pelican-web/src/admin/mounts.rs`,
  шаблоны `templates/admin/list.html`, `templates/admin/mounts/form.html`;
- список с вкладками, фильтрами, опросом и действиями строк: `crates/pelican-web/src/app/servers.rs`.

Цель — **функциональный паритет** с Filament-страницей оригинала (поля, колонки, фильтры, действия, права,
валидация, хуки моделей, уведомления), а не побайтная копия разметки. Источник истины — PHP оригинала:
ресурс `panel/app/Filament/<Panel>/Resources/<X>/<X>Resource.php`, его `Pages/*`, `RelationManagers/*`,
модель `panel/app/Models/<X>.php` (`$validationRules`, `booted()` — хуки creating/saving/deleting, `$attributes`),
сервисы `panel/app/Services/**`. Исходники Filament/Laravel — `/Users/tazik/Desktop/Code/claude/pelican/.ref/vendor`.

## Обработчик
- Экстракторы (`src/ctx.rs`, `src/admin/mod.rs`): `Web` (любой запрос), `Authed` (нужен вход; `web.user: AuthUser`),
  `Admin` (вход + `isAdmin()`), позже `ServerPanel` для `/server/{id}`.
- Права админки: `admin.authorize("viewList node")?` / `admin.can("update node")` — имена spatie
  (`viewList|view|create|update|delete` + модель: `node, server, egg, user, role, mount, webhook, apiKey, databaseHost,
  backupHost, allocation, database`; спец.: `view settings`, `update settings`, `view health`, `view panelLog`,
  `import egg`, `export egg`, `viewList plugin` …). Для серверов/нод дополнительно `admin.user.can_target_node(node_id)`.
- Рендер страницы админки: `admin.page("<slug>", "admin/<x>/<page>.html", context!{...}).await` — добавляет сайдбар.
  Прочие: `web.render(template, context!{...})`. Фрагмент: `web.render_block(template, block, ctx)`.
- Переводы: в Rust `web.t("admin/node.nav_title")`, `web.t_with(key, &[("label", "…")])`,
  `crate::i18n::trans_choice(&locale, key, n, &[])`; в шаблонах `trans('key', name=value)`, `trans_choice('key', n)`.
  Ключи — ровно как в оригинале (`trans('admin/node.…')`, `filament-actions::…`, `filament-panels::…`).
  Дамп всех переводов — `crates/pelican-web/data/lang.json`. Никаких захардкоженных английских строк, если в оригинале есть ключ.
- Уведомления (Filament `Notification::make()`): `web.flash_success(title)`, `web.flash_danger(title, Some(body))`,
  `web.session.flash("warning", title, body)`. Показываются на следующей странице.
- Редирект после POST: `Ok(web.redirect("/admin/x"))` (работает и для htmx). Возврат на список из действия
  строки: `form.back("/admin/x")` (таблица кладёт `_back`).
- Ошибки: `WebError::{NotFound, Forbidden, Unprocessable, Internal}`, `?` для `DbErr`. Без `unwrap()`.

## Панель сервера (`src/server/mod.rs`)
- Экстрактор `ServerPanel` (путь `/server/{server}/…`, `{server}` = `uuid_short`): `p.server`, `p.node`,
  `p.user`, `p.can("file.read")` / `p.authorize("backup.create")?` — права сабюзера (`SubuserPermission` оригинала;
  владелец и админы с `update server` имеют все), `p.conflict()` (`isInConflictState()`), `p.allocation().await?`.
- Рендер: `p.page("<slug>", "server/<x>.html", context!{...})` — сайдбар панели сервера (slug пункта навигации:
  `files`, `backups`, `schedules`, `users`, `databases`, `network`, `activity`, `mounts`, `startup`, `settings`, `webhooks`),
  баннер конфликтного состояния, меню пользователя. Ссылки внутри панели — от `p.base_url()`.
- Логика уже реализована для Client API в `crates/pelican-api/src/client/*.rs` (+ клиент Wings `pelican_wings`):
  вызывай её функции (сделай `pub`, если нужно, сохранив поведение API — по ним идут диф-тесты), а не HTTP.
  Активити — как в Client API (`server:*` события, субъект — сервер).

## Списки (`src/table.rs`, `templates/components/table.html`)
1. `let p = ListParams::parse(admin.query.as_deref(), &[10, 25, 50, 0], 10);` (0 = «All»).
2. Свой SQL (sea-orm или `pelican_db::raw::stmt(b, "select … from {t} where {col} = ?", vals)` — `{ident}` квотируется
   под СУБД, `?` нумеруется для PG). Поиск — `LOWER(col) LIKE ?` c `%q%`; сортировка по `p.sort`/`p.desc`;
   `p.limit_offset()`; общий `count(*)`; `p.clamp(total)`.
3. `Table::new("<dom-id>", "/admin/x")`, `columns = vec![Column::new("name", label).sortable(), …]`,
   строки `Row::new(key).url(link).cell("name", Cell::text_desc(…))`. Типы ячеек: `Text`, `Badge`, `Badges`, `Icon`,
   `Bool`, `Progress`, `Image`. Действия: `Action::link(label, icon, url)`, `Action::post(label, icon, url).color("danger")
   .confirm(question).field(k, v)`; в `row.actions` (кнопки) или `row.group` (выпадающее меню).
   `table.searchable`, `table.tabs`, `table.filters` (`Filter{name,label,options,value}`, значение — `p.filter("name")`),
   `table.empty_heading/empty_icon`, `table.poll = Some("15s")`.
4. `table.finish(&p, total, &opts, &locale)` и шаблон `admin/list.html` (`title`, `table`, `create{label,url}`,
   `header_actions`).

## Формы (`src/forms.rs`, `templates/components/forms.html`)
- Принимать `form: Form` (urlencoded; CSRF проверяется middleware). `form.get("x")`, `form.opt`, `form.bool` (toggle),
  `form.list("x")` для `name="x[]"`/multiple select.
- Валидация — `pelican_api::validation::Validator` (правила Laravel строками):
  `let mut v = Validator::for_value(&json); v.rules("name", &["required","string","max:191"]); v.run();`
  уникальность/существование — заранее посчитать и `v.custom("name", CustomRule::precomputed("unique", msg))`;
  `Errors::from_validator(&v)`. Человеческие имена полей — `v.attribute_name("name", &label.to_lowercase())`.
  Правила = поля Filament-формы (`required`, `maxLength`, `rules([...])`) + `Model::$validationRules`.
- При ошибках — повторно отрендерить форму с введёнными значениями и `errors` (статус 200). При успехе —
  уведомление `…create-record.notifications.created.title` / `…edit-record.notifications.saved.title` и редирект.
- Макросы: `f.text(name, label, value, errors, required=, hint=, placeholder=, attrs='maxlength="64"', prefix=, suffix=)`,
  `f.password`, `f.textarea(..., code=true)`, `f.select(name, label, [[v, label],…], value, errors, multiple=, placeholder=)`,
  `f.toggle(name, label, checked)` (шлёт 0/1), `f.radios(name, label, options, value, errors, inline=true)` (ToggleButtons),
  `f.checkbox`. Секции — `<fieldset class="card card-body grid grid-2">`, колонки `span-2`, `span-full`.
  Вкладки внутри формы — `<nav class="tabs">` + `hidden` панели (см. app.js: `data-tab`), или отдельные страницы.
- Кнопки шапки (Delete/Save) — как в `admin/mounts/form.html`; удаление — POST-форма с `data-confirm*`.

## База данных
- Сущности `pelican_db::entities::*`, типы колонок `pelican_db::{Int, Bool, SmallBool, Json, Timestamp}`.
- Вставка/обновление, как пишет Laravel: `pelican_db::write::insert_id(&db, &am)`, `pelican_db::write::update(&db, &am)`
  (JSON-колонки на PG через CAST). `created_at/updated_at` ставить явно: `pelican_core::time::now()`.
- Связи many-to-many / morph (`mountables`, `egg_mount`, `node_role`, …) — синхронизировать явно (delete + insert).
- Транзакции: `let txn = st.db.begin().await?; … txn.commit().await?;`.
- Доменная логика уже есть в `pelican-api` (Application/Client API, `services.rs`, `jobs.rs`) и `pelican-core` —
  переиспользуй функции (сделай `pub`, если нужно, без изменения поведения), не дублируй.
- Активити: `web.activity("event").subject(morph::X, id).property(k, v).log(&web.st).await` (как в оригинале).

## Шаблоны
- `{% extends "layouts/panel.html" %}`, блок `content`. Импорт: `{% import "components/forms.html" as f %}`,
  `{% import "components/table.html" as tbl %}`. Иконки: `{{ icon('tabler-name-without-prefix', 'icon-sm') }}`
  (SVG из `assets/icons`; если иконки нет — скопируй SVG из
  `/Users/tazik/Desktop/Code/claude/pelican/.ref/vendor/secondnetwork/blade-tabler-icons/resources/svg/` в стиле
  соседних файлов: без class/width/height).
- CSS-классы: `card`, `card-head`, `card-body`, `grid grid-2|3|4`, `stack`, `row`, `btn`, `btn-gray`, `btn-danger`,
  `btn-sm`, `badge badge-<color>`, `banner <color>`, `muted`, `small`, `kv` (`<dl>`), `code-block`, `tabs`.
- Модалка: `<dialog id="x" class="modal">` + кнопка `data-modal-open="x"`, закрытие `data-modal-close`.
- Копирование: `data-copy="текст"`. Подтверждение формы: `data-confirm="вопрос" data-confirm-title data-confirm-ok`.

## Проверка
- `scripts/cargo check -p pelican-web` и `scripts/cargo test -p pelican-web` без ошибок и предупреждений в твоих файлах.
- Чистая логика (форматирование, разбор) — юнит-тесты в модуле.
- Диф-тестов для UI нет (ADR-0013); оркестратор проверяет страницы в браузере.
