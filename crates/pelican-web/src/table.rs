//! Declarative list tables (Filament `Table`): query-string state, columns,
//! cells, tabs, filters, pagination and row actions, rendered by
//! `components/table.html`. Handlers run their own SQL and fill `rows`.

use indexmap::IndexMap;
use serde::Serialize;

/// List state parsed from the query string: `search`, `sort`, `dir`, `page`,
/// `per_page`, `tab`, `filter[<name>]`.
#[derive(Clone, Debug, Default)]
pub struct ListParams {
    pub search: String,
    pub sort: Option<String>,
    pub desc: bool,
    pub page: u64,
    /// 0 = all records.
    pub per_page: u64,
    /// Default page size: left out of generated URLs.
    pub default_per_page: u64,
    pub tab: Option<String>,
    pub filters: IndexMap<String, String>,
}

impl ListParams {
    pub fn parse(query: Option<&str>, per_page_options: &[u64], default_per_page: u64) -> ListParams {
        let mut p = ListParams { page: 1, per_page: default_per_page, default_per_page, ..Default::default() };
        for (k, v) in form_urlencoded::parse(query.unwrap_or("").as_bytes()) {
            let v = v.trim().to_string();
            match k.as_ref() {
                "search" => p.search = v,
                "sort" if !v.is_empty() => p.sort = Some(v),
                "dir" => p.desc = v == "desc",
                "page" => p.page = v.parse().unwrap_or(1).max(1),
                "per_page" => {
                    let n = if v == "all" { 0 } else { v.parse().unwrap_or(default_per_page) };
                    if per_page_options.contains(&n) {
                        p.per_page = n;
                    }
                }
                "tab" if !v.is_empty() => p.tab = Some(v),
                key => {
                    if let Some(name) = key.strip_prefix("filter[").and_then(|r| r.strip_suffix(']')) {
                        if !v.is_empty() {
                            p.filters.insert(name.to_string(), v);
                        }
                    }
                }
            }
        }
        p
    }

    pub fn filter(&self, name: &str) -> Option<&str> {
        self.filters.get(name).map(String::as_str)
    }

    /// `LIMIT` / `OFFSET` for the current page (None = all).
    pub fn limit_offset(&self) -> Option<(u64, u64)> {
        (self.per_page > 0).then(|| (self.per_page, (self.page - 1) * self.per_page))
    }

    /// Clamp the page after counting (Filament resets to the last page).
    pub fn clamp(&mut self, total: u64) {
        if self.per_page > 0 {
            let last = total.div_ceil(self.per_page).max(1);
            self.page = self.page.min(last);
        }
    }

    fn pairs(&self) -> Vec<(String, String)> {
        let mut v = Vec::new();
        if let Some(t) = &self.tab {
            v.push(("tab".into(), t.clone()));
        }
        if !self.search.is_empty() {
            v.push(("search".into(), self.search.clone()));
        }
        for (k, f) in &self.filters {
            v.push((format!("filter[{k}]"), f.clone()));
        }
        if let Some(s) = &self.sort {
            v.push(("sort".into(), s.clone()));
            if self.desc {
                v.push(("dir".into(), "desc".into()));
            }
        }
        if self.per_page != self.default_per_page {
            v.push(("per_page".into(), if self.per_page == 0 { "all".into() } else { self.per_page.to_string() }));
        }
        if self.page > 1 {
            v.push(("page".into(), self.page.to_string()));
        }
        v
    }

    /// URL of `base` with this state, after applying `f` to a copy.
    pub fn url(&self, base: &str, f: impl FnOnce(&mut ListParams)) -> String {
        let mut p = self.clone();
        f(&mut p);
        let qs: String = form_urlencoded::Serializer::new(String::new()).extend_pairs(p.pairs()).finish();
        if qs.is_empty() { base.to_string() } else { format!("{base}?{qs}") }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Column {
    pub key: String,
    pub label: String,
    pub sortable: bool,
    /// Filled by `Table::finish`.
    pub sort_url: Option<String>,
    pub sorted: Option<&'static str>,
    pub align: &'static str,
    /// Hide below the `md` breakpoint (`visibleFrom('md')`).
    pub wide_only: bool,
}

impl Column {
    pub fn new(key: &str, label: impl Into<String>) -> Column {
        Column {
            key: key.into(),
            label: label.into(),
            sortable: false,
            sort_url: None,
            sorted: None,
            align: "left",
            wide_only: false,
        }
    }

    pub fn sortable(mut self) -> Column {
        self.sortable = true;
        self
    }

    pub fn center(mut self) -> Column {
        self.align = "center";
        self
    }

    pub fn wide_only(mut self) -> Column {
        self.wide_only = true;
        self
    }
}

/// One table cell; `type` selects the rendering in the template.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Cell {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        desc: Option<String>,
        mono: bool,
        /// Copy-to-clipboard value.
        #[serde(skip_serializing_if = "Option::is_none")]
        copy: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tooltip: Option<String>,
    },
    Badge {
        text: String,
        color: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tooltip: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        copy: Option<String>,
    },
    Badges {
        items: Vec<String>,
        color: String,
    },
    Icon {
        icon: String,
        color: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tooltip: Option<String>,
    },
    Bool {
        value: bool,
    },
    Progress {
        percent: f64,
        color: String,
        label: String,
    },
    Image {
        src: String,
    },
    /// Trusted HTML produced by the handler (escape inputs yourself).
    Html {
        html: String,
    },
    /// Inline select that posts `field` to `url` on change (`SelectColumn`).
    Select {
        url: String,
        field: String,
        options: Vec<(String, String)>,
        value: String,
        disabled: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },
    Empty,
}

