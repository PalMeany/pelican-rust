//! Reference vectors generated with the original PHP library
//! (`CronExpression::getNextRunDate($nowInTz)` converted to UTC, `isValidExpression`).

use chrono::{NaiveDateTime, TimeZone};
use cron_php::CronExpression;

#[test]
fn matches_php_reference() {
    let data: serde_json::Value = serde_json::from_str(include_str!("vectors.json")).unwrap();
    let mut failures = Vec::new();
    for v in data["valid"].as_array().unwrap() {
        let expr = v["expr"].as_str().unwrap();
        let base = NaiveDateTime::parse_from_str(v["base_utc"].as_str().unwrap(), "%Y-%m-%d %H:%M:%S").unwrap();
        let tz: chrono_tz::Tz = v["tz"].as_str().unwrap().parse().unwrap();
        let want = v["next_utc"].as_str().unwrap();
        let now = chrono::Utc.from_utc_datetime(&base).with_timezone(&tz);
        let got = match CronExpression::new(expr).and_then(|c| c.next_run_date(now)) {
            Ok(d) => d.with_timezone(&chrono::Utc).format("%Y-%m-%d %H:%M:%S").to_string(),
            Err(_) => "ERROR".to_string(),
        };
        if got != want {
            failures.push(format!("{expr:?} base={base} tz={tz}: want {want}, got {got}"));
        }
    }
    for v in data["invalid"].as_array().unwrap() {
        let expr = v["expr"].as_str().unwrap();
        let want = v["valid"].as_bool().unwrap();
        if CronExpression::is_valid_expression(expr) != want {
            failures.push(format!("isValidExpression({expr:?}) want {want}"));
        }
    }
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
}
