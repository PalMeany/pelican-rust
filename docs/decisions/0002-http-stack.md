# 0002. HTTP: axum + tokio

**Статус:** Accepted

**Решение.** axum 0.8 (tower-middleware, extractors), tokio, reqwest для исходящих запросов к Wings,
tower-http для статических файлов/трейсинга. Маршрутизация повторяет `routes/*.php` 1:1,
middleware-цепочки — как в `bootstrap/app.php` и `RouteServiceProvider` (порядок важен для кодов ошибок).

**Последствия.** Самый распространённый стек → проще делегировать; tower-слои позволяют точно
воспроизвести порядок проверок Laravel (auth → IP → throttle → bindings → permissions).
