//! `PluginService` lifecycle: safe zip/URL import, install, update, uninstall,
//! enable/disable, delete and load order. Composer, yarn and PHP migrations are PHP-runtime
//! steps with no equivalent here (ADR-0018); a WASM plugin's own install/uninstall hooks run
//! in the host around these calls.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::manifest::{self, read_manifest};
use crate::{Config, Error, Plugin, Result, Status, updates};

/// `round($maxSize / 1024 / 1024, 2)` as PHP prints it.
fn mib_label(bytes: u64) -> String {
    let v = (bytes as f64 / 1024.0 / 1024.0 * 100.0).round() / 100.0;
    if v.fract() == 0.0 { format!("{}", v as i64) } else { format!("{v}") }
}

fn random16() -> String {
    use rand::Rng;
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..16).map(|_| CHARS[rng.random_range(0..CHARS.len())] as char).collect()
}

/// `TemporaryDirectory(...)->deleteWhenDestroyed()`.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `locatePluginManifest()`: the archive root, else the first directory (by name) holding one.
fn locate_manifest(path: &Path) -> Option<PathBuf> {
    let root = path.join("plugin.json");
    if root.is_file() {
        return Some(root);
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(path)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs.into_iter().map(|d| d.join("plugin.json")).find(|m| m.is_file())
}

fn move_dir(from: &Path, to: &Path) -> bool {
    std::fs::rename(from, to).is_ok()
}

/// `downloadPluginFromFile()`: validate (size, traversal, uncompressed total), extract under
/// `plugins/.import-*`, find the manifest, check the id, carry over `meta` when replacing an
/// existing copy (kept as `plugins/.<id>.bak` until the new one is in place). Returns the id.
pub fn import_zip(cfg: &Config, zip_path: &Path, expected: Option<&str>) -> Result<String> {
    let label = mib_label(cfg.max_import_size);
    let size = std::fs::metadata(zip_path).map(|m| m.len()).unwrap_or(0);
    if size > cfg.max_import_size {
        return Err(Error::Message(format!("Zip file too large. ({label} MiB)")));
    }
    let file = std::fs::File::open(zip_path).map_err(|_| Error::Message("Could not open zip file.".into()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|_| Error::Message("Could not open zip file.".into()))?;
    let mut total: u64 = 0;
    for i in 0..archive.len() {
        let entry = archive.by_index_raw(i).map_err(|_| Error::Message("Could not open zip file.".into()))?;
        let name = entry.name();
        if name.contains("..") || name.starts_with('/') {
            return Err(Error::Message("Zip file contains invalid path traversal sequences.".into()));
        }
        total += entry.size();
        if total > cfg.max_import_size {
            return Err(Error::Message(format!("Zip file contents too large. ({label} MiB)")));
        }
    }
    std::fs::create_dir_all(&cfg.root).map_err(Error::io)?;
    let tmp = TempDir(cfg.root.join(format!(".import-{}", random16())));
    let extract = tmp.0.join("contents");
    std::fs::create_dir_all(&extract).map_err(Error::io)?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|_| Error::Message("Could not extract zip file.".into()))?;
        let Some(rel) = entry.enclosed_name() else {
            return Err(Error::Message("Zip file contains invalid path traversal sequences.".into()));
        };
        // Symbolic links are not extracted (they could point outside the plugin).
        if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            continue;
        }
        let out = extract.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(Error::io)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(Error::io)?;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).map_err(|_| Error::Message("Could not extract zip file.".into()))?;
        std::fs::write(&out, buf).map_err(|_| Error::Message("Could not extract zip file.".into()))?;
    }
    let t = |k: &str, r: &[(&str, &str)]| (cfg.translate)(k, r);
    let no_manifest = || Error::Message(t("admin/plugin.notifications.import_no_manifest", &[]));
    let manifest_path = locate_manifest(&extract).ok_or_else(no_manifest)?;
    let mut data: Map<String, Value> = read_manifest(&manifest_path)?;
    let id = match data.get("id") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_lowercase(),
        _ => return Err(no_manifest()),
    };
    let valid = regex::Regex::new(r"^[a-z0-9][a-z0-9._-]*$").map(|r| r.is_match(&id)).unwrap_or(false);
    if !valid {
        return Err(Error::Message(t("admin/plugin.notifications.import_invalid_id", &[])));
    }
    if let Some(expected) = expected {
        if id != expected {
            return Err(Error::Message(t(
                "admin/plugin.notifications.import_id_mismatch",
                &[("expected", expected), ("actual", &id)],
            )));
        }
    }
    let target = cfg.root.join(&id);
    let source = manifest_path.parent().map(Path::to_path_buf).unwrap_or_else(|| extract.clone());
    let mut rollback: Option<PathBuf> = None;
    if target.is_dir() {
        let old = target.join("plugin.json");
        let meta = if old.exists() {
            read_manifest(&old)
                .ok()
                .and_then(|m| m.get("meta").cloned())
                .map(pelican_core::server_config::php_assoc)
                .unwrap_or(Value::Array(vec![]))
        } else {
            Value::Array(vec![])
        };
        let empty =
            matches!(&meta, Value::Array(a) if a.is_empty()) || matches!(&meta, Value::Object(o) if o.is_empty());
        if empty {
            data.remove("meta");
        } else {
            data.insert("meta".into(), meta);
        }
        std::fs::write(&manifest_path, manifest::encode_manifest(&Value::Object(data))?)
            .map_err(|_| Error::Message("Could not write the plugin manifest.".into()))?;
        let backup = cfg.root.join(format!(".{id}.bak"));
        let _ = std::fs::remove_dir_all(&backup);
        if !move_dir(&target, &backup) {
            return Err(Error::Message("Could not set the existing plugin aside.".into()));
        }
        rollback = Some(backup);
    }
    if !move_dir(&source, &target) {
        if let Some(b) = &rollback {
            move_dir(b, &target);
        }
        return Err(Error::Message("Could not move plugin into place.".into()));
    }
    if let Some(b) = rollback {
        let _ = std::fs::remove_dir_all(b);
    }
    Ok(id)
}

