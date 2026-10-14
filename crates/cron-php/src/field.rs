//! CRON fields mirroring `AbstractField` and its five subclasses from
//! dragonmantank/cron-expression (`MinutesField`, `HoursField`,
//! `DayOfMonthField`, `MonthField`, `DayOfWeekField`).

use chrono::{DateTime, Datelike, Days, NaiveDate, NaiveTime, TimeZone, Timelike};
use std::cmp::Ordering;

use crate::CronError;
use crate::php;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Minute,
    Hour,
    DayOfMonth,
    Month,
    DayOfWeek,
}

impl FieldKind {
    pub fn from_position(position: usize) -> Option<Self> {
        match position {
            0 => Some(Self::Minute),
            1 => Some(Self::Hour),
            2 => Some(Self::DayOfMonth),
            3 => Some(Self::Month),
            4 => Some(Self::DayOfWeek),
            _ => None,
        }
    }

    /// `(rangeStart, rangeEnd)` of each field.
    fn range(self) -> (i64, i64) {
        match self {
            Self::Minute => (0, 59),
            Self::Hour => (0, 23),
            Self::DayOfMonth => (1, 31),
            Self::Month => (1, 12),
            Self::DayOfWeek => (0, 7),
        }
    }

    /// Literal names indexed by their key (1-based) as in the original classes.
    fn literals(self) -> &'static [&'static str] {
        match self {
            Self::Month => &["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"],
            Self::DayOfWeek => &["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"],
            _ => &[],
        }
    }

    /// `convertLiterals()`: uppercase exact match against the literal list.
    fn convert_literal(self, value: &str) -> String {
        let lits = self.literals();
        if lits.is_empty() {
            return value.to_string();
        }
        let up = value.to_uppercase();
        match lits.iter().position(|l| *l == up) {
            Some(idx) => (idx + 1).to_string(),
            None => value.to_string(),
        }
    }

    // ------------------------------------------------------------------
    // Validation
    // ------------------------------------------------------------------

    /// Field-aware `validate()` including the L/W/#/? overlays.
    pub fn validate(self, value: &str) -> bool {
        let basic = self.validate_common(value);
        match self {
            Self::Minute | Self::Hour | Self::Month => basic,
            Self::DayOfMonth => {
                // A list may not contain W or L
                if value.contains(',') && (value.contains('W') || value.contains('L')) {
                    return false;
                }
                if basic {
                    return true;
                }
                if value == "?" || value == "L" {
                    return true;
                }
                // /^(.*)W$/ — greedy capture is everything before the final W
                match value.strip_suffix('W') {
                    Some(prefix) => self.validate(prefix),
                    None => false,
                }
            }
            Self::DayOfWeek => {
                if basic {
                    return true;
                }
                if value == "?" {
                    return true;
                }
                if let Some((head, tail)) = value.split_once('#') {
                    // explode('#'): $chunks[1] is the second element only
                    let nth = tail.split('#').next().unwrap_or("");
                    let head_conv = self.convert_literal(head);
                    if self.validate_common(&head_conv)
                        && php::php_is_numeric(nth)
                        && (1..=5).contains(&php::php_int(nth))
                    {
                        return true;
                    }
                }
                match value.strip_suffix('L') {
                    Some(prefix) => self.validate(prefix),
                    None => false,
                }
            }
        }
    }

    /// `AbstractField::validate()` verbatim.
    fn validate_common(self, value: &str) -> bool {
        let conv = self.convert_literal(value);

        if conv == "*" {
            return true;
        }

        if conv.contains(',') {
            return conv.split(',').all(|item| self.validate(item));
        }

        if conv.contains('/') {
            let mut it = conv.splitn(2, '/');
            let range = it.next().unwrap_or("");
            let step = match it.next() {
                Some(s) => s,
                // explode('/', $v)[1] missing cannot happen once '/' exists,
                // but keep the guard.
                None => return false,
            };
            // More than one slash makes $step an array and filter_var fails.
            if step.contains('/') {
                return false;
            }
            // Don't allow numeric ranges before the slash.
            if php::php_is_numeric(range) {
                return false;
            }
            return self.validate(range) && matches!(php::php_filter_int(step), Some(v) if v != 0);
        }

        if conv.contains('-') {
            if conv.matches('-').count() > 1 {
                return false;
            }
            let mut it = conv.splitn(2, '-');
            let a = self.convert_literal(it.next().unwrap_or(""));
            let b = self.convert_literal(it.next().unwrap_or(""));
            if a == "*" || b == "*" {
                return false;
            }
            return self.validate(&a) && self.validate(&b);
        }

        if !php::php_is_numeric(&conv) {
            return false;
        }
        if conv.contains('.') {
            return false;
        }
        let v = php::php_int(&conv);
        let (lo, hi) = self.range();
        v >= lo && v <= hi
    }

    // ------------------------------------------------------------------
    // Satisfaction checks
    // ------------------------------------------------------------------

    pub fn is_satisfied_by<Tz: TimeZone>(
        self,
        date: &DateTime<Tz>,
        value: &str,
        invert: bool,
    ) -> Result<bool, CronError> {
        match self {
            Self::Minute => {
                if value == "?" {
                    return Ok(true);
                }
                self.is_satisfied(date.minute() as i64, value)
            }
            Self::Hour => self.hour_satisfied(date, value, invert),
            Self::DayOfMonth => self.dom_satisfied(date, value),
            Self::Month => {
                if value == "?" {
                    return Ok(true);
                }
                let conv = self.convert_literal(value);
                self.is_satisfied(date.month() as i64, &conv)
            }
            Self::DayOfWeek => self.dow_satisfied(date, value),
        }
    }

    /// `AbstractField::isSatisfied()`.
    fn is_satisfied(self, date_value: i64, value: &str) -> Result<bool, CronError> {
        if value.contains('/') {
            return self.in_increments_of_ranges(date_value, value);
        }
        if value.contains('-') {
            return self.in_range(date_value, value);
        }
        Ok("*" == value || date_value == php::php_int(value))
    }

    /// `isInRange()`.
    fn in_range(self, date_value: i64, value: &str) -> Result<bool, CronError> {
        let mut it = value.splitn(2, '-');
        let lo_raw = it.next().unwrap_or("");
        let hi_raw = it.next().unwrap_or("");
        let lo = self.convert_literal(lo_raw.trim());
        let hi = self.convert_literal(hi_raw.trim());
        Ok(php::int_ge_str(date_value, &lo) && php::int_le_str(date_value, &hi))
    }

    /// `isInIncrementsOfRanges()`.
    fn in_increments_of_ranges(self, date_value: i64, value: &str) -> Result<bool, CronError> {
        let mut chunks = value.splitn(2, '/');
        let range = chunks.next().unwrap_or("").trim();
        let step = match chunks.next() {
            Some(s) => s.trim(),
            None => "",
        };
        // No step or 0 steps aren't cool
        if step.is_empty() || step == "0" || php::php_int(step) == 0 {
            return Ok(false);
        }

        let (f_lo, f_hi) = self.range();
        let range = if range == "*" { format!("{}-{}", f_lo, f_hi) } else { range.to_string() };

        let mut rit = range.splitn(2, '-');
        let range_start = php::php_int(rit.next().unwrap_or(""));
        let range_end = match rit.next() {
            Some(s) => php::php_int(s),
            None => range_start,
        };

        if range_start < f_lo || range_start > f_hi || range_start > range_end {
            return Err(CronError::Runtime("Invalid range start requested".into()));
        }
        if range_end < f_lo || range_end > f_hi || range_end < range_start {
            return Err(CronError::Runtime("Invalid range end requested".into()));
        }

        let step_n = php::php_int(step);
        let members: Vec<i64> = if step_n > f_hi {
            vec![f_lo + step_n % (f_hi - f_lo + 1)]
        } else if step_n > (range_end - range_start) {
            vec![range_start]
        } else {
            let mut v = Vec::new();
            let mut x = range_start;
            while x <= range_end {
                v.push(x);
                x += step_n;
            }
            v
        };
        Ok(members.contains(&date_value))
    }

    /// `getRangeForExpression()` returning numeric values (the expansion is
    /// only used by minute/hour fields whose parts are always numeric).
    pub fn get_range_for_expression(self, expression: &str, max: i64) -> Result<Vec<i64>, CronError> {
        let expression = self.convert_literal(expression);
        let (_, f_hi) = self.range();

        if expression.contains(',') {
            // The original merges sub-expansions and returns WITHOUT sorting.
            let mut values = Vec::new();
            for r in expression.split(',') {
                values.extend(self.get_range_for_expression(r, f_hi)?);
            }
            return Ok(values);
        }

        let mut values: Vec<i64> = Vec::new();
        if expression.contains('-') || expression.contains('/') {
            if !expression.contains('/') {
                // plain range: offset-to, step 1
                let mut it = expression.splitn(2, '-');
                let o = self.convert_literal(it.next().unwrap_or(""));
                let t = self.convert_literal(it.next().unwrap_or(""));
                let oi = php::php_int(&o);
                let ti = php::php_int(&t);
                let mut x = oi;
                while x <= ti {
                    values.push(x);
                    x += 1;
                }
            } else {
                let mut sit = expression.splitn(2, '/');
                let head = sit.next().unwrap_or("");
                let step_size = php::php_int(sit.next().unwrap_or("0"));
                let mut hit = head.splitn(2, '-');
                let o_raw = hit.next().unwrap_or("").trim();
                let to = match hit.next() {
                    Some(s) => s.trim().to_string(),
                    None => max.to_string(),
                };
                let o = if o_raw == "*" { self.range().0.to_string() } else { o_raw.to_string() };
                if step_size >= f_hi {
                    values.push(f_lo_of(self) + step_size % (f_hi - f_lo_of(self) + 1));
                } else {
                    let oi = php::php_int(&o);
                    let ti = php::php_int(&to);
                    let mut x = oi;
                    while x <= ti {
                        values.push(x);
                        x += step_size;
                    }
                }
            }
        } else {
            values.push(php::php_int(&expression));
        }
        values.sort_unstable();
        Ok(values)
    }

    // ------------------------------------------------------------------
    // Per-field satisfaction
    // ------------------------------------------------------------------

    /// `HoursField::isSatisfiedBy()` including the DST edge handling.
    fn hour_satisfied<Tz: TimeZone>(self, date: &DateTime<Tz>, value: &str, invert: bool) -> Result<bool, CronError> {
        let check_value = date.hour() as i64;
        let retval = self.is_satisfied(check_value, value)?;
        if retval {
            return Ok(retval);
        }

        // Are we on the edge of a transition? The original asks the timezone
        // for transitions within the last hour; comparing offsets reproduces
        // that exactly (offset change at/inside the last hour).
        let ts = date.timestamp();
        let off_now = php::offset_at(&date.timezone(), ts);
        let off_prev = php::offset_at(&date.timezone(), ts - 3600);
        let off_prev_before = php::offset_at(&date.timezone(), ts - 3601);
        if off_now != off_prev && off_prev == off_prev_before {
            let last_offset = off_prev;
            let next_offset = php::offset_at(&date.timezone(), ts + 3600);
            let offset_change = next_offset - last_offset;
            if offset_change >= 3600 {
                return self.is_satisfied(check_value - 1, value);
            }
            if !invert && offset_change <= -3600 {
                return self.is_satisfied(check_value + 1, value);
            }
        }
        Ok(retval)
    }

    /// `DayOfMonthField::isSatisfiedBy()`.
    fn dom_satisfied<Tz: TimeZone>(self, date: &DateTime<Tz>, value: &str) -> Result<bool, CronError> {
        if value == "?" {
            return Ok(true);
        }
        if value == "L" {
            let last = php::dim(date.year() as i64, date.month());
            return Ok(date.day() as i64 == last);
        }
        // strpos($value, 'W') truthiness: position 0 falls through
        if let Some(pos) = value.find('W') {
            if pos > 0 {
                let target_day = php::php_int(&value[..pos]);
                let nearest = get_nearest_weekday(date.year(), date.month(), target_day)
                    .ok_or_else(|| CronError::Runtime("Unable to return nearest weekday".into()))?;
                return Ok(date.day() == nearest.day());
            }
        }
        self.is_satisfied(date.day() as i64, value)
    }

    /// `DayOfWeekField::isSatisfiedBy()`.
    fn dow_satisfied<Tz: TimeZone>(self, date: &DateTime<Tz>, value: &str) -> Result<bool, CronError> {
        if value == "?" {
            return Ok(true);
        }
        let value = self.convert_literal(value);

        let current_year = date.year();
        let current_month = date.month();
        let last_day_of_month = php::dim(current_year as i64, current_month);

        // strpos($value,'L') truthiness: leading L falls through
        if let Some(pos) = value.find('L') {
            if pos > 0 {
                let weekday = php::php_int(&self.convert_literal(&value[..pos])) % 7;
                let remaining_days_in_month = last_day_of_month - date.day() as i64;
                return Ok(weekday == date.weekday().num_days_from_sunday() as i64 && remaining_days_in_month < 7);
            }
        }

        // Hash tokens
        if let Some(pos) = value.find('#') {
            if pos > 0 {
                let mut parts = value.splitn(2, '#');
                let weekday_raw = parts.next().unwrap_or("");
                // explode('#'): $nth is only the second element
                let nth_raw = parts.next().unwrap_or("").split('#').next().unwrap_or("");
                if !php::php_is_numeric(nth_raw) {
                    return Err(CronError::Runtime(format!("Hashed weekdays must be numeric, {nth_raw} given")));
                }
                let nth = php::php_int(nth_raw);
                let weekday_raw = if weekday_raw == "0" { "7" } else { weekday_raw };
                let weekday = php::php_int(&self.convert_literal(weekday_raw));
                if !(0..=7).contains(&weekday) {
                    return Err(CronError::Runtime(format!(
                        "Weekday must be a value between 0 and 7. {weekday} given"
                    )));
                }
                if !(1..=5).contains(&nth) {
                    return Err(CronError::Runtime(format!(
                        "There are never more than 5 or less than 1 of a given weekday in a month, {nth} given"
                    )));
                }

                let iso = date.weekday().number_from_monday() as i64;
                if iso != weekday {
                    return Ok(false);
                }
                // Count occurrences of this weekday within the month
                let first = NaiveDate::from_ymd_opt(current_year, current_month, 1)
                    .ok_or_else(|| CronError::Runtime("invalid month".into()))?;
                let mut day_count: i64 = 0;
                let mut current_day: i64 = 1;
                while current_day < last_day_of_month + 1 {
                    let d = first
                        .checked_add_days(Days::new((current_day - 1) as u64))
                        .ok_or_else(|| CronError::Runtime("invalid day".into()))?;
                    if d.weekday().number_from_monday() as i64 == weekday {
                        day_count += 1;
                        if day_count >= nth {
                            break;
                        }
                    }
                    current_day += 1;
                }
                return Ok(date.day() as i64 == current_day);
            }
        }

        // Range handling: 0/7 both mean Sunday; normalize so ranges align
        // with either the ISO ('N') or Sunday-first ('w') numbering.
        let normalized = if value.contains('-') {
            let parts: Vec<&str> = value.splitn(2, '-').collect();
            let mut p0 = parts[0].to_string();
            let mut p1 = parts.get(1).copied().unwrap_or("").to_string();
            if p0 == "7" {
                p0 = "0".into();
            } else if p1 == "0" {
                p1 = "7".into();
            }
            format!("{}-{}", p0, p1)
        } else {
            value.clone()
        };

        let uses_iso = normalized.chars().any(|c| c.is_ascii_digit() && c == '7');
        let field_value = if uses_iso {
            date.weekday().number_from_monday() as i64
        } else {
            date.weekday().num_days_from_sunday() as i64
        };
        self.is_satisfied(field_value, &normalized)
    }

    // ------------------------------------------------------------------
    // Increments
    // ------------------------------------------------------------------

    pub fn increment<Tz: TimeZone>(
        self,
        date: &mut DateTime<Tz>,
        invert: bool,
        parts: Option<&str>,
    ) -> Result<(), CronError> {
        match self {
            Self::Minute => minute_increment(date, invert, parts, self),
            Self::Hour => hour_increment(date, invert, parts, self),
            Self::DayOfMonth | Self::DayOfWeek => dom_dow_increment(date, invert),
            Self::Month => month_increment(date, invert),
        }
    }
}

