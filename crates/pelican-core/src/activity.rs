//! Activity log writer (`app/Services/Activity/ActivityLogService.php`).

use indexmap::IndexMap;
use pelican_db::entities::{activity_log_subjects, activity_logs};
use pelican_db::{Int, Json, Timestamp};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ConnectionTrait, DatabaseConnection, DbErr,
    TransactionTrait,
};

/// Morph-map keys (`AppServiceProvider::boot` enforceMorphMap).
pub mod morph {
    pub const USER: &str = "user";
    pub const API_KEY: &str = "api_key";
    pub const SERVER: &str = "server";
    pub const NODE: &str = "node";
    pub const BACKUP: &str = "backup";
    pub const DATABASE: &str = "database";
    pub const ALLOCATION: &str = "allocation";
    pub const SCHEDULE: &str = "schedule";
    pub const TASK: &str = "task";
    pub const SSH_KEY: &str = "ssh_key";
    pub const EGG: &str = "egg";
    pub const EGG_VARIABLE: &str = "egg_variable";
}

#[derive(Clone, Debug)]
pub struct Activity {
    pub event: String,
    pub description: Option<String>,
    pub ip: String,
    pub actor: Option<(String, i64)>,
    pub api_key_id: Option<i64>,
    pub properties: IndexMap<String, serde_json::Value>,
    /// In insertion order, de-duplicated.
    pub subjects: Vec<(String, i64)>,
}

impl Activity {
    pub fn new(event: impl Into<String>, ip: impl Into<String>) -> Self {
        Activity {
            event: event.into(),
            description: None,
            ip: ip.into(),
            actor: None,
            api_key_id: None,
            properties: IndexMap::new(),
            subjects: Vec::new(),
        }
    }

    pub fn actor(mut self, kind: &str, id: i64) -> Self {
        self.actor = Some((kind.to_string(), id));
        self
    }

    pub fn api_key(mut self, id: Option<i64>) -> Self {
        self.api_key_id = id;
        self
    }

    pub fn subject(mut self, kind: &str, id: i64) -> Self {
        if !self.subjects.iter().any(|(k, i)| k == kind && *i == id) {
            self.subjects.push((kind.to_string(), id));
        }
        self
    }

    pub fn property(mut self, key: &str, value: impl Into<serde_json::Value>) -> Self {
        self.properties.insert(key.to_string(), value.into());
        self
    }

    /// `save()`: log row, then subject rows, in one transaction.
    pub async fn save(&self, db: &DatabaseConnection) -> Result<i64, DbErr> {
        let txn = db.begin().await?;
        let id = self.save_in(&txn).await?;
        txn.commit().await?;
        Ok(id)
    }

    /// Write inside an existing transaction (`Activity::transaction`).
    pub async fn save_in<C: ConnectionTrait>(&self, db: &C) -> Result<i64, DbErr> {
        let props = if self.properties.is_empty() {
            serde_json::Value::Array(vec![])
        } else {
            serde_json::Value::Object(self.properties.clone().into_iter().collect())
        };
        let row = activity_logs::ActiveModel {
            event: Set(self.event.clone()),
            ip: Set(self.ip.clone()),
            description: Set(self.description.clone()),
            actor_type: Set(self.actor.as_ref().map(|a| a.0.clone())),
            actor_id: Set(self.actor.as_ref().map(|a| Int(a.1))),
            api_key_id: Set(self.api_key_id.map(Int)),
            properties: Set(Json(props)),
            timestamp: Set(Timestamp(crate::time::now())),
            ..Default::default()
        }
        .insert(db)
        .await?;
        for (kind, sid) in &self.subjects {
            activity_log_subjects::ActiveModel {
                activity_log_id: Set(row.id),
                subject_type: Set(kind.clone()),
                subject_id: Set(Int(*sid)),
                ..Default::default()
            }
            .insert(db)
            .await?;
        }
        Ok(row.id.0)
    }
}
