# 0019. События моделей Eloquent (`eloquent.*`) для вебхуков и плагинов

**Статус:** Accepted

**Контекст.** В оригинале `EventServiceProvider` вешает `DispatchWebhooks` на `eloquent.created*`,
`eloquent.updated*`, `eloquent.deleted*`: любой `create()/save()/update()/delete()` экземпляра модели шлёт
глобальным вебхукам `{event: "eloquent.<action>: App\Models\<Model>", data: $model->toArray(), timestamp}`
(кроме `eloquent.created: App\Models\Webhook`). Массовые `query()->update()/delete()` событий не дают. У нас нет
ORM-событий: запись идёт через sea-orm и сырой SQL.

**Решение.**
- `pelican_api::model_events::fire(st, db, action, model, data)` — явный вызов после записи в тех местах, где
  оригинал работает с экземпляром модели. Глобальные вебхуки (scope `global`, событие в `events`) получают
  оригинальный payload; WASM-плагины, подписанные на событие, — его же в `on-event`. Список глобальных вебхуков
  кэшируется на 5 с, без подписчиков вызов ничего не стоит.
- `data` строит `pelican_api::model_array::<таблица>(&Model)` — `toArray()` модели: `$hidden` убраны, касты
  `$casts`/`casts()` (даты — `Y-m-d\TH:i:s.u\Z` в UTC, bool, int, json-массивы), ключи — в физическом порядке колонок
  СУБД (`pelican_db::column_order`, как `SELECT *`; у SQLite/PG и MySQL он разный). После атрибутов — загруженные
  связи: `$with` моделей (`Server.allocation`, `Schedule.tasks`, `ActivityLog.subjects`) у экземпляров, прочитанных
  запросом (не у только что созданных), и связи, которые код оригинала подгружает до записи (`AuthenticateServerAccess`
  в Client API: `subusers`/`node`/`transfer`; `DetailsModificationService`: `user`) — `fire_id_with`. Для удалений,
  после которых связи исчезают каскадом (Schedule), payload строится до удаления (`pending`).
  Для `created` порядок атрибутов в оригинале — порядок присваивания при создании, а не колонок, и поля,
  не заданные при создании, отсутствуют; мы отдаём строку целиком (расхождение в порядке и составе ключей
  для `created` осознанное).
- Вызовы — после commit транзакции (доставка вебхука — синхронный HTTP); ошибка события логируется и не
  ломает уже выполненную запись. Диф-тест `webhooks/model-events` сверяет payload'ы `updated`/`deleted` на 4 СУБД.
- Инструментируются основные пути записи (Client/Application API, админка, панель сервера, Remote API,
  фоновые задачи). Побочные `save()` фреймворка (например, `remember_token` при входе) не воспроизводятся.

**Последствия.** Вебхуки и плагины на события моделей работают для всех значимых операций; полный охват
служебных записей Laravel не цель.