impl Cell {
    pub fn text(s: impl Into<String>) -> Cell {
        Cell::Text { text: s.into(), desc: None, mono: false, copy: None, tooltip: None }
    }

    pub fn text_desc(s: impl Into<String>, desc: Option<String>) -> Cell {
        Cell::Text { text: s.into(), desc: desc.filter(|d| !d.is_empty()), mono: false, copy: None, tooltip: None }
    }

    pub fn mono(s: impl Into<String>) -> Cell {
        Cell::Text { text: s.into(), desc: None, mono: true, copy: None, tooltip: None }
    }

    pub fn badge(s: impl Into<String>, color: &str) -> Cell {
        Cell::Badge { text: s.into(), color: color.into(), icon: None, tooltip: None, copy: None }
    }

    pub fn icon(icon: &str, color: &str) -> Cell {
        Cell::Icon { icon: icon.into(), color: color.into(), tooltip: None }
    }
}

/// A per-row action: a link, or a POST form (optionally confirmed).
#[derive(Clone, Debug, Serialize)]
pub struct Action {
    pub label: String,
    pub icon: String,
    pub color: String,
    pub url: String,
    pub method: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirm: Option<String>,
    /// Hidden form fields for POST actions.
    pub fields: Vec<(String, String)>,
}

impl Action {
    pub fn link(label: impl Into<String>, icon: &str, url: impl Into<String>) -> Action {
        Action {
            label: label.into(),
            icon: icon.into(),
            color: "gray".into(),
            url: url.into(),
            method: "get",
            confirm: None,
            fields: Vec::new(),
        }
    }

    pub fn post(label: impl Into<String>, icon: &str, url: impl Into<String>) -> Action {
        Action { method: "post", ..Action::link(label, icon, url) }
    }

    pub fn color(mut self, c: &str) -> Action {
        self.color = c.into();
        self
    }

    pub fn confirm(mut self, question: impl Into<String>) -> Action {
        self.confirm = Some(question.into());
        self
    }

