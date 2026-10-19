//! Two SQLite transactions that read before they write (two scheduled backups in the same
//! minute): begun deferred, the second writer fails at once with "database is locked"; begun
//! with `begin_immediate`, it waits for the first to commit.

use std::time::{Duration, Instant};

use pelican_db::Driver;
use pelican_db::testing::empty;
use pelican_db::write::begin_immediate;
use sea_orm::{ConnectionTrait, TransactionTrait};

const READ: &str = "select n from lock_probe where id = 1";
const WRITE: &str = "update lock_probe set n = n + 1 where id = 1";

#[tokio::test]
async fn immediate_transactions_wait_instead_of_failing() -> anyhow::Result<()> {
    let t = empty(Driver::Sqlite).await?;
    t.db.execute_unprepared("create table lock_probe (id integer primary key, n integer not null)").await?;
    t.db.execute_unprepared("insert into lock_probe (id, n) values (1, 0)").await?;

    // Deferred: b already holds a read lock, so its write cannot wait for a's (it would deadlock).
    let a = t.db.begin().await?;
    let b = t.db.begin().await?;
    a.execute_unprepared(READ).await?;
    b.execute_unprepared(READ).await?;
    a.execute_unprepared(WRITE).await?;
    let started = Instant::now();
    let err = b.execute_unprepared(WRITE).await.expect_err("deferred writer should fail");
    assert!(err.to_string().contains("locked"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(5));
    b.rollback().await?;
    a.commit().await?;

    // Immediate: the second transaction starts once the first has committed.
    let a = begin_immediate(&t.db).await?;
    a.execute_unprepared(READ).await?;
    let db = t.db.clone();
    let second = tokio::spawn(async move {
        let b = begin_immediate(&db).await?;
        b.execute_unprepared(READ).await?;
        b.execute_unprepared(WRITE).await?;
        b.commit().await
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    a.execute_unprepared(WRITE).await?;
    a.commit().await?;
    second.await??;

    let n =
        t.db.query_one_raw(sea_orm::Statement::from_string(t.db.get_database_backend(), READ))
            .await?
            .and_then(|r| r.try_get_by_index::<i64>(0).ok());
    assert_eq!(n, Some(3));
    Ok(())
}