fn f_lo_of(kind: FieldKind) -> i64 {
    kind.range().0
}

/// Expand a comma-separated part into concrete integer values for increments
/// (minute/hour fields). Parts are sorted like PHP's `sort()` first.
fn expand_list(field: FieldKind, parts: &str, max: i64) -> Result<Vec<i64>, CronError> {
    let mut list: Vec<&str> = if parts.contains(',') { parts.split(',').collect() } else { vec![parts] };
    sort_parts_php(&mut list);
    let mut out = Vec::new();
    for part in list {
        out.extend(field.get_range_for_expression(part, max)?);
    }
    Ok(out)
}

fn sort_parts_php(list: &mut [&str]) {
    list.sort_by(|a, b| {
        if php::php_is_numeric(a) && php::php_is_numeric(b) {
            let fa = a.trim().parse::<f64>().unwrap_or(f64::MAX);
            let fb = b.trim().parse::<f64>().unwrap_or(f64::MAX);
            fa.partial_cmp(&fb).unwrap_or(Ordering::Equal)
        } else {
            a.cmp(b)
        }
    });
}

/// Position scan shared by minute/hour increments.
fn find_position(values: &[i64], current: i64, invert: bool) -> usize {
    let n = values.len();
    let mut position = if invert { n.saturating_sub(1) } else { 0 };
    if n > 1 {
        for i in 0..n - 1 {
            if (!invert && current >= values[i] && current < values[i + 1])
                || (invert && current > values[i] && current <= values[i + 1])
            {
                position = if invert { i } else { i + 1 };
                break;
            }
        }
    }
    position
}