    pub fn field(mut self, k: &str, v: impl Into<String>) -> Action {
        self.fields.push((k.into(), v.into()));
        self
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Row {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub cells: IndexMap<String, Cell>,
    pub actions: Vec<Action>,
    /// Actions shown in a dropdown (`ActionGroup`) instead of inline.
    pub group: Vec<Action>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_icon: Option<String>,
    /// Group header shown before this row (`Table::groups`): title and description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading: Option<(String, Option<String>)>,
}

impl Row {
    pub fn new(key: impl Into<String>) -> Row {
        Row { key: key.into(), ..Default::default() }
    }

    pub fn url(mut self, url: impl Into<String>) -> Row {
        self.url = Some(url.into());
        self
    }

    pub fn cell(mut self, key: &str, cell: Cell) -> Row {
        self.cells.insert(key.into(), cell);
        self
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Tab {
    pub key: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub badge: Option<i64>,
    pub url: String,
    pub active: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Filter {
    pub name: String,
    pub label: String,
    /// `(value, label)`.
    pub options: Vec<(String, String)>,
    pub value: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct PageLink {
    pub label: String,
    pub url: Option<String>,
    pub current: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Pagination {
    pub total: u64,
    pub first: u64,
    pub last: u64,
    pub links: Vec<PageLink>,
    pub prev: Option<String>,
    pub next: Option<String>,
    pub per_page: String,
    pub per_page_options: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Table {
    pub id: String,
    pub base_url: String,
    /// This list's URL with the current state (polling target).
    pub current_url: String,
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
    pub searchable: bool,
    pub search: String,
    pub search_placeholder: String,
    pub tabs: Vec<Tab>,
    pub filters: Vec<Filter>,
    pub pagination: Pagination,
    pub empty_heading: String,
    pub empty_description: String,
    pub empty_icon: String,
    /// htmx polling interval (`15s`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll: Option<String>,
    /// Hidden inputs that keep tab/sort/per-page across the search/filter form.
    pub keep: Vec<(String, String)>,
    pub has_actions: bool,
    pub actions_label: String,
}

impl Table {
    pub fn new(id: &str, base_url: &str) -> Table {
        Table { id: id.into(), base_url: base_url.into(), empty_icon: "x".into(), ..Default::default() }
    }

    /// Compute sort/tab/page URLs and pagination after rows and `total` are known.
    pub fn finish(&mut self, p: &ListParams, total: u64, per_page_options: &[u64], locale: &str) {
        let base = self.base_url.clone();
        for c in &mut self.columns {
            if c.sortable {
                let active = p.sort.as_deref() == Some(c.key.as_str());
                c.sorted = active.then_some(if p.desc { "desc" } else { "asc" });
                let key = c.key.clone();
                let desc = active && !p.desc;
                c.sort_url = Some(p.url(&base, |q| {
                    q.sort = Some(key);
                    q.desc = desc;
                    q.page = 1;
                }));
            }
        }
        for t in &mut self.tabs {
            let key = t.key.clone();
            t.active = p.tab.as_deref() == Some(key.as_str());
            t.url = p.url(&base, |q| {
                q.tab = Some(key);
                q.page = 1;
            });
        }
        self.search = p.search.clone();
        self.current_url = p.url(&base, |_| {});
        self.has_actions = self.rows.iter().any(|r| !r.actions.is_empty() || !r.group.is_empty());
        self.actions_label =
            crate::i18n::trans_choice(locale, "filament-tables::table.columns.actions.label", 2.0, &[]);
        let mut keep = Vec::new();
        if let Some(t) = &p.tab {
            keep.push(("tab".to_string(), t.clone()));
        }
        if let Some(s) = &p.sort {
            keep.push(("sort".to_string(), s.clone()));
            if p.desc {
                keep.push(("dir".to_string(), "desc".to_string()));
            }
        }
        self.keep = keep;

        let per = p.per_page;
        let last_page = if per == 0 { 1 } else { total.div_ceil(per).max(1) };
        let (first, last) = if total == 0 {
            (0, 0)
        } else if per == 0 {
            (1, total)
        } else {
            ((p.page - 1) * per + 1, (p.page * per).min(total))
        };
        let mut links = Vec::new();
        // Laravel-style window: first, last, and two pages around the current one.
        let mut prev_shown = 0;
        for n in 1..=last_page {
            let show = n == 1 || n == last_page || n.abs_diff(p.page) <= 2;
            if !show {
                continue;
            }
            if prev_shown != 0 && n > prev_shown + 1 {
                links.push(PageLink { label: "…".into(), url: None, current: false });
            }
            links.push(PageLink {
                label: n.to_string(),
                url: Some(p.url(&base, |q| q.page = n)),
                current: n == p.page,
            });
            prev_shown = n;
        }
        let all =
            crate::i18n::trans(locale, "filament::components/pagination.fields.records_per_page.options.all", &[]);
        self.pagination = Pagination {
            total,
            first,
            last,
            links: if last_page > 1 { links } else { Vec::new() },
            prev: (p.page > 1).then(|| p.url(&base, |q| q.page -= 1)),
            next: (p.page < last_page).then(|| p.url(&base, |q| q.page += 1)),
            per_page: if per == 0 { "all".into() } else { per.to_string() },
            per_page_options: per_page_options
                .iter()
                .map(|n| if *n == 0 { ("all".into(), all.clone()) } else { (n.to_string(), n.to_string()) })
                .collect(),
        };
    }
}

/// Progress colour against the list thresholds (ListServers WARNING/DANGER_THRESHOLD).
pub fn threshold_color(current: f64, max: f64) -> &'static str {
    if max <= 0.0 {
        return "success";
    }
    let ratio = current / max;
    if ratio >= 0.9 {
        "danger"
    } else if ratio >= 0.7 {
        "warning"
    } else {
        "success"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_roundtrip() {
        let p = ListParams::parse(
            Some("search=abc&sort=name&dir=desc&page=3&per_page=50&tab=my&filter[egg]=2"),
            &[10, 20, 50],
            20,
        );
        assert_eq!(p.search, "abc");
        assert_eq!(p.sort.as_deref(), Some("name"));
        assert!(p.desc);
        assert_eq!((p.page, p.per_page), (3, 50));
        assert_eq!(p.filter("egg"), Some("2"));
        let url = p.url("/x", |q| q.page = 1);
        assert_eq!(url, "/x?tab=my&search=abc&filter%5Begg%5D=2&sort=name&dir=desc&per_page=50");
        let d = ListParams::parse(Some("per_page=20"), &[10, 20, 50], 20);
        assert_eq!(d.url("/x", |_| {}), "/x");
    }

    #[test]
    fn invalid_per_page_falls_back() {
        let p = ListParams::parse(Some("per_page=7"), &[10, 20], 20);
        assert_eq!(p.per_page, 20);
    }

    #[test]
    fn pagination_window() {
        let p = ListParams::parse(Some("page=5&per_page=10"), &[10], 10);
        let mut t = Table::new("t", "/x");
        t.finish(&p, 200, &[10], "en");
        let labels: Vec<_> = t.pagination.links.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(labels, ["1", "…", "3", "4", "5", "6", "7", "…", "20"]);
        assert_eq!((t.pagination.first, t.pagination.last), (41, 50));
    }
}
