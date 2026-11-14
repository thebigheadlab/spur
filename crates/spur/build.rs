//! Embeds the default UX pack (`extensions/ux_fresh`) into the binary.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read the UX pack folder {}: {e}", dir.display()));
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = e.path();
        let r = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        if path.is_dir() {
            walk(&path, &r, out);
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
        {
            out.push((r, path));
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let pack = manifest.join("../../extensions/ux_fresh");
    println!("cargo:rerun-if-changed={}", pack.display());
    let mut files = Vec::new();
    walk(&pack, "", &mut files);
    files.sort();
    assert!(
        files.iter().any(|(rel, _)| rel == "pack.toml"),
        "no pack.toml in {}",
        pack.display()
    );
    let mut code = String::from("/// The `ux_fresh` pack files: relative path, text.\n");
    code.push_str("pub static UX_FRESH: &[(&str, &str)] = &[\n");
    for (rel, path) in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        let abs = path.canonicalize().unwrap_or_else(|_| path.clone());
        let _ = writeln!(
            code,
            "    ({rel:?}, include_str!({:?})),",
            abs.display().to_string()
        );
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default()).join("ux_fresh.rs");
    if let Err(e) = std::fs::write(&out, code) {
        panic!("cannot write {}: {e}", out.display());
    }
}
