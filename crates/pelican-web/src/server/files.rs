//! File manager (app/Filament/Server/Resources/Files: ListFiles, EditFiles,
//! DownloadFiles, SearchFiles) over Wings (`DaemonFileRepository`).

use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::Query;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::services::LogActivity;
use pelican_core::state::AppState;
use pelican_wings::WingsError;
use pelican_wings::client::{Body, Call, Method};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::ServerPanel;
use crate::error::{WebError, WebResult};
use crate::forms::Form;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/server/{server}/files", get(list))
        .route("/server/{server}/files/action", post(action))
        .route("/server/{server}/files/edit", get(edit_form).post(save))
        .route("/server/{server}/files/download", get(download))
        .route("/server/{server}/files/upload-url", get(upload_url))
        .route("/server/{server}/files/uploaded", post(uploaded))
        .route("/server/{server}/files/search", get(search))
        .route("/server/{server}/files/{*rest}", get(original_url))
}

/// `File::ARCHIVE_MIMES`.
const ARCHIVE_MIMES: &[&str] = &[
    "application/vnd.rar",
    "application/x-rar-compressed",
    "application/x-tar",
    "application/x-br",
    "application/x-bzip2",
    "application/gzip",
    "application/x-gzip",
    "application/x-lzip",
    "application/x-sz",
    "application/x-xz",
    "application/x-7z-compressed",
    "application/zstd",
    "application/zip",
];

/// `join_paths()` (app/helpers.php).
pub fn join_paths(base: &str, parts: &[&str]) -> String {
    let base = base.trim_end_matches('/');
    let parts: Vec<&str> = parts.iter().map(|p| p.trim_matches('/')).filter(|p| !p.is_empty()).collect();
    if base.is_empty() { parts.join("/") } else { format!("{base}/{}", parts.join("/")) }
}

/// `resolve_path()` (app/helpers.php).
pub fn resolve_path(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        match part {
            "." => {}
            ".." => {
                out.pop();
            }
            p => out.push(p),
        }
    }
    out.join("/")
}

fn dirname(path: &str) -> String {
    let t = path.trim_end_matches('/');
    match t.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => t[..i].to_string(),
    }
}

/// A row of `File::getRows()`.
#[derive(Clone, Debug, Serialize)]
pub struct FileRow {
    pub name: String,
    pub mode: String,
    pub mode_bits: i64,
    pub size: i64,
    pub is_directory: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    pub mime_type: String,
    pub modified_at: String,
    pub icon: &'static str,
    pub can_edit: bool,
    pub is_archive: bool,
}

impl FileRow {
    fn from_wings(v: &Value) -> FileRow {
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        let name = s("name");
        let is_file = b("file");
        let mut mime = s("mime");
        if is_file && name.to_lowercase().ends_with(".jar") && ARCHIVE_MIMES.contains(&mime.as_str()) {
            mime = "application/java-archive".into();
        }
        let is_directory = b("directory");
        let is_symlink = b("symlink");
        let is_archive = is_file && ARCHIVE_MIMES.contains(&mime.as_str());
        let is_image = mime.starts_with("image/") && mime != "image/svg+xml";
        let icon = if is_directory {
            "folder"
        } else if is_archive {
            "file-zip"
        } else if is_image {
            "photo"
        } else if is_symlink {
            "file-symlink"
        } else {
            "file"
        };
        let can_edit = !(is_directory || is_archive || is_symlink || is_image)
            && is_file
            && !["application/java-archive", "application/octet-stream", "inode/directory"].contains(&mime.as_str());
        let modified = chrono::DateTime::parse_from_rfc3339(&s("modified"))
            .map(|d| d.with_timezone(&chrono::Utc).format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        FileRow {
            name,
            mode: s("mode"),
            mode_bits: v
                .get("mode_bits")
                .and_then(|m| m.as_str().and_then(|s| s.parse().ok()).or(m.as_i64()))
                .unwrap_or(0),
            size: v.get("size").and_then(Value::as_i64).unwrap_or(0),
            is_directory,
            is_file,
            is_symlink,
            mime_type: mime,
            modified_at: modified,
            icon,
            can_edit,
            is_archive,
        }
    }
}

/// `DaemonFileRepository` for one server.
struct Repo<'a> {
    p: &'a ServerPanel,
}

