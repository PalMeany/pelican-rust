//! "Now" as Laravel stores it: app timezone (always UTC), second precision.

use chrono::{NaiveDateTime, Timelike, Utc};

pub fn now() -> NaiveDateTime {
    let n = Utc::now().naive_utc();
    n.with_nanosecond(0).unwrap_or(n)
}

/// Carbon `toAtomString()` / `DATE_ATOM`: `2026-10-14T12:00:00+00:00`.
pub fn atom(t: &NaiveDateTime) -> String {
    t.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
}

/// Laravel-data default date format for `CarbonImmutable` with microseconds:
/// `2026-10-14T12:00:00.000000Z` (spec: egg/server variables).
pub fn iso_micro_z(t: &NaiveDateTime) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
}
