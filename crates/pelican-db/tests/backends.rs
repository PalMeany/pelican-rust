//! Fresh install + flexible column types against every backend.

use pelican_db::entities::nodes;
use pelican_db::install::{schema_state, SchemaState};
use pelican_db::testing::{drivers_under_test, fresh};
use pelican_db::{Bool, Int, Json, SmallBool, Timestamp};
use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait, QueryFilter, ColumnTrait};

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
