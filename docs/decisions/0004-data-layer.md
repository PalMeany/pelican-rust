# 0004. Слой данных: SeaORM + гибкие типы колонок

**Статус:** Accepted

**Контекст.** Одна логическая колонка имеет разные физические типы: `nodes.public` — `smallint unsigned`
(MySQL) / `smallint` (PG) / `integer` (SQLite); `behind_proxy` — `tinyint(1)` / `boolean`; JSON — `json`
(MySQL, PG), `longtext CHECK json_valid` (MariaDB), `text` (SQLite и часть колонок PG); MySQL
`TIMESTAMP`. sqlx декодирует строго (нельзя `i64` из PG `int4`, signed из unsigned MySQL), а PG строго
проверяет типы параметров (нет assignment-cast bool→int, text→json).

**Решение.** SeaORM 2 (runtime-выбор СУБД, ActiveModel пишет только изменённые поля — как dirty-tracking
Eloquent) с собственными типами колонок в `pelican-db::types`:
- `Int` — декодирует любые целые/bool, биндит BIGINT (PG делает assignment-cast в int2/int4).
- `Bool` — колонка `boolean` в PG: биндит BOOL. `SmallBool` — bool-каст поверх целой колонки в PG: биндит 0/1.
- `Json` — декодирует из строки или json; биндит JSON (PG приводит json→text при присваивании).
- `Timestamp` — `NaiveDateTime` с точностью до секунды; MySQL `TIMESTAMP` читается как стенные часы сессии.
Выбор `Bool`/`SmallBool` для колонки определяется её типом в PG-схеме.

Сессия как у Laravel: MySQL `SET NAMES … COLLATE …`, `sql_mode='NO_ENGINE_SUBSTITUTION'` (или strict-набор
при `DB_STRICT_MODE`), **без** `time_zone` (sqlx по умолчанию ставит `+00:00` — отключено), без
`PIPES_AS_CONCAT`; PG `search_path=DB_SCHEMA`; SQLite `foreign_keys` по `DB_FOREIGN_KEYS`, journal_mode файла не меняем.

**Запись (уточнено).** INSERT/UPDATE идут через `pelican_db::write`, а не через `ActiveModel::insert`:
JSON кодируется как PHP `json_encode` (флаги 0: `\/`, `\uXXXX`) и биндится текстом; на PostgreSQL —
`CAST($1 AS json)`, чтобы колонки `json` хранили текст как есть (SeaORM биндит `jsonb`, который
переупорядочивает ключи — это всплыло бы и в ответах API). Итог: байты в БД совпадают с Laravel на
MariaDB/SQLite/PG; на MySQL `JSON` нормализует сам сервер (одинаково для обеих панелей).

**Последствия.** Проверено на SQLite, MariaDB 11.4, MySQL 8.4, PostgreSQL 17; диф-тесты сравнивают
JSON-текст побайтно. Риск: «нулевые» даты MySQL `0000-00-00` в старых БД (QUESTIONS Q5).
