//! The core crates and the terminal client hold no look: no colours, no key
//! bindings, no glyphs.
//!
//! Scans the non-test source of each core crate for hex colours, key notation in
//! string literals and non-ASCII characters in string literals. Looks and bindings
//! belong in UX packs and extensions.

use std::path::{Path, PathBuf};

const CORE: &[&str] = &[
    "spur-text",
    "spur-core",
    "spur-keymap",
    "spur-ext",
    "spur-rpc",
    "spur-server",
    "spur-tui",
];

/// Files that produce key notation (they bind nothing): exempt from that check only.
const NOTATION_MAKERS: &[&str] = &["spur-tui/src/input.rs"];

/// Key notation that marks a binding.
const KEY_NOTATION: &[&str] = &["<C-", "<A-", "<S-", "<M-", "<D-", "<Space>"];

#[test]
fn core_crates_hold_no_look() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut found = Vec::new();
    let mut scanned = 0;
    for name in CORE {
        let files = rust_files(&crates.join(name).join("src"));
        assert!(!files.is_empty(), "no source found for {name}");
        scanned += files.len();
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            let makes_notation = NOTATION_MAKERS
                .iter()
                .any(|m| rel(&file).replace('\\', "/").ends_with(m));
            for (i, line) in non_test_lines(&text) {
                for problem in problems(line) {
                    if makes_notation && problem.starts_with("key notation") {
                        continue;
                    }
                    found.push(format!("{}:{}: {problem}", rel(&file), i + 1));
                }
            }
        }
    }
    assert!(scanned >= CORE.len());
    assert!(
        found.is_empty(),
        "look in core crates:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_scanner_finds_what_it_looks_for() {
    assert_eq!(problems(r##"let c = "#1e1e2e";"##).len(), 1);
    assert_eq!(problems(r#"bind("<C-s>")"#).len(), 1);
    assert_eq!(problems(r#"let g = "→";"#).len(), 1);
    assert!(problems("// a comment about → and <C-s>").is_empty());
    assert!(problems(r#"let ok = "plain";"#).is_empty());
    assert!(problems("#[derive(Debug)]").is_empty());
    let text = "fn a() {}\n#[cfg(test)]\nmod tests { const X: &str = \"<C-x>\"; }\n";
    assert_eq!(non_test_lines(text).count(), 1);
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(rust_files(&p));
        } else if p.extension().is_some_and(|x| x == "rs")
            // Test modules kept in their own files (`#[path = "x_tests.rs"]`).
            && !p.file_name().is_some_and(|n| n.to_string_lossy().ends_with("_tests.rs"))
        {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// Lines before the first `#[cfg(test)]`, numbered from 0.
fn non_test_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .take_while(|(_, l)| l.trim() != "#[cfg(test)]")
}

fn problems(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let code = line.split("//").next().unwrap_or("");
    if has_hex_colour(code) {
        out.push(format!("hex colour: {}", line.trim()));
    }
    for lit in string_literals(code) {
        if let Some(k) = KEY_NOTATION.iter().find(|k| lit.contains(**k)) {
            out.push(format!("key notation {k}: {}", line.trim()));
        }
        if !lit.is_ascii() {
            out.push(format!("non-ASCII text: {}", line.trim()));
        }
    }
    out
}

/// `#` followed by exactly six hex digits.
fn has_hex_colour(code: &str) -> bool {
    let b = code.as_bytes();
    (0..b.len()).any(|i| {
        b[i] == b'#'
            && b.len() >= i + 7
            && b[i + 1..i + 7].iter().all(u8::is_ascii_hexdigit)
            && b.get(i + 7).is_none_or(|c| !c.is_ascii_alphanumeric())
    })
}

/// The contents of `"..."` literals on one line (escapes are skipped, not decoded).
fn string_literals(code: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = None;
    let mut escaped = false;
    for (i, c) in code.char_indices() {
        match (start, c) {
            (Some(_), '\\') if !escaped => escaped = true,
            (Some(s), '"') if !escaped => {
                out.push(&code[s..i]);
                start = None;
            }
            (None, '"') => start = Some(i + 1),
            _ => escaped = false,
        }
    }
    out
}

fn rel(p: &Path) -> String {
    let s = p.display().to_string();
    s.find("crates").map_or(s.clone(), |i| s[i..].to_string())
}
