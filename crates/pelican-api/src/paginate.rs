//! `Illuminate\Pagination\LengthAwarePaginator` as serialized by
//! `PaginatedResourceResponse` (`{data, links, meta}`), used by the remote
//! server list (remote-api.md §2.2).

use serde_json::{Map, Value, json};

pub struct LengthAware {
    pub total: i64,
    /// After `perPage ?: 15`; may be negative (no LIMIT).
    pub per_page: i64,
    pub current_page: i64,
    pub last_page: i64,
    pub path: String,
}

/// `PaginationState`: `page` if FILTER_VALIDATE_INT and >= 1, else 1.
pub fn resolve_page(v: Option<&Value>) -> i64 {
    let s = match v {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.trim().to_string(),
        _ => return 1,
    };
    match s.parse::<i64>() {
        Ok(p) if p >= 1 && (s == "0" || !s.trim_start_matches(['+', '-']).starts_with('0')) => p,
        _ => 1,
    }
}

impl LengthAware {
    pub fn new(total: i64, per_page: i64, page: Option<&Value>, path: String) -> Self {
        let per_page = if per_page == 0 { 15 } else { per_page };
        let last_page = if per_page > 0 { ((total as f64) / (per_page as f64)).ceil().max(1.0) as i64 } else { 1 };
        LengthAware { total, per_page, current_page: resolve_page(page), last_page, path }
    }

    pub fn url(&self, page: i64) -> String {
        let sep = if self.path.contains('?') { '&' } else { '?' };
        format!("{}{sep}page={page}", self.path)
    }

    fn opt_url(&self, cond: bool, page: i64) -> Value {
        if cond { Value::String(self.url(page)) } else { Value::Null }
    }

    fn range(&self, from: i64, to: i64) -> Vec<(i64, String)> {
        (from..=to).map(|p| (p, self.url(p))).collect()
    }

    /// `elements()` via `UrlWindow` (onEachSide = 3).
    fn elements(&self) -> Vec<Option<Vec<(i64, String)>>> {
        let on_each_side = 3;
        let last = self.last_page;
        if last < on_each_side * 2 + 8 {
            return vec![Some(self.range(1, last))];
        }
        let window = on_each_side + 4;
        let start = self.range(1, 2);
        let finish = self.range(last - 1, last);
        if self.current_page <= window {
            vec![Some(self.range(1, window + on_each_side)), None, Some(finish)]
        } else if self.current_page > last - window {
            vec![Some(start), None, Some(self.range(last - (window + (on_each_side - 1)), last))]
        } else {
            let slider = self.range(self.current_page - on_each_side, self.current_page + on_each_side);
            vec![Some(start), None, Some(slider), None, Some(finish)]
        }
    }

    fn link_collection(&self) -> Value {
        let mut links = vec![json!({
            "url": self.opt_url(self.current_page > 1, self.current_page - 1),
            "label": "&laquo; Previous",
            "page": if self.current_page > 1 { json!(self.current_page - 1) } else { Value::Null },
            "active": false,
        })];
        for el in self.elements() {
            match el {
                None => links.push(json!({"url": null, "label": "...", "active": false})),
                Some(items) => {
                    for (p, url) in items {
                        links.push(
                            json!({"url": url, "label": p.to_string(), "page": p, "active": p == self.current_page}),
                        );
                    }
                }
            }
        }
        let more = self.current_page < self.last_page;
        links.push(json!({
            "url": self.opt_url(more, self.current_page + 1),
            "label": "Next &raquo;",
            "page": if more { json!(self.current_page + 1) } else { Value::Null },
            "active": false,
        }));
        Value::Array(links)
    }

    /// `{data, links, meta}`.
    pub fn resource_response(&self, data: Vec<Value>) -> Value {
        let count = data.len() as i64;
        let from = if count > 0 { json!((self.current_page - 1) * self.per_page + 1) } else { Value::Null };
        let to = if count > 0 { json!((self.current_page - 1) * self.per_page + count) } else { Value::Null };
        let mut links = Map::new();
        links.insert("first".into(), json!(self.url(1)));
        links.insert("last".into(), json!(self.url(self.last_page)));
        links.insert("prev".into(), self.opt_url(self.current_page > 1, self.current_page - 1));
        links.insert("next".into(), self.opt_url(self.current_page < self.last_page, self.current_page + 1));
        let mut meta = Map::new();
        meta.insert("current_page".into(), json!(self.current_page));
        meta.insert("from".into(), from);
        meta.insert("last_page".into(), json!(self.last_page));
        meta.insert("links".into(), self.link_collection());
        meta.insert("path".into(), json!(self.path));
        meta.insert("per_page".into(), json!(self.per_page));
        meta.insert("to".into(), to);
        meta.insert("total".into(), json!(self.total));
        let mut root = Map::new();
        root.insert("data".into(), Value::Array(data));
        root.insert("links".into(), Value::Object(links));
        root.insert("meta".into(), Value::Object(meta));
        Value::Object(root)
    }
}
