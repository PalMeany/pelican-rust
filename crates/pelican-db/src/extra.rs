//! Columns a database has beyond the schema this build knows: plugins' migrations add them to
//! core tables (the subdomains plugin's `nodes.srv_target`). The original serializes whole rows
//! (`$model->toArray()`), so such columns appear in its output; [`refresh`] reads their values
//! and [`values`] hands them to the code that builds those outputs.

use std::collections::HashMap;
use std::sync::Mutex;

use sea_orm::{ConnectionTrait, DbErr, QueryResult};
use serde_json::Value;

use crate::Driver;

type Rows = HashMap<i64, Vec<(String, Value)>>;

/// Extra columns per table (checked once per process) and their values per row id.
static COLUMNS: Mutex<Option<HashMap<String, Vec<String>>>> = Mutex::new(None);
static VALUES: Mutex<Option<HashMap<String, Rows>>> = Mutex::new(None);

/// Columns of `table` in the live database that the schema of this build does not have, in
/// their physical order.
pub async fn columns<C: ConnectionTrait>(db: &C, driver: Driver, table: &str) -> Result<Vec<String>, DbErr> {
    if let Some(cols) = COLUMNS.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(table).cloned())) {
        return Ok(cols);
    }
    let known = crate::column_order::columns(driver, table).unwrap_or(&[]);
    let backend = db.get_database_backend();
    let sql = match driver {
        Driver::Sqlite => format!("select name as column_name from pragma_table_info('{table}') order by cid"),
        Driver::Pgsql => format!(
            "select column_name::text as column_name from information_schema.columns where table_schema = current_schema() \
             and table_name = '{table}' order by ordinal_position"
        ),
        Driver::Mysql | Driver::Mariadb => format!(
            "select column_name as column_name from information_schema.columns where table_schema = database() \
             and table_name = '{table}' order by ordinal_position"
        ),
    };
    let rows = db.query_all_raw(sea_orm::Statement::from_string(backend, sql)).await?;
    let extra: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<String>("", "column_name").ok())
        .filter(|c| !known.is_empty() && !known.contains(&c.as_str()))
        .collect();
    if let Ok(mut g) = COLUMNS.lock() {
        g.get_or_insert_with(Default::default).insert(table.to_string(), extra.clone());
    }
    Ok(extra)
}

/// Read the extra columns' values of every row of `table` (small core tables: nodes).
pub async fn refresh<C: ConnectionTrait>(db: &C, driver: Driver, table: &str) -> Result<(), DbErr> {
    let cols = columns(db, driver, table).await?;
    if cols.is_empty() {
        return Ok(());
    }
    let q = |c: &str| match driver {
        Driver::Mysql | Driver::Mariadb => format!("`{c}`"),
        _ => format!("\"{c}\""),
    };
    let list = cols.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ");
    let sql = format!("select {}, {list} from {}", q("id"), q(table));
    let rows = db.query_all_raw(sea_orm::Statement::from_string(db.get_database_backend(), sql)).await?;
    let mut out = Rows::new();
    for r in &rows {
        let Ok(id) = r.try_get::<i64>("", "id") else { continue };
        out.insert(id, cols.iter().map(|c| (c.clone(), value(r, c))).collect());
    }
    if let Ok(mut g) = VALUES.lock() {
        g.get_or_insert_with(Default::default).insert(table.to_string(), out);
    }
    Ok(())
}

/// The extra columns of row `id` of `table` as last read by [`refresh`].
pub fn values(table: &str, id: i64) -> Vec<(String, Value)> {
    VALUES.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(table)?.get(&id).cloned())).unwrap_or_default()
}

/// A column value as PDO returns it: integers, floats, strings, null.
fn value(r: &QueryResult, c: &str) -> Value {
    if let Ok(Some(v)) = r.try_get::<Option<i64>>("", c) {
        return Value::from(v);
    }
    if let Ok(Some(v)) = r.try_get::<Option<f64>>("", c) {
        return Value::from(v);
    }
    if let Ok(Some(v)) = r.try_get::<Option<String>>("", c) {
        return Value::from(v);
    }
    Value::Null
}