fn minute_increment<Tz: TimeZone>(
    date: &mut DateTime<Tz>,
    invert: bool,
    parts: Option<&str>,
    kind: FieldKind,
) -> Result<(), CronError> {
    let parts = match parts {
        Some(p) => p,
        None => {
            php::shift_seconds(date, if invert { -60 } else { 60 })?;
            return Ok(());
        }
    };

    let current_minute = date.naive_local().minute() as i64;
    let minutes = expand_list(kind, parts, 59)?;

    let position = find_position(&minutes, current_minute, invert);
    let target = minutes[position];
    let mut original_minute = current_minute;

    if !invert {
        if original_minute >= target {
            let distance = 60 - original_minute;
            php::shift_seconds(date, distance * 60)?;
            original_minute = date.naive_local().minute() as i64;
        }
        let distance = target - original_minute;
        php::shift_seconds(date, distance * 60)?;
    } else {
        if original_minute <= target {
            let distance = original_minute + 1;
            php::shift_seconds(date, -distance * 60)?;
            original_minute = date.naive_local().minute() as i64;
        }
        let distance = original_minute - target;
        php::shift_seconds(date, -distance * 60)?;
    }
    Ok(())
}

/// `AbstractField::setTimeHour()`.
fn set_time_hour<Tz: TimeZone>(
    date: &mut DateTime<Tz>,
    invert: bool,
    original_timestamp: i64,
) -> Result<(), CronError> {
    let wall = date.naive_local();
    set_wall_time(date, wall.hour(), if invert { 59 } else { 0 }, wall.second())?;
    let actual_timestamp = date.timestamp();
    if !invert && actual_timestamp <= original_timestamp {
        php::shift_seconds(date, 3600)?;
    }
    if invert && actual_timestamp >= original_timestamp {
        php::shift_seconds(date, -3600)?;
    }
    Ok(())
}