impl Repo<'_> {
    fn path(&self, action: &str) -> String {
        format!("/api/servers/{}/files/{action}", self.p.server.uuid)
    }

    async fn send(&self, call: Call<'_>) -> Result<pelican_wings::client::WingsResponse, WingsError> {
        let conn = self.p.st.node_conn(&self.p.node).map_err(|e| WingsError::Connection(e.to_string()))?;
        self.p.st.wings.send(&conn, call).await
    }

    async fn list(&self, dir: &str) -> Result<Vec<FileRow>, WingsError> {
        let p = self.path("list-directory");
        let q = [("directory", dir.to_string())];
        let r = self.send(Call::new(Method::GET, &p).query(&q)).await?;
        Ok(r.json().and_then(|v| v.as_array().cloned()).unwrap_or_default().iter().map(FileRow::from_wings).collect())
    }

    async fn search(&self, term: &str, dir: &str) -> Result<Vec<FileRow>, WingsError> {
        let p = self.path("search");
        let q = [("pattern", term.to_string()), ("directory", dir.to_string())];
        let mut c = Call::new(Method::GET, &p).query(&q);
        c.timeout = Some(Duration::from_secs(120));
        let r = self.send(c).await?;
        Ok(r.json().and_then(|v| v.as_array().cloned()).unwrap_or_default().iter().map(FileRow::from_wings).collect())
    }

    async fn content(&self, file: &str) -> Result<pelican_wings::client::WingsResponse, WingsError> {
        let p = self.path("contents");
        let q = [("file", file.to_string())];
        self.send(Call::new(Method::GET, &p).query(&q)).await
    }

    async fn put(&self, file: &str, content: Vec<u8>) -> Result<(), WingsError> {
        let p = self.path("write");
        let q = [("file", file.to_string())];
        let mut c = Call::new(Method::POST, &p).query(&q);
        c.body = Body::Raw(content);
        self.send(c).await.map(|_| ())
    }

    async fn post(
        &self,
        action: &str,
        body: Value,
        long: bool,
    ) -> Result<pelican_wings::client::WingsResponse, WingsError> {
        let p = self.path(action);
        let mut c = Call::new(Method::POST, &p).json(body);
        if long {
            c.timeout = Some(Duration::from_secs(15 * 60));
        }
        self.send(c).await
    }

    async fn rename(&self, root: &str, files: Value) -> Result<(), WingsError> {
        let p = self.path("rename");
        self.send(Call::new(Method::PUT, &p).json(json!({ "root": root, "files": files }))).await.map(|_| ())
    }
}

fn wings_message(e: &WingsError) -> String {
    match e {
        WingsError::Connection(_) => "Node connection failed".into(),
        WingsError::Request { status, .. } => format!("Wings returned HTTP {status}"),
    }
}

