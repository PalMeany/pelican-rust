//! Load-test data (`bench/`): the difftest fixtures cloned into a realistic panel —
//! `scale` × (1000 users, 5000 servers with allocations and variables, 100000 activity
//! entries) over 10 nodes. The fixture user 2 (client key `pacc_user0000001…`) owns the
//! first 50 servers; fixture node 1 (daemon token in infra.yaml) hosts a tenth of them.

use indexmap::IndexMap;
use serde_yaml::Value;

use crate::scenario::Rows;

type Row = IndexMap<String, Value>;

fn template(rows: &Rows, table: &str, id: i64) -> Row {
    rows.get(table)
        .and_then(|l| l.iter().find(|r| r.get("id").and_then(Value::as_i64) == Some(id)))
        .cloned()
        .unwrap_or_else(|| panic!("fixture {table}#{id} missing"))
}

fn set(r: &mut Row, k: &str, v: impl Into<Value>) {
    r.insert(k.to_string(), v.into());
}

/// Deterministic UUID-shaped ids (`<prefix>-…-<n>`), unique per table.
fn uuid(prefix: u32, n: usize) -> String {
    format!("{prefix:08x}-0000-4000-8000-{n:012x}")
}

pub fn scale_rows(fixtures: &Rows, scale: usize) -> Rows {
    let mut out = Rows::new();
    let (users, servers, activity) = (1000 * scale, 5000 * scale, 100_000 * scale);
    let user_t = template(fixtures, "users", 2);
    for i in 0..users {
        let mut r = user_t.clone();
        let id = 1000 + i as i64;
        set(&mut r, "id", id);
        set(&mut r, "uuid", uuid(0xbe0c0001, i));
        set(&mut r, "username", format!("bench{i}"));
        set(&mut r, "email", format!("bench{i}@bench.test"));
        r.shift_remove("external_id");
        out.entry("users".into()).or_default().push(r);
    }
    let node_t = template(fixtures, "nodes", 1);
    let mut nodes = vec![1i64];
    for k in 0..9 {
        let mut r = node_t.clone();
        let id = 100 + k as i64;
        set(&mut r, "id", id);
        set(&mut r, "uuid", uuid(0xbe0c0002, k));
        set(&mut r, "name", format!("Bench Node {k}"));
        set(&mut r, "daemon_token_id", format!("benchNodeToken{k:02}"));
        out.entry("nodes".into()).or_default().push(r);
        nodes.push(id);
    }
    let (server_t, alloc_t) = (template(fixtures, "servers", 1), template(fixtures, "allocations", 1));
    let vars: Vec<Row> = fixtures
        .get("server_variables")
        .map(|l| l.iter().filter(|r| r.get("server_id").and_then(Value::as_i64) == Some(1)).cloned().collect())
        .unwrap_or_default();
    let mut var_id = 100_000i64;
    for i in 0..servers {
        let id = 10_000 + i as i64;
        let node = nodes[i % nodes.len()];
        let mut a = alloc_t.clone();
        set(&mut a, "id", id);
        set(&mut a, "node_id", node);
        set(&mut a, "ip", format!("10.{}.{}.{}", (i >> 16) & 255, (i >> 8) & 255, i & 255));
        set(&mut a, "port", 25565);
        set(&mut a, "server_id", id);
        a.shift_remove("ip_alias");
        out.entry("allocations".into()).or_default().push(a);
        let mut s = server_t.clone();
        let u = uuid(0xbe0c0003, i);
        set(&mut s, "id", id);
        set(&mut s, "uuid_short", format!("b{:07x}", i));
        set(&mut s, "uuid", u);
        set(&mut s, "name", format!("Bench Server {i}"));
        set(&mut s, "owner_id", if i < 50 { 2 } else { 1000 + (i % users) as i64 });
        set(&mut s, "node_id", node);
        set(&mut s, "allocation_id", id);
        s.shift_remove("external_id");
        out.entry("servers".into()).or_default().push(s);
        for v in &vars {
            let mut v = v.clone();
            set(&mut v, "id", var_id);
            set(&mut v, "server_id", id);
            var_id += 1;
            out.entry("server_variables".into()).or_default().push(v);
        }
    }
    let (log_t, subj_t) = (template(fixtures, "activity_logs", 1), template(fixtures, "activity_log_subjects", 1));
    for i in 0..activity {
        let id = 1_000_000 + i as i64;
        let mut l = log_t.clone();
        set(&mut l, "id", id);
        set(&mut l, "actor_id", if i % 10 == 0 { 2 } else { 1000 + (i % users) as i64 });
        set(&mut l, "timestamp", format!("2026-01-{:02} {:02}:{:02}:00", 1 + i % 28, (i / 60) % 24, i % 60));
        out.entry("activity_logs".into()).or_default().push(l);
        let mut s = subj_t.clone();
        set(&mut s, "id", id);
        set(&mut s, "activity_log_id", id);
        // The client key's user's own entries are about the account (`User::activity()` is the
        // morph on activity_log_subjects), the rest about servers.
        if i % 10 == 0 {
            set(&mut s, "subject_type", "user");
            set(&mut s, "subject_id", 2);
        } else {
            set(&mut s, "subject_id", 10_000 + (i % servers) as i64);
        }
        out.entry("activity_log_subjects".into()).or_default().push(s);
    }
    out
}
