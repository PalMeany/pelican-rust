# pelican-rs — правила для исполнителей

Переписываем панель Pelican (`../panel`, PHP/Laravel — ТОЛЬКО ЧТЕНИЕ) на Rust. Wings не трогаем.
Главное требование — совместимость: та же БД, тот же APP_KEY, те же форматы API.

- Сборка и тесты ТОЛЬКО через `scripts/cargo` (cargo в Linux-контейнере; на хосте нет линковщика):
  `scripts/cargo check -p <crate>`, `scripts/cargo test -p <crate>`. `cargo fmt` можно на хосте.
- Контракты оригинала — `docs/spec/*.md`; решения — `docs/decisions/*.md`. Не противоречь им.
- Типы колонок БД — только из `pelican_db::types` (`Int`, `Bool`, `SmallBool`, `Json`, `Timestamp`),
  `Option<…>` для nullable. `Bool` vs `SmallBool` — по типу колонки в `crates/pelican-db/schema/pgsql.sql`
  (`boolean` → `Bool`, `smallint`/`integer` → `SmallBool`).
- Порядок полей в JSON-ответах = порядок в трансформерах оригинала. Не используй HashMap для
  сериализуемых объектов (только структуры или `IndexMap`).
- Без `unwrap()` в коде, обрабатывающем запросы. Ошибки — через типы ошибок крейта.
- Комментарии — на английском, кратко, только где неочевидно; ссылайся на исходник оригинала
  (`app/Services/...php`), если повторяешь его семантику.
- Не меняй файлы вне своей задачи, не трогай PLAN.md/PROGRESS.md/QUESTIONS.md/docs/decisions.
