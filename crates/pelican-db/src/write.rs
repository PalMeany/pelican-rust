//! INSERT/UPDATE that store values exactly like Laravel (ADR-0004):
//! JSON is encoded with PHP `json_encode` flags 0 and bound as text — on
//! PostgreSQL wrapped in `CAST(? AS json)` so `json` columns keep the text
//! verbatim (sea-orm would bind `jsonb`, which reorders keys).

use sea_orm::sea_query::{Expr, ExprTrait, Query};
use sea_orm::{
    ActiveModelTrait, ActiveValue, ConnectionTrait, DbBackend, DbErr, EntityTrait, Iterable, PrimaryKeyToColumn,
    PrimaryKeyTrait, TryFromU64, Value,
};

/// PHP `json_encode($value)` (flags 0) of a JSON value.
pub fn php_json(v: &serde_json::Value) -> String {
    laravel_compat::json::to_string(v).unwrap_or_else(|_| "null".into())
}

/// Expression binding a value the way the original stores it.
pub fn value_expr(backend: DbBackend, v: Value) -> Expr {
    match v {
        Value::Json(j) => {
            let text: Option<String> = j.map(|j| php_json(&j));
            match backend {
                DbBackend::Postgres => Expr::cust_with_values("CAST($1 AS json)", [Value::String(text)]),
                _ => Expr::val(Value::String(text)),
            }
        }
        other => Expr::val(other),
    }
}

/// JSON value expression for `update_many().col_expr(...)`.
pub fn json_expr(backend: DbBackend, v: Option<&serde_json::Value>) -> Expr {
    value_expr(backend, Value::Json(v.map(|j| Box::new(j.clone()))))
}

fn set_or_unchanged(av: ActiveValue<Value>) -> Option<Value> {
    match av {
        ActiveValue::Set(v) | ActiveValue::Unchanged(v) => Some(v),
        ActiveValue::NotSet => None,
    }
}

/// INSERT the Set/Unchanged columns of an ActiveModel. Returns the
/// auto-increment id (if the entity has one).
pub async fn insert_id<A, C>(db: &C, am: &A) -> Result<Option<i64>, DbErr>
where
    A: ActiveModelTrait,
    C: ConnectionTrait,
{
    let backend = db.get_database_backend();
    let mut q = Query::insert();
    q.into_table(<A::Entity as Default>::default());
    let mut cols = Vec::new();
    let mut vals = Vec::new();
    for c in <<A::Entity as EntityTrait>::Column as Iterable>::iter() {
        if let Some(v) = set_or_unchanged(am.get(c)) {
            cols.push(c);
            vals.push(value_expr(backend, v));
        }
    }
    q.columns(cols);
    q.values(vals).map_err(|e| DbErr::Custom(e.to_string()))?;
    let auto = <<A::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::auto_increment();
    let pk = <<A::Entity as EntityTrait>::PrimaryKey as Iterable>::iter().next().map(|p| p.into_column());
    if !auto {
        db.execute(&q).await?;
        return Ok(None);
    }
    if backend == DbBackend::Postgres {
        if let Some(pk) = pk {
            q.returning_col(pk);
        }
        let row = db.query_one(&q).await?.ok_or_else(|| DbErr::Custom("insert returned no row".into()))?;
        let id: i64 = row.try_get_by_index::<crate::Int>(0)?.0;
        Ok(Some(id))
    } else {
        let res = db.execute(&q).await?;
        Ok(Some(res.last_insert_id() as i64))
    }
}

/// INSERT and read the row back (like `ActiveModel::insert`).
pub async fn insert<A, C>(db: &C, am: A) -> Result<<A::Entity as EntityTrait>::Model, DbErr>
where
    A: ActiveModelTrait,
    C: ConnectionTrait,
    <<A::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType: TryFromU64,
{
    match insert_id(db, &am).await? {
        Some(id) => {
            let key =
                <<<A::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType as TryFromU64>::try_from_u64(
                    id as u64,
                )?;
            <A::Entity as EntityTrait>::find_by_id(key)
                .one(db)
                .await?
                .ok_or_else(|| DbErr::RecordNotFound("inserted row".into()))
        }
        None => Err(DbErr::Custom("insert(): entity has no auto-increment key; use insert_id()".into())),
    }
}

/// UPDATE only the `Set` columns, keyed by the primary key values of the model.
/// Returns false (and writes nothing) when no column is Set — Eloquent does not
/// save clean models.
pub async fn update<A, C>(db: &C, am: &A) -> Result<bool, DbErr>
where
    A: ActiveModelTrait,
    C: ConnectionTrait,
{
    let backend = db.get_database_backend();
    let mut q = Query::update();
    q.table(<A::Entity as Default>::default());
    let mut any = false;
    for c in <<A::Entity as EntityTrait>::Column as Iterable>::iter() {
        if let ActiveValue::Set(v) = am.get(c) {
            q.value(c, value_expr(backend, v));
            any = true;
        }
    }
    if !any {
        return Ok(false);
    }
    for pk in <<A::Entity as EntityTrait>::PrimaryKey as Iterable>::iter() {
        let col = pk.into_column();
        let v = set_or_unchanged(am.get(col)).ok_or_else(|| DbErr::Custom("update(): primary key not set".into()))?;
        q.and_where(ExprTrait::eq(Expr::col(col), v));
    }
    db.execute(&q).await?;
    Ok(true)
}
