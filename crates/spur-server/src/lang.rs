//! File extension to language id. Packs label the ids (`[languages]`); highlighting
//! and, later, tree-sitter key off them.

use std::path::Path;

/// Every language id, in the order packs usually list them.
pub const IDS: &[&str] = &[
    "c",
    "cpp",
    "go",
    "java",
    "javascript",
    "json",
    "markdown",
    "python",
    "rust",
    "shell",
    "text",
    "toml",
    "typescript",
    "yaml",
];

/// The language id of a file, by its extension; `text` when unknown or absent.
#[allow(dead_code)] // used by the UI model (next phase)
pub fn of_path(path: Option<&Path>) -> &'static str {
    let ext = path
        .and_then(Path::extension)
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("rs") => "rust",
        Some("toml") => "toml",
        Some("md" | "markdown") => "markdown",
        Some("c" | "h") => "c",
        Some("cc" | "cpp" | "cxx" | "hpp" | "hh") => "cpp",
        Some("js" | "mjs" | "cjs") => "javascript",
        Some("ts" | "tsx") => "typescript",
        Some("go") => "go",
        Some("java") => "java",
        Some("py") => "python",
        Some("sh" | "bash" | "zsh") => "shell",
        Some("json") => "json",
        Some("yaml" | "yml") => "yaml",
        _ => "text",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_map_to_known_ids() {
        for (p, id) in [
            ("a.rs", "rust"),
            ("Cargo.toml", "toml"),
            ("README.MD", "markdown"),
            ("x.hpp", "cpp"),
            ("y.tsx", "typescript"),
            ("Makefile", "text"),
            ("z.unknown", "text"),
        ] {
            assert_eq!(of_path(Some(Path::new(p))), id, "{p}");
            assert!(IDS.contains(&id));
        }
        assert_eq!(of_path(None), "text");
    }
}
