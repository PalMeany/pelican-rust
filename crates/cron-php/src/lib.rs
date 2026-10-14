//! Port of `dragonmantank/cron-expression` 3.x
//! (`vendor/dragonmantank/cron-expression/src/Cron`) as used by Laravel/Pelican.
//!
//! Only the pieces needed by the panel are public: parsing/validation and
//! next-run-date computation with byte-exact semantics against the PHP
//! reference vectors (`tests/vectors.rs`).

mod field;
mod php;

use chrono::{DateTime, Datelike, NaiveDate, Offset, TimeZone, Timelike};
use field::FieldKind;

/// Max iteration count when searching for the next run date
/// (`CronExpression::$maxIterationCount`).
const MAX_ITERATION_COUNT: usize = 1000;

/// Built-in aliases (`CronExpression::MAPPINGS`).
const ALIASES: &[(&str, &str)] = &[
    ("@yearly", "0 0 1 1 *"),
    ("@annually", "0 0 1 1 *"),
    ("@monthly", "0 0 1 * *"),
    ("@weekly", "0 0 * * 0"),
    ("@daily", "0 0 * * *"),
    ("@midnight", "0 0 * * *"),
    ("@hourly", "0 * * * *"),
];

#[derive(Debug, thiserror::Error)]
pub enum CronError {
    /// `InvalidArgumentException('... is not a valid CRON expression')`.
    #[error("`{0}' is not a valid CRON expression")]
    Invalid(String),
    /// `InvalidArgumentException('Invalid CRON field value ... at position ...')`.
    #[error("Invalid CRON field value `{0}' at position {1}")]
    InvalidField(String, usize),
    /// `FieldFactory` rejecting positions beyond WEEKDAY.
    #[error("{0} is not a valid position")]
    BadPosition(usize),
    /// `RuntimeException('Impossible CRON expression')` after exhausting the
    /// iteration limit.
    #[error("Impossible CRON expression")]
    Impossible,
    /// Other runtime exceptions bubbling out of the original library
    /// (bad ranges, unresolvable weekdays, hashed-weekday errors, ...).
    #[error("{0}")]
    Runtime(String),
}

/// A parsed CRON expression.
pub struct CronExpression {
    /// Expression parts as originally supplied (`cronParts`).
    parts: Vec<String>,
}

impl CronExpression {
    /// Parse a CRON expression, mirroring `new CronExpression($expr)`
    /// (throws on invalid input).
    pub fn new(expr: &str) -> Result<Self, CronError> {
        let lowered = expr.to_lowercase();
        let mut resolved = expr.to_string();
        for (alias, mapped) in ALIASES {
            if *alias == lowered {
                resolved = (*mapped).to_string();
                break;
            }
        }

        // preg_split('/\s/', $value, -1, PREG_SPLIT_NO_EMPTY)
        let split: Vec<&str> = resolved.split_whitespace().collect();

        let question_mark_in_invalid_part = split.first().map_or(false, |p| *p == "?")
            || split.get(1).map_or(false, |p| *p == "?")
            || split.get(3).map_or(false, |p| *p == "?");
        let too_many_question_marks =
            split.get(2).map_or(false, |p| *p == "?") && split.get(4).map_or(false, |p| *p == "?");

        if split.len() < 5 || question_mark_in_invalid_part || too_many_question_marks {
            return Err(CronError::Invalid(resolved));
        }

        let parts: Vec<String> = split.into_iter().map(str::to_string).collect();

        // setPart() validates every position through the field factory; a
        // 6th part reaches getField(5), which throws — matching the original.
        for (position, part) in parts.iter().enumerate() {
            match FieldKind::from_position(position) {
                Some(kind) => {
                    if !kind.validate(part) {
                        return Err(CronError::InvalidField(part.clone(), position));
                    }
                }
                None => return Err(CronError::BadPosition(position)),
            }
        }

        Ok(Self { parts })
    }

    /// `CronExpression::isValidExpression()`.
    pub fn is_valid_expression(expr: &str) -> bool {
        Self::new(expr).is_ok()
    }

    /// `getNextRunDate($now)` with `$nth = 0`, `$allowCurrentDate = false`;
    /// the result keeps the timezone of `now`.
    pub fn next_run_date<Tz: TimeZone>(&self, now: chrono::DateTime<Tz>) -> Result<DateTime<Tz>, CronError> {
        self.run_date(&now, false, false)
    }

    // ------------------------------------------------------------------
    // getRunDate()
    // ------------------------------------------------------------------

