//! Raw SQL in the exact shape Eloquent generates. Several original queries
//! have no ORDER BY, so row order is the database's natural order for that
//! plan; issuing the same SQL keeps the order identical (ADR-0011).

use sea_orm::{DbBackend, Statement, Value};

/// Quote an identifier like Laravel's grammar (`a.b` → `` `a`.`b` `` / `"a"."b"`).
pub fn q(backend: DbBackend, ident: &str) -> String {
    let quote = |s: &str| {
        if s == "*" {
            return s.to_string();
        }
        match backend {
            DbBackend::MySql => format!("`{s}`"),
            _ => format!("\"{s}\""),
        }
    };
    ident.split('.').map(quote).collect::<Vec<_>>().join(".")
}

/// Build a statement from SQL using `?` placeholders and `{name}` identifier
/// markers (`{egg_variables.id}` → quoted).
pub fn stmt(backend: DbBackend, sql: &str, values: Vec<Value>) -> Statement {
    let mut out = String::with_capacity(sql.len() + 16);
    let mut n = 0;
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '?' => {
                n += 1;
                if backend == DbBackend::Postgres {
                    out.push_str(&format!("${n}"));
                } else {
                    out.push('?');
                }
            }
            '{' => {
                let mut ident = String::new();
                for d in chars.by_ref() {
                    if d == '}' {
                        break;
                    }
                    ident.push(d);
                }
                out.push_str(&q(backend, &ident));
            }
            _ => out.push(c),
        }
    }
    Statement::from_sql_and_values(backend, out, values)
}