/// `downloadPluginFromUrl()`: fetch (Hub key for the Hub), check the advertised sha256 and
/// the size, then import the zip.
pub async fn import_url(cfg: &Config, url: &str, expected: Option<&str>, sha256: Option<&str>) -> Result<String> {
    let basename = url.rsplit('/').next().filter(|b| !b.is_empty()).unwrap_or("plugin.zip").to_string();
    let body = async {
        let resp = updates::client(cfg, url, Duration::from_secs(60), Duration::from_secs(5))?.get(url).send().await?;
        resp.error_for_status()?.bytes().await
    }
    .await
    .map_err(|e| Error::Message(e.to_string()))?;
    if let Some(expected) = sha256 {
        let actual = hex::encode(Sha256::digest(&body));
        if expected.to_lowercase() != actual {
            return Err(Error::InvalidUpload((cfg.translate)(
                "admin/plugin.notifications.import_checksum_mismatch",
                &[],
            )));
        }
    }
    if body.len() as u64 > cfg.max_import_size {
        return Err(Error::InvalidUpload(format!("Zip file too large. ({} MiB)", mib_label(cfg.max_import_size))));
    }
    let dir = std::env::temp_dir().join(format!("pelican-plugin-{}", random16()));
    std::fs::create_dir_all(&dir).map_err(|_| Error::InvalidUpload("Could not write temporary file.".into()))?;
    let tmp = TempDir(dir);
    let path = tmp.0.join(&basename);
    std::fs::write(&path, &body).map_err(|_| Error::InvalidUpload("Could not write temporary file.".into()))?;
    import_zip(cfg, &path, expected)
}

/// `isInstalled($id)` after a re-scan.
pub fn is_installed(cfg: &Config, id: &str) -> Result<bool> {
    Ok(cfg.find(id)?.status != Status::NotInstalled)
}

/// `installPlugin($plugin, $enable)`.
pub fn install(cfg: &Config, p: &Plugin, enable: bool) -> Result<()> {
    let result = if enable {
        cfg.set_status(p, Status::Enabled, None)
    } else if p.status == Status::NotInstalled {
        cfg.set_status(p, Status::Disabled, None)
    } else {
        Ok(())
    };
    if let Err(e) = &result {
        let _ = cfg.set_status(p, Status::NotInstalled, Some(e.to_string()));
    }
    result
}

/// `uninstallPlugin($plugin, $deleteFiles)`.
pub fn uninstall(cfg: &Config, p: &Plugin, delete_files: bool) -> Result<()> {
    cfg.set_status(p, Status::NotInstalled, None)?;
    if delete_files {
        delete(cfg, p)?;
    }
    Ok(())
}

/// `deletePlugin()`.
pub fn delete(cfg: &Config, p: &Plugin) -> Result<()> {
    let dir = cfg.root.join(&p.dir);
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(Error::io)?;
    }
    Ok(())
}

pub fn enable(cfg: &Config, p: &Plugin) -> Result<()> {
    cfg.set_status(p, Status::Enabled, None)
}

pub fn disable(cfg: &Config, p: &Plugin) -> Result<()> {
    cfg.set_status(p, Status::Disabled, None)
}

/// `updatePlugin($plugin)`: download the feed's zip (checked against its sha256), then
/// reinstall unless it was never installed.
pub async fn update(cfg: &Config, p: &Plugin) -> Result<Plugin> {
    let (url, sha) =
        updates::update_download(cfg, p).await.ok_or_else(|| Error::Message("No download url found.".into()))?;
    import_url(cfg, &url, Some(&p.id), sha.as_deref()).await?;
    let fresh = cfg.find(&p.id)?;
    if fresh.status != Status::NotInstalled {
        install(cfg, &fresh, false)?;
    }
    updates::forget(&p.id);
    cfg.find(&p.id)
}

