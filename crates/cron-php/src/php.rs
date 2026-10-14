//! Small shims reproducing PHP (timelib) date/number semantics on top of chrono.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Offset, TimeZone, Timelike};

use crate::CronError;

/// PHP `(int)` cast on strings: leading whitespace, optional sign, decimal digits prefix.
pub fn php_int(s: &str) -> i64 {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
        i += 1;
    }
    let mut sign = 1i64;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        if b[i] == b'-' {
            sign = -1;
        }
        i += 1;
    }
    let mut val: i64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        val = val.saturating_mul(10).saturating_add((b[i] - b'0') as i64);
        i += 1;
    }
    sign * val
}

/// `is_numeric()` — integer or float literal with optional surrounding
/// " \t\n\r\v\f" whitespace.
pub fn php_is_numeric(s: &str) -> bool {
    let t = s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{0b}' | '\u{0c}'));
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let mut digits = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return false;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let mut exp = 0;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            exp += 1;
        }
        if exp == 0 {
            return false;
        }
    }
    i == b.len()
}

/// `filter_var($v, FILTER_VALIDATE_INT)` — strict whole-string integer.
pub fn php_filter_int(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    if i == b.len() {
        return None;
    }
    for &c in &b[i..] {
        if !c.is_ascii_digit() {
            return None;
        }
    }
    s.parse::<i64>().ok()
}

/// Ordering of an integer against a string per PHP 8 rules: numeric strings
/// compare numerically, non-numeric strings fall back to a string comparison
/// against the integer rendered as text.
pub fn cmp_int_str(a: i64, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if php_is_numeric(b) {
        match b.parse::<f64>() {
            Ok(f) => (a as f64).partial_cmp(&f).unwrap_or(Ordering::Equal),
            Err(_) => Ordering::Equal,
        }
    } else {
        a.to_string().as_str().cmp(b)
    }
}

pub fn int_ge_str(a: i64, b: &str) -> bool {
    cmp_int_str(a, b) != std::cmp::Ordering::Less
}

pub fn int_le_str(a: i64, b: &str) -> bool {
    cmp_int_str(a, b) != std::cmp::Ordering::Greater
}

/// Offset in seconds that applies to the given UTC timestamp.
pub fn offset_at<Tz: TimeZone>(tz: &Tz, ts: i64) -> i32 {
    match chrono::DateTime::from_timestamp(ts, 0) {
        Some(n) => tz.offset_from_utc_datetime(&n.naive_utc()).fix().local_minus_utc(),
        // Only reachable outside chrono's supported range; callers always pass
        // timestamps near a live DateTime so the sentinel never propagates.
        None => 0,
    }
}

/// Approximation of PHP's `format('I')` DST flag: 1 when the current offset is
/// above the minimum offset observed within ±180 days.
pub fn dst_flag<Tz: TimeZone>(tz: &Tz, ts: i64) -> u8 {
    let now_off = offset_at(tz, ts);
    let mut min = now_off;
    for k in -18i64..=18 {
        min = min.min(offset_at(tz, ts + k * 864_000));
    }
    u8::from(now_off > min)
}

/// Resolve a local wall-clock time to an instant like PHP does:
///
/// * unique mapping — as-is;
/// * ambiguous (fall-back overlap) — earliest occurrence;
/// * non-existent (spring-forward gap) — timelib guesses forward by one hour.
fn resolve_inner<Tz: TimeZone>(tz: &Tz, wall: NaiveDateTime, depth: u8) -> Result<DateTime<Tz>, CronError> {
    match tz.from_local_datetime(&wall) {
        chrono::LocalResult::Single(d) => Ok(d),
        chrono::LocalResult::Ambiguous(earliest, _) => Ok(earliest),
        chrono::LocalResult::None => {
            if depth == 0 {
                return Err(CronError::Runtime("unresolvable local time".into()));
            }
            match wall.checked_add_signed(Duration::hours(1)) {
                Some(shifted) => resolve_inner(tz, shifted, depth - 1),
                None => Err(CronError::Runtime("local time out of range".into())),
            }
        }
    }
}

/// Public wrapper for [`resolve_inner`].
pub fn php_resolve<Tz: TimeZone>(tz: &Tz, wall: NaiveDateTime) -> Result<DateTime<Tz>, CronError> {
    resolve_inner(tz, wall, 3)
}

/// Shift an instant by a signed number of seconds (`timezoneSafeModify` is a
/// pure duration shift in the original library).
pub fn shift_seconds<Tz: TimeZone>(dt: &mut DateTime<Tz>, secs: i64) -> Result<(), CronError> {
    let utc = dt.naive_utc();
    let shifted = utc
        .checked_add_signed(Duration::seconds(secs))
        .ok_or_else(|| CronError::Runtime("date out of range".into()))?;
    *dt = dt.timezone().from_utc_datetime(&shifted);
    Ok(())
}

/// Wall date arithmetic: add signed days to the local calendar date,
/// preserving the local time-of-day (PHP `modify('+N days')` semantics).
pub fn wall_shift_days<Tz: TimeZone>(dt: &mut DateTime<Tz>, days: i64) -> Result<(), CronError> {
    let wall = dt.naive_local();
    let date = NaiveDate::from_ymd_opt(wall.year(), wall.month(), wall.day())
        .ok_or_else(|| CronError::Runtime("invalid wall date".into()))?;
    let shifted = if days >= 0 {
        date.checked_add_days(chrono::Days::new(days as u64))
    } else {
        date.checked_sub_days(chrono::Days::new((-days) as u64))
    }
    .ok_or_else(|| CronError::Runtime("date out of range".into()))?;
    let naive = shifted.and_time(wall.time());
    *dt = php_resolve(&dt.timezone(), naive)?;
    Ok(())
}

/// Month-shifted first/last day construction used by MonthField::increment.
///
/// `want_first` picks day 1 of month+1, otherwise the last day of month-1;
/// the local time of day is preserved (then overridden by setTime in the caller).
pub fn month_boundary_wall<Tz: TimeZone>(dt: &mut DateTime<Tz>, invert: bool, h: u32, m: u32) -> Result<(), CronError> {
    let wall = dt.naive_local();
    let total = wall.year() as i64 * 12 + (wall.month() as i64 - 1) + if invert { -1 } else { 1 };
    let y = total.div_euclid(12);
    let mo = (total.rem_euclid(12)) as u32 + 1;
    let day = if invert { dim(y, mo) } else { 1 };
    let sec = wall.second();
    let date = NaiveDate::from_ymd_opt(y as i32, mo, day as u32)
        .ok_or_else(|| CronError::Runtime("invalid month boundary".into()))?;
    let time = NaiveTime::from_hms_opt(h, m, sec).ok_or_else(|| CronError::Runtime("invalid time".into()))?;
    *dt = php_resolve(&dt.timezone(), NaiveDateTime::new(date, time))?;
    Ok(())
}

/// Days in month, mirroring PHP's `format('t')`.
pub fn dim(y: i64, m: u32) -> i64 {
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}