/// Set local time h:m keeping the wall date and seconds.
pub(crate) fn set_wall_time<Tz: TimeZone>(dt: &mut DateTime<Tz>, h: u32, m: u32, s: u32) -> Result<(), CronError> {
    let wall = dt.naive_local();
    let date = NaiveDate::from_ymd_opt(wall.year(), wall.month(), wall.day())
        .ok_or_else(|| CronError::Runtime("invalid wall date".into()))?;
    let time = NaiveTime::from_hms_opt(h, m, s).ok_or_else(|| CronError::Runtime("invalid time".into()))?;
    *dt = php::php_resolve(&dt.timezone(), chrono::NaiveDateTime::new(date, time))?;
    Ok(())
}

fn hour_increment<Tz: TimeZone>(
    date: &mut DateTime<Tz>,
    invert: bool,
    parts: Option<&str>,
    kind: FieldKind,
) -> Result<(), CronError> {
    let original_timestamp = date.timestamp();

    match parts {
        None | Some("*") => {
            php::shift_seconds(date, if invert { -3600 } else { 3600 })?;
            set_time_hour(date, invert, original_timestamp)?;
            return Ok(());
        }
        _ => {}
    }
    let parts = parts.unwrap_or("");

    // explode(',') then merge expansions — NOT sorted in the original.
    let split: Vec<&str> = if parts.contains(',') { parts.split(',').collect() } else { vec![parts] };
    let mut hours: Vec<i64> = Vec::new();
    for part in split {
        hours.extend(kind.get_range_for_expression(part, 23)?);
    }

    let current_hour = date.naive_local().hour() as i64;
    let position = find_position(&hours, current_hour, invert);
    let target = hours[position];
    let mut original_hour = current_hour;
    let tz = date.timezone();
    let original_dst = php::dst_flag(&tz, original_timestamp);

    let original_day = date.naive_local().day() as i64;
    let previous_offset = php::offset_at(&tz, original_timestamp);

    if !invert {
        if original_hour >= target {
            let distance = 24 - original_hour;
            php::shift_seconds(date, distance * 3600)?;

            let actual_day = date.naive_local().day() as i64;
            let actual_hour = date.naive_local().hour() as i64;
            if actual_day != original_day + 1 && actual_hour != 0 {
                let offset_change = previous_offset - php::offset_at(&tz, date.timestamp());
                php::shift_seconds(date, offset_change as i64)?;
            }
            original_hour = date.naive_local().hour() as i64;
        }
        let distance = target - original_hour;
        php::shift_seconds(date, distance * 3600)?;
    } else {
        if original_hour <= target {
            let distance = original_hour + 1;
            php::shift_seconds(date, -(distance * 3600))?;

            let actual_day = date.naive_local().day() as i64;
            let actual_hour = date.naive_local().hour() as i64;
            if actual_day != original_day - 1 && actual_hour != 23 {
                let offset_change = previous_offset - php::offset_at(&tz, date.timestamp());
                php::shift_seconds(date, offset_change as i64)?;
            }
            original_hour = date.naive_local().hour() as i64;
        }
        let distance = original_hour - target;
        php::shift_seconds(date, -(distance * 3600))?;
    }

    let actual_dst = php::dst_flag(&tz, date.timestamp());
    if original_dst < actual_dst {
        php::shift_seconds(date, -3600)?;
    }

    set_time_hour(date, invert, original_timestamp)?;

    let actual_hour = date.naive_local().hour() as i64;
    if invert && (actual_hour == target - 1 || ((actual_hour == 23) && (target == 0))) {
        php::shift_seconds(date, 3600)?;
    }
    Ok(())
}

