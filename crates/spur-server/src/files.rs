//! The workspace on disk: the file tree shown in a panel and the flat file index used
//! by Ask. Reads happen when a folder is opened or Ask starts, never per key press.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use spur_rpc::{ListRow, RowKind, Span};

/// Most rows a tree shows.
const MAX_ROWS: usize = 2000;
/// Most files Ask indexes.
const MAX_INDEX: usize = 5000;
/// Folders that are never listed.
const SKIP: &[&str] = &[".git", "target", "node_modules", ".DS_Store"];

/// A visible row of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileRow {
    pub path: PathBuf,
    pub name: String,
    pub depth: u16,
    pub dir: bool,
    pub open: bool,
}

/// One client's file tree: which folders are open and which row is selected.
#[derive(Debug)]
pub(crate) struct Tree {
    root: PathBuf,
    expanded: BTreeSet<PathBuf>,
    pub rows: Vec<FileRow>,
    pub selected: usize,
}

/// Entries of `dir`: folders first, then by name. Unreadable folders are empty.
fn entries(dir: &Path) -> Vec<(String, PathBuf, bool)> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<_> = read
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if SKIP.contains(&name.as_str()) {
                return None;
            }
            // `file_type` does not follow links, so a link loop cannot recurse.
            let dir = e.file_type().is_ok_and(|t| t.is_dir());
            Some((name, e.path(), dir))
        })
        .collect();
    out.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    out
}

impl Tree {
    /// A tree of `root` with only the top level open.
    pub(crate) fn new(root: &Path) -> Self {
        let mut t = Self {
            root: root.to_path_buf(),
            expanded: BTreeSet::new(),
            rows: Vec::new(),
            selected: 0,
        };
        t.refresh();
        t
    }

    /// Re-reads the open folders.
    pub(crate) fn refresh(&mut self) {
        let keep = self.rows.get(self.selected).map(|r| r.path.clone());
        self.rows.clear();
        let root = self.root.clone();
        self.walk(&root, 0);
        self.selected = keep
            .and_then(|p| self.rows.iter().position(|r| r.path == p))
            .unwrap_or_else(|| self.selected.min(self.rows.len().saturating_sub(1)));
    }

    fn walk(&mut self, dir: &Path, depth: u16) {
        for (name, path, is_dir) in entries(dir) {
            if self.rows.len() >= MAX_ROWS {
                return;
            }
            let open = is_dir && self.expanded.contains(&path);
            self.rows.push(FileRow {
                path: path.clone(),
                name,
                depth,
                dir: is_dir,
                open,
            });
            if open {
                self.walk(&path, depth + 1);
            }
        }
    }

    /// Moves the selection, stopping at the ends.
    pub(crate) fn move_by(&mut self, delta: i32) {
        let last = self.rows.len().saturating_sub(1);
        let step = usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX);
        self.selected = if delta < 0 {
            self.selected.saturating_sub(step)
        } else {
            self.selected.saturating_add(step).min(last)
        };
    }

    /// Opens (`Some(true)`), closes (`Some(false)`) or toggles the selected folder.
    /// Returns the file to open if the selected row is a file and `open` is not `false`.
    pub(crate) fn activate(&mut self, open: Option<bool>) -> Option<PathBuf> {
        let row = self.rows.get(self.selected)?.clone();
        if !row.dir {
            return (open != Some(false)).then_some(row.path);
        }
        let want = open.unwrap_or(!row.open);
        if want {
            self.expanded.insert(row.path);
        } else {
            self.expanded.remove(&row.path);
        }
        self.refresh();
        None
    }

    /// Opens the folders above `path` and selects it, if it is inside the root.
    pub(crate) fn reveal(&mut self, path: &Path) {
        let Ok(rel) = path.strip_prefix(&self.root) else {
            return;
        };
        let mut dir = self.root.clone();
        let parts: Vec<_> = rel.components().collect();
        for part in parts.iter().take(parts.len().saturating_sub(1)) {
            dir.push(part);
            self.expanded.insert(dir.clone());
        }
        self.refresh();
        if let Some(i) = self.rows.iter().position(|r| r.path == path) {
            self.selected = i;
        }
    }

    /// The tree as list rows.
    pub(crate) fn list_rows(&self) -> Vec<ListRow> {
        self.rows
            .iter()
            .map(|r| ListRow {
                kind: RowKind::Item,
                indent: r.depth,
                icon: match (r.dir, r.open) {
                    (true, true) => "▾".into(),
                    (true, false) => "▸".into(),
                    (false, _) => " ".into(),
                },
                spans: vec![Span {
                    text: r.name.clone(),
                    style: r.dir.then(|| "list.folder".to_owned()),
                }],
                detail: String::new(),
                hint: String::new(),
            })
            .collect()
    }
}

/// Files under `root` as `/`-separated relative paths, nearest first. Hidden entries and
/// build folders are skipped.
pub(crate) fn index(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut queue = VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = queue.pop_front() {
        for (name, path, is_dir) in entries(&dir) {
            if name.starts_with('.') {
                continue;
            }
            if is_dir {
                queue.push_back(path);
            } else if let Ok(rel) = path.strip_prefix(root) {
                let parts: Vec<_> = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect();
                out.push(parts.join("/"));
                if out.len() >= MAX_INDEX {
                    return out;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spur-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/deep")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("b.txt"), "").unwrap();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        std::fs::write(dir.join("src/main.rs"), "").unwrap();
        std::fs::write(dir.join("src/deep/x.rs"), "").unwrap();
        std::fs::write(dir.join(".hidden"), "").unwrap();
        dir
    }

    fn names(t: &Tree) -> Vec<&str> {
        t.rows.iter().map(|r| &*r.name).collect()
    }

    #[test]
    fn tree_opens_folders_and_keeps_the_selection() {
        let dir = fixture("tree");
        let mut t = Tree::new(&dir);
        assert_eq!(names(&t), vec!["src", ".hidden", "a.txt", "b.txt"]);
        assert_eq!(t.activate(None), None);
        assert_eq!(
            names(&t),
            vec!["src", "deep", "main.rs", ".hidden", "a.txt", "b.txt"]
        );
        t.move_by(2);
        assert_eq!(t.activate(None), Some(dir.join("src/main.rs")));
        assert_eq!(t.activate(Some(false)), None, "closing a file does nothing");
        t.move_by(-100);
        t.activate(Some(false));
        assert_eq!(names(&t).len(), 4);
        t.move_by(100);
        assert_eq!(t.selected, 3);

        t.reveal(&dir.join("src/deep/x.rs"));
        assert_eq!(t.rows[t.selected].name, "x.rs");
        assert_eq!(t.rows[t.selected].depth, 2);
        assert_eq!(t.list_rows()[0].icon, "▾");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn index_lists_files_nearest_first() {
        let dir = fixture("index");
        assert_eq!(
            index(&dir),
            vec!["a.txt", "b.txt", "src/main.rs", "src/deep/x.rs"]
        );
        assert!(index(&dir.join("missing")).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
