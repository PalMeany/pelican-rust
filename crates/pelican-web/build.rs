//! Embed `templates/**` and `assets/**` into the binary (ADR-0013).

use std::fmt::Write as _;
use std::path::Path;

fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.path());
    for e in entries {
        let p = e.path();
        if p.is_dir() {
            walk(&p, base, out);
        } else if let Ok(rel) = p.strip_prefix(base) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut code = String::new();
    for (name, dir) in [("TEMPLATES", "templates"), ("ASSETS", "assets")] {
        let base = root.join(dir);
        println!("cargo:rerun-if-changed={}", base.display());
        let mut files = Vec::new();
        walk(&base, &base, &mut files);
        let _ = writeln!(code, "pub static {name}: &[(&str, &[u8])] = &[");
        for f in &files {
            let _ =
                writeln!(code, "    ({f:?}, include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/{dir}/{f}\"))),");
        }
        let _ = writeln!(code, "];");
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default()).join("embedded.rs");
    let _ = std::fs::write(out, code);
}
