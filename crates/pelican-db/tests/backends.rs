//! Fresh install + flexible column types against every backend.

use pelican_db::entities::nodes;
use pelican_db::install::{SchemaState, schema_state};
use pelican_db::testing::{drivers_under_test, fresh};
use pelican_db::{Bool, Int, Json, SmallBool, Timestamp};
use sea_orm::{ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};

#[tokio::test]
async fn fresh_install_and_roundtrip() -> anyhow::Result<()> {
    for driver in drivers_under_test() {
        let t = fresh(driver).await.map_err(|e| anyhow::anyhow!("{driver:?}: {e}"))?;
        assert_eq!(schema_state(&t.db).await?, SchemaState::Ready { unknown: vec![] }, "{driver:?}");

        let now = chrono::NaiveDate::from_ymd_opt(2026, 10, 14).unwrap().and_hms_opt(12, 30, 5).unwrap();
        let am = nodes::ActiveModel {
            uuid: Set(Some("0b9a3d0e-2c39-4b0e-8f41-3a0f1d9f0c11".into())),
            public: Set(SmallBool(true)),
            name: Set("node \u{1F600} /x".into()),
            fqdn: Set("wings.test".into()),
            scheme: Set("http".into()),
            behind_proxy: Set(Bool(false)),
            maintenance_mode: Set(Bool(true)),
            memory: Set(Int(4096)),
            memory_overallocate: Set(Int(-1)),
            disk: Set(Int(100_000)),
            disk_overallocate: Set(Int(0)),
            cpu: Set(Int(0)),
            cpu_overallocate: Set(Int(0)),
            upload_size: Set(Int(100)),
            daemon_token_id: Set(Some("abcdefghijklmnop".into())),
            daemon_token: Set("eyJpdiI6...".into()),
            daemon_listen: Set(Int(8080)),
            daemon_sftp: Set(Int(2022)),
            daemon_base: Set("/var/lib/pelican/volumes".into()),
            daemon_connect: Set(Int(8080)),
            tags: Set(Some(Json(serde_json::json!(["eu", "a/b"])))),
            created_at: Set(Some(Timestamp(now))),
            updated_at: Set(Some(Timestamp(now))),
            ..Default::default()
        };
        let inserted = am.insert(&t.db).await.map_err(|e| anyhow::anyhow!("{driver:?} insert: {e}"))?;
        assert_eq!(inserted.id, Int(1), "{driver:?}");

        let found = nodes::Entity::find()
            .filter(nodes::Column::Public.eq(SmallBool(true)))
            .filter(nodes::Column::MaintenanceMode.eq(Bool(true)))
            .filter(nodes::Column::Memory.gt(Int(10)))
            .one(&t.db)
            .await
            .map_err(|e| anyhow::anyhow!("{driver:?} select: {e}"))?
            .expect("row");
        assert_eq!(found.name, "node \u{1F600} /x", "{driver:?}");
        assert_eq!(found.public, SmallBool(true));
        assert_eq!(found.behind_proxy, Bool(false));
        assert_eq!(found.memory_overallocate, Int(-1));
        assert_eq!(found.tags, Some(Json(serde_json::json!(["eu", "a/b"]))));
        assert_eq!(found.created_at, Some(Timestamp(now)), "{driver:?}");

        let mut am: nodes::ActiveModel = found.into();
        am.public = Set(SmallBool(false));
        am.tags = Set(None);
        let updated = am.update(&t.db).await.map_err(|e| anyhow::anyhow!("{driver:?} update: {e}"))?;
        assert_eq!(updated.public, SmallBool(false));
        assert_eq!(updated.tags, None);
        eprintln!("{driver:?}: ok");
    }
    Ok(())
}

/// Every entity's column list must exist on every backend.
#[tokio::test]
async fn all_entities_select() -> anyhow::Result<()> {
    use pelican_db::entities::*;
    use sea_orm::DatabaseConnection;
    async fn sel<E: EntityTrait>(db: &DatabaseConnection, name: &str) -> anyhow::Result<()> {
        E::find().all(db).await.map_err(|e| anyhow::anyhow!("{name}: {e}"))?;
        Ok(())
    }
    for driver in drivers_under_test() {
        let t = fresh(driver).await?;
        let db = &t.db;
        macro_rules! check { ($($m:ident),*) => { $( sel::<$m::Entity>(db, stringify!($m)).await.map_err(|e| anyhow::anyhow!("{driver:?}: {e}"))?; )* } }
        check!(
            activity_log_subjects,
            activity_logs,
            allocations,
            api_keys,
            api_logs,
            backup_host_node,
            backup_hosts,
            backups,
            database_host_node,
            database_hosts,
            databases,
            egg_variables,
            eggs,
            failed_jobs,
            jobs,
            migrations,
            model_has_permissions,
            model_has_roles,
            mountables,
            mounts,
            node_role,
            nodes,
            notifications,
            passkeys,
            password_resets,
            permissions,
            role_has_permissions,
            roles,
            schedules,
            server_transfers,
            server_user_settings,
            server_variables,
            servers,
            sessions,
            settings,
            subusers,
            tasks,
            tasks_log,
            user_ssh_keys,
            users,
            webhook_configurations,
            webhooks
        );
        // Seed row written by the original migrations decodes.
        let hosts = backup_hosts::Entity::find().all(db).await?;
        assert_eq!(hosts.len(), 1, "{driver:?}");
        eprintln!("{driver:?}: all entities ok");
    }
    Ok(())
}

/// QUESTIONS Q5: MySQL/MariaDB "zero" dates from old non-strict servers read as NULL.
#[tokio::test]
async fn mysql_zero_dates_read_as_null() -> anyhow::Result<()> {
    use pelican_db::Driver;
    use sea_orm::{ConnectionTrait, FromQueryResult, Statement};
    #[derive(FromQueryResult)]
    struct Row {
        at: Option<Timestamp>,
    }
    for driver in drivers_under_test().into_iter().filter(|d| matches!(d, Driver::Mysql | Driver::Mariadb)) {
        let t = pelican_db::testing::empty(driver).await.map_err(|e| anyhow::anyhow!("{driver:?}: {e}"))?;
        let b = t.db.get_database_backend();
        for sql in [
            "SET SESSION sql_mode = ''",
            "CREATE TABLE zero_dates (id INT PRIMARY KEY, at DATETIME NULL)",
            "INSERT INTO zero_dates VALUES (1, '0000-00-00 00:00:00'), (2, '2026-10-15 01:02:03'), (3, NULL)",
        ] {
            t.db.execute_raw(Statement::from_string(b, sql)).await?;
        }
        let rows = Row::find_by_statement(Statement::from_string(b, "SELECT at FROM zero_dates ORDER BY id"))
            .all(&t.db)
            .await?;
        let got: Vec<Option<String>> = rows.iter().map(|r| r.at.map(|t| t.0.to_string())).collect();
        assert_eq!(got, vec![None, Some("2026-10-15 01:02:03".into()), None], "{driver:?}");
    }
    Ok(())
}
