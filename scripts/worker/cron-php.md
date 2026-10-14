Задача: портировать PHP-библиотеку dragonmantank/cron-expression на Rust (крейт crates/cron-php) так,
чтобы результаты совпадали с PHP ПОБАЙТНО на эталонных векторах.
Рабочая папка — {{WORKDIR}} (git worktree; работай ТОЛЬКО в ней). Сначала прочитай CLAUDE.md.

ВХОД (только чтение): /Users/tazik/Desktop/Code/claude/pelican/.ref/vendor/dragonmantank/cron-expression/src/Cron/*.php
(CronExpression.php, AbstractField.php, MinutesField.php, HoursField.php, DayOfMonthField.php, MonthField.php,
DayOfWeekField.php, FieldFactory.php). Портируй алгоритм максимально близко к оригиналу (включая обработку DST
в HoursField/MinutesField и getRunDate, макросы @hourly/@daily/..., L, W, #, ?, имена месяцев/дней, шаги и диапазоны,
лимит итераций и исключение «Impossible CRON expression»).

ВЫХОД: crates/cron-php/src/*.rs (lib.rs уже есть — замени заглушку). Публичный API РОВНО такой:
  pub struct CronExpression { .. }
  #[derive(Debug, thiserror::Error)] pub enum CronError { .. }   // невалидное выражение / невозможное выражение
  impl CronExpression {
      pub fn new(expr: &str) -> Result<Self, CronError>;                       // = new CronExpression($expr) (бросает на невалидном)
      pub fn is_valid_expression(expr: &str) -> bool;                          // = CronExpression::isValidExpression
      pub fn next_run_date<Tz: chrono::TimeZone>(&self, now: chrono::DateTime<Tz>) -> Result<chrono::DateTime<Tz>, CronError>;
          // = getNextRunDate($now) с nth=0, allowCurrentDate=false; результат в той же таймзоне
  }
Зависимости уже в Cargo.toml (chrono, chrono-tz, thiserror). Cargo.toml не меняй, других крейтов не трогай.

ТЕСТ: crates/cron-php/tests/vectors.rs + vectors.json (1840 векторов, сгенерированы настоящим PHP). Не меняй их.
  scripts/cargo test -p cron-php
Итерируй, пока тест не станет зелёным. Если отдельные векторы не удаётся совпасть — объясни в отчёте почему.