/// The original's file manager URLs (`/files/{path?}`, `/files/edit/{path}`, `/files/download/{path}`,
/// `/files/search/{searchTerm}`, app/Filament/Server/Resources/Files/FileResource.php), as plugins and
/// bookmarks link them: redirected to ours, which take the path as a query parameter.
async fn original_url(
    axum::extract::Path((server, rest)): axum::extract::Path<(String, String)>,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> Response {
    let (page, key, value) = match rest.split_once('/') {
        Some(("edit", p)) => ("/edit", "path", p),
        Some(("download", p)) => ("/download", "path", p),
        Some(("search", t)) => ("/search", "q", t),
        _ => ("", "path", rest.as_str()),
    };
    let mut url = format!(
        "/server/{server}/files{page}?{}",
        form_urlencoded::Serializer::new(String::new()).append_pair(key, value).finish()
    );
    if let Some(extra) = query.filter(|q| !q.is_empty()) {
        url.push('&');
        url.push_str(&extra);
    }
    Redirect::to(&url).into_response()
}

#[derive(Deserialize, Default)]
struct PathQuery {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    sort: Option<String>,
    #[serde(default)]
    dir: Option<String>,
    #[serde(default)]
    page: Option<u64>,
    #[serde(default)]
    per_page: Option<u64>,
    #[serde(default)]
    q: Option<String>,
}

fn norm_dir(p: Option<&str>) -> String {
    let r = resolve_path(p.unwrap_or("/"));
    if r.is_empty() { "/".into() } else { format!("/{r}") }
}

fn breadcrumbs(base: &str, dir: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut acc = String::new();
    for part in dir.split('/').filter(|p| !p.is_empty()) {
        acc = format!("{acc}/{part}");
        out.push((part.to_string(), format!("{base}/files?path={}", urlencode(&acc))));
    }
    out
}

fn urlencode(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// `GET /server/{server}/files?path=` (ListFiles).
async fn list(p: ServerPanel, Query(q): Query<PathQuery>) -> WebResult {
    p.authorize("file.read")?;
    let dir = norm_dir(q.path.as_deref());
    let repo = Repo { p: &p };
    let (mut rows, error) = match repo.list(&dir).await {
        Ok(r) => (r, None),
        Err(e) => (Vec::new(), Some(wings_message(&e))),
    };
    let needle = q.search.clone().unwrap_or_default().to_lowercase();
    if !needle.is_empty() {
        rows.retain(|r| r.name.to_lowercase().contains(&needle));
    }
    let desc = q.dir.as_deref() == Some("desc");
    match q.sort.as_deref().unwrap_or("name") {
        "size" => rows.sort_by_key(|r| r.size),
        "modified_at" => rows.sort_by(|a, b| a.modified_at.cmp(&b.modified_at)),
        _ => rows.sort_by(|a, b| a.name.cmp(&b.name)),
    }
    if desc {
        rows.reverse();
    }
    // `orderByDesc('is_directory')` first.
    rows.sort_by_key(|r| !r.is_directory);
    let per_page = q.per_page.filter(|n| [25, 50, 100, 150, 200].contains(n)).unwrap_or(50);
    let total = rows.len() as u64;
    let last_page = total.div_ceil(per_page).max(1);
    let page = q.page.unwrap_or(1).clamp(1, last_page);
    let start = ((page - 1) * per_page) as usize;
    let shown: Vec<FileRow> = rows.into_iter().skip(start).take(per_page as usize).collect();
    let base = p.base_url();
    let qs = |page: u64| {
        let mut s = format!("{base}/files?path={}&page={page}", urlencode(&dir));
        if per_page != 50 {
            s.push_str(&format!("&per_page={per_page}"));
        }
        if !needle.is_empty() {
            s.push_str(&format!("&search={}", urlencode(&needle)));
        }
        s
    };
    let sort_url = |col: &str| {
        let active = q.sort.as_deref().unwrap_or("name") == col;
        let next_desc = active && !desc;
        format!("{base}/files?path={}&sort={col}{}", urlencode(&dir), if next_desc { "&dir=desc" } else { "" })
    };
    let archive_default = format!("archive-{}Z", chrono::Utc::now().format("%Y-%m-%dT%H%M%S"));
    let sftp = super::sftp_url(&p, Some(&dir));
    p.page(
        "files",
        "server/files.html",
        context! {
            title => p.t("server/file.title"),
            dir => dir.clone(),
            parent => (dir != "/").then(|| dirname(&dir)),
            crumbs => breadcrumbs(&base, &dir),
            rows => Mj::from_serialize(&shown),
            error => error,
            search => needle,
            sort => q.sort.clone().unwrap_or_else(|| "name".into()),
            desc => desc,
            sort_urls => context! { name => sort_url("name"), size => sort_url("size"), modified_at => sort_url("modified_at") },
            pagination => context! {
                page => page, last => last_page, total => total, per_page => per_page,
                prev => (page > 1).then(|| qs(page - 1)), next => (page < last_page).then(|| qs(page + 1)),
                first_item => if total == 0 { 0 } else { start as u64 + 1 },
                last_item => (start as u64 + per_page).min(total),
            },
            archive_default => archive_default,
            sftp_url => sftp,
            can => context! {
                read_content => p.can("file.read-content"),
                create => p.can("file.create"),
                update => p.can("file.update"),
                delete => p.can("file.delete"),
                archive => p.can("file.archive"),
            },
        },
    )
}

fn back_to(p: &ServerPanel, dir: &str) -> Response {
    p.redirect(&format!("{}/files?path={}", p.base_url(), urlencode(dir)))
}

fn checked(f: &Form, group: &str) -> u32 {
    let list = f.list(group);
    (if list.iter().any(|v| v == "read") { 4 } else { 0 })
        | (if list.iter().any(|v| v == "write") { 2 } else { 0 })
        | (if list.iter().any(|v| v == "execute") { 1 } else { 0 })
}

/// `POST /server/{server}/files/action`: the record/toolbar/bulk actions of ListFiles.
async fn action(p: ServerPanel, form: Form) -> WebResult {
    let dir = norm_dir(form.opt("directory"));
    let op = form.get("op").to_string();
    let name = form.get("name").to_string();
    let repo = Repo { p: &p };
    let st = &p.st;
    let selected = form.list("files");
    let fail = |e: WingsError| -> WebResult {
        p.flash_danger(p.t("server/file.alerts.files_node_error.title"), Some(wings_message(&e)));
        Ok(back_to(&p, &dir))
    };
    match op.as_str() {
        "rename" => {
            p.authorize("file.update")?;
            let to = form.get("to").trim().to_string();
            let files = json!([{ "to": to, "from": name }]);
            if let Err(e) = repo.rename(&dir, files.clone()).await {
                return fail(e);
            }
            let _ = p
                .activity_for_server("server:file.rename")
                .property("directory", dir.clone())
                .property("files", files)
                .property("to", to.clone())
                .property("from", name.clone())
                .log(st)
                .await;
            p.session.flash("success", p.t("server/file.actions.rename.notification"), Some(format!("{name} -> {to}")));
        }
        "move" => {
            p.authorize("file.update")?;
            let location = form.get("location").trim().to_string();
            let to = if location.ends_with('/') { join_paths(&location, &[&name]) } else { location.clone() };
            let files = json!([{ "to": to, "from": name }]);
            if let Err(e) = repo.rename(&dir, files.clone()).await {
                return fail(e);
            }
            let old = join_paths(&dir, &[&name]);
            let new = resolve_path(&join_paths(&dir, &[&to]));
            let _ = p
                .activity_for_server("server:file.rename")
                .property("directory", dir.clone())
                .property("files", files)
                .property("to", new.clone())
                .property("from", old.clone())
                .log(st)
                .await;
            p.session.flash("success", p.t("server/file.actions.move.notification"), Some(format!("{old} -> {new}")));
        }
        "bulk-move" => {
            p.authorize("file.update")?;
            let location = form.get("location").trim().to_string();
            let files: Vec<Value> =
                selected.iter().map(|f| json!({ "to": join_paths(&location, &[f]), "from": f })).collect();
            if let Err(e) = repo.rename(&dir, json!(files)).await {
                return fail(e);
            }
            let _ = p
                .activity_for_server("server:file.rename")
                .property("directory", dir.clone())
                .property("files", json!(files))
                .log(st)
                .await;
            let target = resolve_path(&join_paths(&dir, &[&location]));
            p.flash_success(p.t_with(
                "server/file.actions.move.bulk_notification",
                &[("count", &files.len().to_string()), ("directory", &target)],
            ));
        }
        "copy" => {
            p.authorize("file.create")?;
            let file = join_paths(&dir, &[&name]);
            if let Err(e) = repo.post("copy", json!({ "location": file }), false).await {
                return fail(e);
            }
            let _ = p.activity_for_server("server:file.copy").property("file", file).log(st).await;
            p.flash_success(p.t("server/file.actions.copy.notification"));
        }
        "chmod" => {
            p.authorize("file.update")?;
            let mode =
                format!("{}{}{}", checked(&form, "owner[]"), checked(&form, "group[]"), checked(&form, "public[]"));
            if let Err(e) =
                repo.post("chmod", json!({ "root": dir, "files": [{ "file": name, "mode": mode }] }), false).await
            {
                return fail(e);
            }
            p.flash_success(p.t_with("server/file.actions.permissions.notification", &[("mode", &mode)]));
        }
        "archive" | "bulk-archive" => {
            p.authorize("file.archive")?;
            let files: Vec<String> = if op == "archive" { vec![name.clone()] } else { selected.clone() };
            let ext = form.get("extension");
            let ext = if ["tar.gz", "zip", "tar.bz2", "tar.xz"].contains(&ext) { ext } else { "tar.gz" };
            let archive_name = form.get("archive_name").trim().to_string();
            match repo
                .post("compress", json!({ "root": dir, "files": files, "name": archive_name, "extension": ext }), true)
                .await
            {
                Ok(r) => {
                    let created = r
                        .json()
                        .and_then(|v| v.get("name").and_then(Value::as_str).map(String::from))
                        .unwrap_or_default();
                    let _ = p
                        .activity_for_server("server:file.compress")
                        .property("name", created.clone())
                        .property("directory", dir.clone())
                        .property("files", json!(files))
                        .log(st)
                        .await;
                    p.session.flash("success", p.t("server/file.actions.archive.notification"), Some(created));
                }
                Err(e) => return fail(e),
            }
        }
        "unarchive" => {
            p.authorize("file.archive")?;
            if let Err(e) = repo.post("decompress", json!({ "root": dir, "file": name }), true).await {
                return fail(e);
            }
            let _ = p
                .activity_for_server("server:file.decompress")
                .property("directory", dir.clone())
                .property("file", name.clone())
                .log(st)
                .await;
            p.flash_success(p.t("server/file.actions.unarchive.notification"));
        }
        "delete" | "bulk-delete" => {
            p.authorize("file.delete")?;
            let files: Vec<String> = if op == "delete" { vec![name.clone()] } else { selected.clone() };
            if let Err(e) = repo.post("delete", json!({ "root": dir, "files": files }), false).await {
                return fail(e);
            }
            let prop = if op == "delete" { json!(name) } else { json!(files) };
            let _ = p
                .activity_for_server("server:file.delete")
                .property("directory", dir.clone())
                .property("files", prop)
                .log(st)
                .await;
            if op == "bulk-delete" {
                p.flash_success(
                    p.t_with("server/file.actions.delete.bulk_notification", &[("count", &files.len().to_string())]),
                );
            }
        }
        "new-folder" => {
            p.authorize("file.create")?;
            match repo.post("create-directory", json!({ "name": name, "path": dir }), false).await {
                Ok(_) => {
                    let _ = p
                        .activity_for_server("server:file.create-directory")
                        .property("directory", dir.clone())
                        .property("name", name.clone())
                        .log(st)
                        .await;
                }
                Err(WingsError::Request { status: 400, .. }) => {
                    p.flash_danger(
                        strip_code(&p.t_with(
                            "server/file.alerts.file_already_exists.title",
                            &[("name", &join_paths(&dir, &[&name]))],
                        )),
                        None,
                    );
                }
                Err(e) => return fail(e),
            }
        }
        "new-file" => {
            p.authorize("file.create")?;
            let path = join_paths(&dir, &[&name]);
            match repo.put(&path, form.get("content").as_bytes().to_vec()).await {
                Ok(()) => {
                    let _ = p.activity_for_server("server:file.write").property("file", path).log(st).await;
                }
                Err(WingsError::Request { status: 400, .. }) => {
                    p.flash_danger(
                        strip_code(&p.t_with("server/file.alerts.file_already_exists.title", &[("name", &path)])),
                        None,
                    );
                }
                Err(e) => return fail(e),
            }
        }
        "pull" => {
            p.authorize("file.create")?;
            let url = form.get("url").trim().to_string();
            if !pelican_api::url_rule::is_url(&url) {
                p.flash_danger(p.t_with("validation.url", &[("attribute", "url")]), None);
                return Ok(back_to(&p, &dir));
            }
            if let Err(e) = repo.post("pull", json!({ "url": url, "root": dir }), false).await {
                return fail(e);
            }
            let _ = p
                .activity_for_server("server:file.pull")
                .property("url", url)
                .property("directory", dir.clone())
                .log(st)
                .await;
        }
        _ => return Err(WebError::Unprocessable),
    }
    Ok(back_to(&p, &dir))
}

/// Alert titles carry `<code>` markup; notifications are plain text here.
fn strip_code(s: &str) -> String {
    s.replace("<code>", "").replace("</code>", "")
}

/// Map an extension to the editor's language list (`EditorLanguages::fromWithAlias`).
fn language_for(path: &str) -> String {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "h" => "c",
        "cc" | "hpp" => "cpp",
        "cs" => "csharp",
        "class" => "java",
        "htm" => "html",
        "js" | "mjs" | "cjs" => "javascript",
        "kt" | "kts" => "kotlin",
        "md" => "markdown",
        "m" => "objective-c",
        "pl" | "pm" => "perl",
        "php3" | "php4" | "php5" | "phtml" => "php",
        "py" | "pyc" | "pyo" | "pyi" => "python",
        "rb" | "erb" => "ruby",
        "sc" => "scala",
        "sh" | "zsh" => "shell",
        "ts" | "tsx" => "typescript",
        "yml" => "yaml",
        "json" | "yaml" | "ini" | "xml" | "html" | "css" | "lua" | "sql" | "java" | "c" | "cpp" | "go" | "rust"
        | "toml" | "properties" | "dockerfile" | "markdown" => ext.as_str(),
        _ => "plaintext",
    }
    .to_string()
}

/// `GET /server/{server}/files/edit?path=` (EditFiles).
async fn edit_form(p: ServerPanel, Query(q): Query<PathQuery>) -> WebResult {
    p.authorize("file.read-content")?;
    let path = q.path.clone().unwrap_or_default();
    let repo = Repo { p: &p };
    let st = &p.st;
    let _ = p.activity_for_server("server:file.read").property("file", path.clone()).log(st).await;
    let name = path.rsplit('/').next().unwrap_or(&path).to_string();
    let content = match repo.content(&path).await {
        Ok(r) => {
            let max = st.config.files_max_edit_size;
            if max != 0 && r.content_length.is_some_and(|l| l as i128 > max as i128) {
                p.flash_danger(
                    strip_code(&p.t_with("server/file.alerts.file_too_large.title", &[("name", &name)])),
                    Some(p.t_with(
                        "server/file.alerts.file_too_large.body",
                        &[("max", &crate::views::bytes(max, st.config.use_binary_prefix))],
                    )),
                );
                return Ok(back_to(&p, &dirname(&path)));
            }
            String::from_utf8_lossy(&r.body).into_owned()
        }
        Err(WingsError::Request { status: 404 | 400, .. }) => {
            p.flash_danger(strip_code(&p.t_with("server/file.alerts.file_not_found.title", &[("name", &name)])), None);
            return Ok(back_to(&p, &dirname(&path)));
        }
        Err(e) => {
            p.flash_danger(p.t("server/file.alerts.files_node_error.title"), Some(wings_message(&e)));
            return Ok(back_to(&p, &dirname(&path)));
        }
    };
    let special = path.ends_with(".pelicanignore").then(|| {
        context! { title => strip_code(&p.t("server/file.alerts.pelicanignore.title")), body => p.t("server/file.alerts.pelicanignore.body").replace("<code>", "").replace("</code>", "").replace("<br>", "\n") }
    });
    p.page(
        "files",
        "server/file_edit.html",
        context! {
            title => p.t_with("server/file.actions.edit.title", &[("file", &path)]),
            path => path.clone(),
            dir => dirname(&path),
            content => content,
            language => language_for(&path),
            can_update => p.can("file.update"),
            special => special,
        },
    )
}

/// `POST /server/{server}/files/edit?path=`: save / save & close.
async fn save(p: ServerPanel, Query(q): Query<PathQuery>, form: Form) -> WebResult {
    p.authorize("file.update")?;
    let path = q.path.clone().unwrap_or_default();
    let repo = Repo { p: &p };
    if let Err(e) = repo.put(&path, form.get("content").as_bytes().to_vec()).await {
        p.flash_danger(p.t("server/file.alerts.files_node_error.title"), Some(wings_message(&e)));
        return Ok(p.redirect(&format!("{}/files/edit?path={}", p.base_url(), urlencode(&path))));
    }
    let _ = p.activity_for_server("server:file.write").property("file", path.clone()).log(&p.st).await;
    if p.hx {
        return Ok(axum::http::StatusCode::NO_CONTENT.into_response());
    }
    p.session.flash("success", p.t("server/file.actions.edit.notification"), Some(path.clone()));
    if form.bool("close") {
        return Ok(back_to(&p, &dirname(&path)));
    }
    Ok(p.redirect(&format!("{}/files/edit?path={}", p.base_url(), urlencode(&path))))
}

/// Signed Wings URL (`NodeJWTService`, 15 minutes) for `download`/`upload`.
fn signed(p: &ServerPanel, kind: &str, claims: Vec<(String, Value)>) -> Result<String, WebError> {
    let st = &p.st;
    let conn = st.node_conn(&p.node).map_err(WebError::internal)?;
    let now = chrono::Utc::now().timestamp();
    let token = pelican_wings::jwt::JwtBuilder {
        issuer: st.config.app_url.clone(),
        audience: conn.connection_address(),
        identified_by: format!("{}{}", p.user.id(), p.server.uuid),
        issued_at: now,
        expires_at: now + 15 * 60,
        subject: None,
        claims,
        user_uuid: Some(p.user.user.uuid.clone()),
        scope: format!("file-{kind}"),
        unique_id: pelican_wings::jwt::str_random(16),
    }
    .sign(&conn.token)
    .map_err(WebError::internal)?;
    Ok(format!("{}/{kind}/file?token={token}", conn.connection_address()))
}

/// `GET /server/{server}/files/download?path=` (DownloadFiles): redirect to Wings.
async fn download(p: ServerPanel, Query(q): Query<PathQuery>) -> Result<Response, WebError> {
    p.authorize("file.read-content")?;
    let path = q.path.clone().unwrap_or_default();
    let url =
        signed(&p, "download", vec![("file_path".into(), json!(path)), ("server_uuid".into(), json!(p.server.uuid))])?;
    let _ = p.activity_for_server("server:file.download").property("file", path).log(&p.st).await;
    Ok(Redirect::to(&url).into_response())
}

/// `GET /server/{server}/files/upload-url`: the browser posts files straight to Wings.
async fn upload_url(p: ServerPanel, Query(q): Query<PathQuery>) -> Result<Response, WebError> {
    p.authorize("file.create")?;
    let dir = norm_dir(q.path.as_deref());
    let url = signed(&p, "upload", vec![("server_uuid".into(), json!(p.server.uuid))])?;
    Ok(Json(json!({ "url": format!("{url}&directory={}", urlencode(&dir)) })).into_response())
}

/// `POST /server/{server}/files/uploaded`: activity entry once a batch finished.
async fn uploaded(p: ServerPanel, form: Form) -> Result<Response, WebError> {
    p.authorize("file.create")?;
    let dir = norm_dir(form.opt("directory"));
    let files = form.list("files");
    if !files.is_empty() {
        let _ = p
            .activity_for_server("server:file.uploaded")
            .property("directory", dir)
            .property("files", json!(files))
            .log(&p.st)
            .await;
    }
    Ok(axum::http::StatusCode::NO_CONTENT.into_response())
}

/// `GET /server/{server}/files/search?path=&q=` (SearchFiles).
async fn search(p: ServerPanel, Query(q): Query<PathQuery>) -> WebResult {
    p.authorize("file.read")?;
    let dir = norm_dir(q.path.as_deref());
    let term = q.q.clone().unwrap_or_default();
    let valid = term.chars().count() >= 3 && term.matches('*').count() <= 1;
    let (rows, error) = if valid {
        match (Repo { p: &p }).search(&term, &dir).await {
            Ok(r) => (r, None),
            Err(e) => (Vec::new(), Some(wings_message(&e))),
        }
    } else {
        (Vec::new(), None)
    };
    p.page(
        "files",
        "server/file_search.html",
        context! {
            title => p.t_with("server/file.actions.nested_search.search_for_term", &[("term", &term)]),
            dir => dir,
            term => term,
            rows => Mj::from_serialize(&rows),
            error => error,
            can_read_content => p.can("file.read-content"),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(join_paths("/", &["a", "/b/"]), "a/b");
        assert_eq!(join_paths("", &["a"]), "a");
        assert_eq!(resolve_path("/a/./b/../c"), "a/c");
        assert_eq!(dirname("/a/b.txt"), "/a");
        assert_eq!(dirname("/b.txt"), "/");
        assert_eq!(norm_dir(Some("a/../../etc")), "/etc");
    }

    #[test]
    fn rows() {
        let r = FileRow::from_wings(
            &json!({"name": "x.jar", "file": true, "mime": "application/zip", "modified": "2024-01-01T00:00:00Z", "mode_bits": "644"}),
        );
        assert_eq!(r.mime_type, "application/java-archive");
        assert!(!r.can_edit && !r.is_archive);
        assert_eq!(r.mode_bits, 644);
        let t = FileRow::from_wings(&json!({"name": "a.txt", "file": true, "mime": "text/plain"}));
        assert!(t.can_edit);
        assert_eq!(language_for("server.yml"), "yaml");
    }
}
