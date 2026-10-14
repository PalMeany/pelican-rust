Задача: написать SeaORM-сущности (Rust) для таблиц: {{TABLES}}.
Рабочая папка — /Users/tazik/Desktop/Code/claude/pelican/pelican-rs (Rust workspace). Сначала прочитай CLAUDE.md.

ВХОДЫ:
- Образец (повтори стиль 1:1): crates/pelican-db/src/entities/nodes.rs
- Типы колонок: crates/pelican-db/src/types.rs (Int, Bool, SmallBool, Json, Timestamp)
- Схемы БД оригинала: crates/pelican-db/schema/pgsql.sql (главный источник типов), mysql.sql, sqlite.sql
- Модели оригинала (касты!): ../panel/app/Models/*.php — смотри $casts / casts() и $table.

ВЫХОДЫ: только файлы crates/pelican-db/src/entities/<table>.rs для перечисленных таблиц (сейчас там заглушки с TODO — замени целиком). mod.rs НЕ трогай, другие файлы НЕ трогай.

ПРАВИЛА ТИПОВ (строго):
- Все целочисленные колонки (int, bigint, smallint, unsigned) → Int; nullable → Option<Int>.
- Колонка `boolean` в pgsql.sql → Bool. Колонка, которую модель кастит в bool, но в pgsql.sql она smallint/integer → SmallBool.
  Если модель НЕ кастит в bool, а колонка smallint/integer → Int.
- Тип `json` в pgsql.sql, ЛИБО модель кастит колонку в array/json/collection/AsArrayObject/AsCollection → Json (nullable → Option<Json>).
- Колонки с кастом 'encrypted' или хранящие зашифрованное значение → String (расшифровка не здесь).
- timestamp/datetime → Timestamp (nullable → Option<Timestamp>).
- varchar/text/inet/прочее → String / Option<String>. Enum-касты → String (или Int, если колонка целая).
- Nullable = в pgsql.sql нет NOT NULL.
- Поля структуры — в том же порядке, что колонки в pgsql.sql. Имена полей = имена колонок; если имя колонки — ключевое слово Rust (type, schema и т.п.), используй `r#type` с атрибутом `#[sea_orm(column_name = "type")]` при необходимости.
- Первичный ключ: колонка id с `#[sea_orm(primary_key)]`. Если id нет (pivot-таблицы, password_resets, sessions, node_role, backup_host_node и т.п.) — составной ключ из колонок, однозначно определяющих строку (по UNIQUE/PRIMARY KEY в схеме; для password_resets — email+token), у каждой `#[sea_orm(primary_key, auto_increment = false)]`. Если id строковый (sessions.id, notifications.id) — `#[sea_orm(primary_key, auto_increment = false)] pub id: String`.
- Над структурой однострочный doc-комментарий: какая таблица и какая модель оригинала (как в nodes.rs). Над шифрованными колонками — комментарий `/// Laravel-encrypted.`
- `pub enum Relation {}` оставь пустым, `impl ActiveModelBehavior for ActiveModel {}` — как в образце.

ПРОВЕРКА: `scripts/cargo check -p pelican-db` должен пройти без ошибок и предупреждений в твоих файлах.