fn dom_dow_increment<Tz: TimeZone>(date: &mut DateTime<Tz>, invert: bool) -> Result<(), CronError> {
    if !invert {
        php::wall_shift_days(date, 1)?;
        let sec = date.naive_local().second();
        set_wall_time(date, 0, 0, sec)?;
    } else {
        php::wall_shift_days(date, -1)?;
        let sec = date.naive_local().second();
        set_wall_time(date, 23, 59, sec)?;
    }
    Ok(())
}

fn month_increment<Tz: TimeZone>(date: &mut DateTime<Tz>, invert: bool) -> Result<(), CronError> {
    if !invert {
        php::month_boundary_wall(date, false, 0, 0)?;
    } else {
        php::month_boundary_wall(date, true, 23, 59)?;
    }
    Ok(())
}

/// `DayOfMonthField::getNearestWeekday()`; `Y-m-d` construction rolls
/// overflowing days into following months like `createFromFormat` does.
fn get_nearest_weekday(current_year: i32, current_month: u32, target_day: i64) -> Option<NaiveDate> {
    let base = NaiveDate::from_ymd_opt(current_year, current_month, 1)?;
    let target = shift_from_first(base, target_day)?;

    let current_weekday = target.weekday().number_from_monday() as i64;
    if current_weekday < 6 {
        return Some(target);
    }

    let last_day_of_month = target.num_days_in_month() as i64;
    for i in [-1i64, 1, -2, 2] {
        let adjusted = target_day + i;
        if adjusted > 0 && adjusted <= last_day_of_month {
            if let Some(candidate) = shift_from_first(base, adjusted) {
                if (candidate.weekday().number_from_monday() as i64) < 6 && candidate.month() == current_month {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn shift_from_first(base: NaiveDate, day: i64) -> Option<NaiveDate> {
    if day >= 1 {
        base.checked_add_days(Days::new((day - 1) as u64))
    } else {
        base.checked_sub_days(Days::new((1 - day) as u64))
    }
}