/// `updateLoadOrder($order)`.
pub fn update_load_order(cfg: &Config, order: &[String]) -> Result<()> {
    for (i, id) in order.iter().enumerate() {
        let p = cfg.find(id)?;
        manifest::set_meta(&cfg.root, &p.dir, &[("load_order", Value::from(i as i64))])?;
    }
    Ok(())
}

/// `hasThemePluginEnabled()`.
pub fn has_theme_enabled(cfg: &Config) -> bool {
    cfg.plugins().iter().any(|p| p.is_theme() && p.status == Status::Enabled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn cfg(root: &Path) -> Config {
        Config {
            root: root.to_path_buf(),
            dev_mode: false,
            safe_mode: false,
            max_import_size: 1024 * 1024,
            hub_url: "https://hub.pelican.dev".into(),
            hub_api_key: None,
            panel_version: None,
            panel_version_display: "canary".into(),
            production: true,
            translate: std::sync::Arc::new(crate::english),
        }
    }

    fn zip(entries: &[(&str, &str)]) -> tempfile::NamedTempFile {
        let f = tempfile::NamedTempFile::new().expect("tmp");
        let mut w = zip::ZipWriter::new(f.reopen().expect("reopen"));
        for (name, body) in entries {
            w.start_file(*name, zip::write::SimpleFileOptions::default()).expect("start");
            w.write_all(body.as_bytes()).expect("write");
        }
        w.finish().expect("finish");
        f
    }

    const MANIFEST: &str = r#"{"id":"Demo","name":"Demo","author":"Me","category":"plugin","namespace":"Me\\Demo","class":"DemoPlugin","meta":{"status":"enabled"}}"#;

    #[test]
    fn import_nested_and_replace_keeps_meta() {
        let tmp = tempfile::tempdir().expect("tmp");
        let c = cfg(tmp.path());
        let z = zip(&[("demo-1.0/plugin.json", MANIFEST), ("demo-1.0/README.md", "hi")]);
        assert_eq!(import_zip(&c, z.path(), None).expect("import"), "demo");
        assert!(tmp.path().join("demo/README.md").exists());
        // The archive's own meta stays on a fresh import.
        assert_eq!(c.find("demo").expect("row").status, Status::Enabled);
        // Re-import over an install: keep the current meta.
        manifest::set_meta(tmp.path(), "demo", &[("status", "disabled".into()), ("load_order", 3.into())])
            .expect("meta");
        let z2 = zip(&[("plugin.json", MANIFEST)]);
        import_zip(&c, z2.path(), Some("demo")).expect("reimport");
        let p = c.find("demo").expect("row");
        assert_eq!((p.status, p.load_order), (Status::Disabled, 3));
        assert!(!tmp.path().join(".demo.bak").exists());
        assert_eq!(std::fs::read_dir(tmp.path()).expect("ls").count(), 1, "temp dirs removed");
    }

    #[test]
    fn rejects_traversal_mismatch_and_missing_manifest() {
        let tmp = tempfile::tempdir().expect("tmp");
        let c = cfg(tmp.path());
        let bad = zip(&[("../evil/plugin.json", MANIFEST)]);
        assert_eq!(
            import_zip(&c, bad.path(), None).unwrap_err().to_string(),
            "Zip file contains invalid path traversal sequences."
        );
        let other = zip(&[("plugin.json", MANIFEST)]);
        assert_eq!(
            import_zip(&c, other.path(), Some("other")).unwrap_err().to_string(),
            "The zip is for plugin \"demo\", not \"other\""
        );
        let empty = zip(&[("readme.txt", "x")]);
        assert_eq!(
            import_zip(&c, empty.path(), None).unwrap_err().to_string(),
            "The zip does not contain a valid plugin.json"
        );
        let big = zip(&[("plugin.json", &"x".repeat(2 * 1024 * 1024))]);
        assert_eq!(import_zip(&c, big.path(), None).unwrap_err().to_string(), "Zip file contents too large. (1 MiB)");
    }

    #[test]
    fn install_states() {
        let tmp = tempfile::tempdir().expect("tmp");
        let c = cfg(tmp.path());
        let z = zip(&[("plugin.json", &MANIFEST.replace(r#","meta":{"status":"enabled"}"#, ""))]);
        import_zip(&c, z.path(), None).expect("import");
        let p = c.find("demo").expect("row");
        assert_eq!(p.status, Status::NotInstalled);
        install(&c, &p, false).expect("install");
        assert_eq!(c.find("demo").expect("row").status, Status::Disabled);
        install(&c, &c.find("demo").expect("row"), true).expect("install");
        assert_eq!(c.find("demo").expect("row").status, Status::Enabled);
        uninstall(&c, &c.find("demo").expect("row"), true).expect("uninstall");
        assert!(c.find("demo").is_err());
    }
}