    fn run_date<Tz: TimeZone>(
        &self,
        now: &DateTime<Tz>,
        invert: bool,
        allow_current: bool,
    ) -> Result<DateTime<Tz>, CronError> {
        let tz = now.timezone();

        // Normalize: seconds dropped while keeping the offset observed at the
        // start instant (DateTime::createFromFormat("!Y-m-d H:iO", ...) trick).
        let wall = now.naive_local();
        let start_wall = NaiveDate::from_ymd_opt(wall.year(), wall.month(), wall.day())
            .and_then(|d| d.and_hms_opt(wall.hour(), wall.minute(), 0))
            .ok_or_else(|| CronError::Runtime("invalid start date".into()))?;
        let off_secs = now.offset().fix().local_minus_utc() as i64;
        let start_utc = start_wall
            .checked_sub_signed(chrono::Duration::seconds(off_secs))
            .ok_or_else(|| CronError::Runtime("start date out of range".into()))?;
        let current = tz.from_utc_datetime(&start_utc);

        // Fields to satisfy, in YEAR/MONTH/DAY/WEEKDAY/HOUR/MINUTE order,
        // skipping '*' and absent parts.
        let mut active: Vec<(usize, &String)> = Vec::new();
        for &position in &[5usize, 3, 2, 4, 1, 0] {
            if let Some(part) = self.parts.get(position) {
                if part != "*" {
                    active.push((position, part));
                }
            }
        }

        let day_set = active.iter().any(|&(p, _)| p == 2);
        let weekday_set = active.iter().any(|&(p, _)| p == 4);
        if day_set && weekday_set {
            return self.run_combined(&current, invert, allow_current);
        }

        let mut next_run = current.clone();
        let mut nth: i64 = 0;

        for _ in 0..MAX_ITERATION_COUNT {
            let mut satisfied_all = true;
            for &(position, part) in &active {
                let kind = FieldKind::from_position(position).ok_or_else(|| CronError::BadPosition(position))?;
                let mut satisfied = false;
                if !part.contains(',') {
                    satisfied = kind.is_satisfied_by(&next_run, part, invert)?;
                } else {
                    for list_part in part.split(',') {
                        if kind.is_satisfied_by(&next_run, list_part.trim(), invert)? {
                            satisfied = true;
                            break;
                        }
                    }
                }
                if !satisfied {
                    kind.increment(&mut next_run, invert, Some(part))?;
                    satisfied_all = false;
                    break;
                }
            }

            if !satisfied_all {
                continue;
            }

            // Skip this match if needed
            let equal_current = next_run.naive_local() == current.naive_local();
            let skip = if !allow_current && equal_current {
                true
            } else {
                nth -= 1;
                nth > -1
            };
            if skip {
                let minute_part = active.iter().find(|&&(p, _)| p == 0).map(|&(_, part)| part.as_str());
                FieldKind::Minute.increment(&mut next_run, invert, minute_part)?;
                continue;
            }

            return Ok(next_run);
        }

        Err(CronError::Impossible)
    }

    /// The DAY+WEEKDAY disjunction branch of `getRunDate()`.
    fn run_combined<Tz: TimeZone>(
        &self,
        current: &DateTime<Tz>,
        invert: bool,
        allow_current: bool,
    ) -> Result<DateTime<Tz>, CronError> {
        let dom_expr = format!("{} {} {} {} *", self.parts[0], self.parts[1], self.parts[2], self.parts[3]);
        let dow_expr = format!("{} {} * {} {}", self.parts[0], self.parts[1], self.parts[3], self.parts[4]);

        let dom = CronExpression::new(&dom_expr)?;
        let dow = CronExpression::new(&dow_expr)?;

        // total = nth + 1 with nth always 0 here.
        let mut dom_dates = dom.multiple_run_dates(current, invert, allow_current)?;
        let mut dow_dates = dow.multiple_run_dates(current, invert, allow_current)?;

        if self.parts[2] == "?" || self.parts[2] == "*" {
            dom_dates.clear();
        }
        if self.parts[4] == "?" || self.parts[4] == "*" {
            dow_dates.clear();
        }

        let mut combined: Vec<DateTime<Tz>> = dom_dates;
        combined.extend(dow_dates);

        // usort comparing formatted "Y-m-d H:i:s" strings.
        combined.sort_by_key(|dt| {
            let n = dt.naive_local();
            (n.year(), n.month(), n.day(), n.hour(), n.minute(), n.second())
        });
        if invert {
            combined.reverse();
        }

        combined.into_iter().next().ok_or_else(|| CronError::Runtime("no matching run dates".into()))
    }

    /// `getMultipleRunDates()` restricted to what `run_combined` needs.
    fn multiple_run_dates<Tz: TimeZone>(
        &self,
        current: &DateTime<Tz>,
        invert: bool,
        mut allow_current: bool,
    ) -> Result<Vec<DateTime<Tz>>, CronError> {
        let mut matches = Vec::new();
        let mut current_time = current.clone();
        // total = nth + 1 = 1
        for _ in 0..1 {
            match self.run_date(&current_time, invert, allow_current) {
                Ok(result) => {
                    allow_current = false;
                    current_time = result.clone();
                    matches.push(result);
                }
                Err(CronError::Impossible) => break,
                Err(e) => return Err(e),
            }
        }
        Ok(matches)
    }
}
