//! The `workspace.files` list: the workspace folder as a tree, read lazily. It lives
//! in the server until the extension host exists.
//!
//! Listings are cached per session; each client's tree keeps its own expanded
//! folders, selection and scroll, and a flattened list of visible rows rebuilt only
//! when a folder it shows is expanded, collapsed or read again.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use spur_rpc::{Item, ItemKind, ListView};

use super::pack::Pack;

/// Most entries read from one folder; the rest show as one "N more" row.
pub const MAX_ENTRIES: usize = 10_000;

/// One folder entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Name (lossy if not UTF-8).
    pub name: String,
    /// A real folder (not a symlink to one).
    pub dir: bool,
    /// The name was valid UTF-8 (otherwise it cannot be opened).
    pub utf8: bool,
    /// A symlink to a folder: listed as an entry, never followed or opened.
    pub link_dir: bool,
}

/// What reading a folder gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Listing {
    /// Entries, sorted folders first then by name; `more` were not read.
    Read {
        /// Entries.
        entries: Vec<Entry>,
        /// Entries past the cap.
        more: usize,
    },
    /// Why it could not be read.
    Failed(String),
}

/// Session-wide listing cache.
#[derive(Debug)]
pub struct Files {
    root: PathBuf,
    dirs: BTreeMap<String, Listing>,
    /// Rises whenever a listing changes.
    pub generation: u64,
}

impl Files {
    /// A cache for `root`.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            dirs: BTreeMap::new(),
            generation: 0,
        }
    }

    /// The workspace root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The listing of `dir` (relative, `""` for the root), read on first use.
    pub fn listing(&mut self, dir: &str) -> &Listing {
        if !self.dirs.contains_key(dir) {
            let l = read(&self.root.join(dir));
            self.dirs.insert(dir.to_owned(), l);
            self.generation += 1;
        }
        &self.dirs[dir]
    }

    /// Reads `dir` again if it was read before.
    pub fn reread(&mut self, dir: &str) {
        if self.dirs.contains_key(dir) {
            let l = read(&self.root.join(dir));
            if self.dirs.get(dir) != Some(&l) {
                self.dirs.insert(dir.to_owned(), l);
                self.generation += 1;
            }
        }
    }

    /// Reads every folder read before again (some saves were missed).
    pub fn reread_all(&mut self) {
        let dirs: Vec<String> = self.dirs.keys().cloned().collect();
        for d in dirs {
            self.reread(&d);
        }
    }

    /// The relative id of an absolute path under the root.
    pub fn rel(&self, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(&self.root).ok()?;
        let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
        Some(parts?.join("/"))
    }
}

fn read(path: &Path) -> Listing {
    read_capped(path, MAX_ENTRIES)
}

fn read_capped(path: &Path, cap: usize) -> Listing {
    let rd = match std::fs::read_dir(path) {
        Ok(rd) => rd,
        Err(e) => return Listing::Failed(e.to_string()),
    };
    let mut entries = Vec::new();
    let mut more = 0;
    for e in rd {
        let Ok(e) = e else { continue };
        let os = e.file_name();
        if os.as_encoded_bytes().first() == Some(&b'.') {
            continue;
        }
        if entries.len() >= cap {
            more += 1;
            continue;
        }
        let (name, utf8) = match os.to_str() {
            Some(s) => (s.to_owned(), true),
            None => (os.to_string_lossy().into_owned(), false),
        };
        // `file_type` does not follow symlinks, so a link to a folder is a leaf.
        let ty = e.file_type().ok();
        let dir = ty.is_some_and(|t| t.is_dir());
        let link_dir = ty.is_some_and(|t| t.is_symlink())
            && std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir());
        entries.push(Entry {
            name,
            dir,
            utf8,
            link_dir,
        });
    }
    entries.sort_by_cached_key(|e| (!e.dir, e.name.to_lowercase(), e.name.clone()));
    Listing::Read { entries, more }
}

/// One visible row of a tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// Item id: the path relative to the root, or `#more:<dir>` / `#error:<dir>`.
    pub id: String,
    /// Name, or the count / reason for those rows.
    pub name: String,
    /// Depth.
    pub depth: u32,
    /// Kind.
    pub kind: ItemKind,
    /// Folder shown open.
    pub open: bool,
    /// Can be opened (a file with a UTF-8 path).
    pub openable: bool,
}

/// One client's Files tree.
#[derive(Clone, Debug, Default)]
pub struct Tree {
    /// Expanded folders.
    pub expanded: BTreeSet<String>,
    /// Selected row id.
    pub selected: Option<String>,
    /// First visible row.
    pub offset: u32,
    rows: Vec<Row>,
    built: Option<(u64, u64)>,
    generation: u64,
}

impl Tree {
    /// A tree with the folders above `id` expanded and `id` selected.
    pub fn revealing(files: &mut Files, id: Option<&str>) -> Self {
        let mut t = Self::default();
        if let Some(id) = id {
            let parts: Vec<&str> = id.split('/').collect();
            for i in 1..parts.len() {
                t.expanded.insert(parts[..i].join("/"));
            }
            t.selected = Some(id.to_owned());
        }
        t.rows(files);
        if t.index_of_selected().is_none() {
            t.selected = t.rows.first().map(|r| r.id.clone());
        }
        t
    }

    /// The visible rows, rebuilt if a listing or the expanded set changed.
    pub fn rows(&mut self, files: &mut Files) -> &[Row] {
        // Reading folders while building may raise the generation; build until stable.
        while self.built != Some((files.generation, self.generation)) {
            let mut rows = Vec::new();
            push_rows(files, &self.expanded, "", 0, &mut rows);
            self.rows = rows;
            self.built = Some((files.generation, self.generation));
            self.fix_selection();
        }
        &self.rows
    }

    fn fix_selection(&mut self) {
        let Some(sel) = self.selected.clone() else {
            self.selected = self.rows.first().map(|r| r.id.clone());
            return;
        };
        if self.rows.iter().any(|r| r.id == sel) {
            return;
        }
        // The nearest folder above it still shown.
        let mut id = row_parent(&sel);
        while !id.is_empty() {
            if self.rows.iter().any(|r| r.id == id) {
                self.selected = Some(id.to_owned());
                return;
            }
            id = parent_of(id);
        }
        self.selected = self.rows.first().map(|r| r.id.clone());
    }

    /// Index of the selected row.
    pub fn index_of_selected(&self) -> Option<usize> {
        let sel = self.selected.as_deref()?;
        self.rows.iter().position(|r| r.id == sel)
    }

    /// The selected row.
    pub fn selected_row(&self) -> Option<&Row> {
        self.index_of_selected().map(|i| &self.rows[i])
    }

    /// Moves the selection by `by` rows, clamped, keeping it in a window of `height`
    /// rows with a one-row margin.
    pub fn move_by(&mut self, files: &mut Files, by: i64, height: u32) {
        let n = self.rows(files).len();
        if n == 0 {
            return;
        }
        let cur = self.index_of_selected().unwrap_or(0);
        let last = i64::try_from(n - 1).unwrap_or(i64::MAX);
        let to = i64::try_from(cur)
            .unwrap_or(0)
            .saturating_add(by)
            .clamp(0, last);
        let to = usize::try_from(to).unwrap_or(0);
        self.selected = Some(self.rows[to].id.clone());
        self.keep_visible(to, height);
    }

    fn keep_visible(&mut self, index: usize, height: u32) {
        let h = height.max(1);
        let i = u32::try_from(index).unwrap_or(u32::MAX);
        let margin = u32::from(h > 2);
        if i < self.offset + margin {
            self.offset = i.saturating_sub(margin);
        } else if i + margin >= self.offset + h {
            self.offset = (i + margin + 1).saturating_sub(h);
        }
    }

    /// Selects `id` if it is a visible row.
    pub fn select(&mut self, id: &str) -> bool {
        if self.rows.iter().any(|r| r.id == id) {
            self.selected = Some(id.to_owned());
            true
        } else {
            false
        }
    }

    /// Expands the selected folder, reading it again. False if not a closed folder.
    pub fn expand(&mut self, files: &mut Files) -> bool {
        let Some(r) = self.selected_row().cloned() else {
            return false;
        };
        if r.kind != ItemKind::Dir || r.open {
            return false;
        }
        self.expanded.insert(r.id.clone());
        files.reread(&r.id);
        self.generation += 1;
        true
    }

    /// Collapses the selected open folder; on anything else selects its parent.
    pub fn collapse(&mut self) {
        let Some(r) = self.selected_row().cloned() else {
            return;
        };
        if r.kind == ItemKind::Dir && r.open {
            self.expanded.remove(&r.id);
            self.generation += 1;
        } else if r.depth > 0 {
            let parent = row_parent(&r.id);
            if !parent.is_empty() {
                self.selected = Some(parent.to_owned());
            }
        }
    }

    /// Activates the selected row: a folder toggles, a file asks to be opened.
    pub fn activate(&mut self, files: &mut Files) -> Activate {
        self.rows(files);
        let Some(r) = self.selected_row().cloned() else {
            return Activate::Nothing;
        };
        match r.kind {
            ItemKind::Dir if r.open => {
                self.collapse();
                Activate::Toggled
            }
            ItemKind::Dir => {
                self.expand(files);
                Activate::Toggled
            }
            ItemKind::Entry if r.openable => Activate::Open(files.root().join(&r.id)),
            _ => Activate::Nothing,
        }
    }

    /// The window of `height` rows, offset clamped; `current` is the row id of
    /// the document the client shows.
    pub fn window(
        &mut self,
        files: &mut Files,
        height: u32,
        pack: &Pack,
        current: Option<&str>,
    ) -> ListView {
        let ascii = pack.ascii_icons;
        let sel = {
            self.rows(files);
            self.index_of_selected()
        };
        let total = u32::try_from(self.rows.len()).unwrap_or(u32::MAX);
        let h = height.max(1);
        self.offset = self.offset.min(total.saturating_sub(h));
        let icon = |name: &str| pack.icons.get(name);
        let items = self
            .rows
            .iter()
            .skip(self.offset as usize)
            .take(h as usize)
            .map(|r| {
                let i = match r.kind {
                    ItemKind::Dir if r.open => icon("folder_open"),
                    ItemKind::Dir => icon("folder"),
                    ItemKind::Entry => icon("file"),
                    _ => None,
                };
                let (glyph, alt) = match i {
                    Some(i) if ascii => (Some(i.ascii.clone()), None),
                    Some(i) => (Some(i.glyph.clone()), Some(i.ascii.clone())),
                    None => (None, None),
                };
                let text = match r.kind {
                    ItemKind::More => pack.message("list.more", &[("count", &r.name)]),
                    ItemKind::Error => pack.message("list.unreadable", &[("reason", &r.name)]),
                    _ => r.name.clone(),
                };
                Item {
                    id: r.id.clone(),
                    text,
                    glyph,
                    alt,
                    depth: r.depth,
                    kind: r.kind,
                    open: r.open,
                    current: r.kind == ItemKind::Entry && current == Some(r.id.as_str()),
                }
            })
            .collect();
        ListView {
            offset: self.offset,
            total,
            selected: sel.map(|i| u32::try_from(i).unwrap_or(u32::MAX)),
            items,
        }
    }

    /// Scrolls the window by `lines`; the selection stays.
    pub fn scroll(&mut self, lines: i32, height: u32) {
        let total = u32::try_from(self.rows.len()).unwrap_or(u32::MAX);
        let max = total.saturating_sub(height.max(1));
        let o = i64::from(self.offset) + i64::from(lines);
        self.offset = u32::try_from(o.clamp(0, i64::from(max))).unwrap_or(0);
    }
}

/// What activating a row asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activate {
    /// Nothing happens.
    Nothing,
    /// A folder was expanded or collapsed.
    Toggled,
    /// Open this file.
    Open(PathBuf),
}

/// The parent folder id of a path id (`""` at the top).
pub fn parent_of(id: &str) -> &str {
    id.rfind('/').map_or("", |i| &id[..i])
}

/// The folder a row sits in: its parent, or for a "more" or "unreadable" row the
/// folder it reports on.
pub fn row_parent(id: &str) -> &str {
    id.strip_prefix("#more:")
        .or_else(|| id.strip_prefix("#error:"))
        .unwrap_or_else(|| parent_of(id))
}

fn push_rows(
    files: &mut Files,
    expanded: &BTreeSet<String>,
    dir: &str,
    depth: u32,
    out: &mut Vec<Row>,
) {
    let listing = files.listing(dir).clone();
    match listing {
        Listing::Failed(reason) => out.push(Row {
            id: format!("#error:{dir}"),
            name: reason,
            depth,
            kind: ItemKind::Error,
            open: false,
            openable: false,
        }),
        Listing::Read { entries, more } => {
            for e in entries {
                let id = if dir.is_empty() {
                    e.name.clone()
                } else {
                    format!("{dir}/{}", e.name)
                };
                let open = e.dir && e.utf8 && expanded.contains(&id);
                out.push(Row {
                    id: id.clone(),
                    name: e.name,
                    depth,
                    kind: if e.dir {
                        ItemKind::Dir
                    } else {
                        ItemKind::Entry
                    },
                    open,
                    openable: !e.dir && e.utf8 && !e.link_dir,
                });
                if open {
                    push_rows(files, expanded, &id, depth + 1, out);
                }
            }
            if more > 0 {
                out.push(Row {
                    id: format!("#more:{dir}"),
                    name: more.to_string(),
                    depth,
                    kind: ItemKind::More,
                    open: false,
                    openable: false,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("spur-files-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn listings_sort_folders_first_hide_dot_files_and_cap() {
        let d = temp("sort");
        for f in ["b.txt", "A.txt", ".hidden", "c.txt"] {
            std::fs::write(d.join(f), "").unwrap();
        }
        std::fs::create_dir(d.join("zdir")).unwrap();
        let Listing::Read { entries, more } = read_capped(&d, 10) else {
            panic!("unreadable");
        };
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["zdir", "A.txt", "b.txt", "c.txt"]);
        assert_eq!(more, 0);
        let Listing::Read { entries, more } = read_capped(&d, 3) else {
            panic!("unreadable");
        };
        assert_eq!((entries.len(), more), (3, 1), "the dot-file is not counted");
    }

    #[test]
    fn more_and_error_rows() {
        let d = temp("rows");
        let mut files = Files::new(d.join("missing"));
        let mut t = Tree::revealing(&mut files, None);
        let rows = t.rows(&mut files);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "#error:");
        assert_eq!(rows[0].kind, ItemKind::Error);

        let mut files = Files::new(d.clone());
        files.dirs.insert(
            String::new(),
            Listing::Read {
                entries: vec![Entry {
                    name: "a".into(),
                    dir: false,
                    utf8: true,
                    link_dir: false,
                }],
                more: 12,
            },
        );
        let mut t = Tree::default();
        let rows = t.rows(&mut files);
        assert_eq!(rows[1].id, "#more:");
        assert_eq!(
            (rows[1].kind, rows[1].name.as_str()),
            (ItemKind::More, "12")
        );
        assert!(!rows[1].openable);
    }

    #[test]
    fn a_vanished_selection_moves_to_its_nearest_shown_folder() {
        let d = temp("vanish");
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(d.join("a/b/x.txt"), "").unwrap();
        let mut files = Files::new(d.clone());
        let mut t = Tree::revealing(&mut files, Some("a/b/x.txt"));
        assert_eq!(t.selected.as_deref(), Some("a/b/x.txt"));
        std::fs::remove_dir_all(d.join("a/b")).unwrap();
        files.reread("a");
        t.rows(&mut files);
        assert_eq!(t.selected.as_deref(), Some("a"));
        assert_eq!(files.rel(&d.join("a/b")).as_deref(), Some("a/b"));
        assert_eq!(parent_of("a/b"), "a");
        assert_eq!(parent_of("a"), "");
    }

    #[test]
    fn left_on_an_error_row_selects_its_folder() {
        let d = temp("errrow");
        let mut files = Files::new(d.clone());
        files.dirs.insert(
            String::new(),
            Listing::Read {
                entries: vec![Entry {
                    name: "src".into(),
                    dir: true,
                    utf8: true,
                    link_dir: false,
                }],
                more: 0,
            },
        );
        files
            .dirs
            .insert("src".into(), Listing::Failed("denied".into()));
        let mut t = Tree::default();
        t.expanded.insert("src".into());
        t.rows(&mut files);
        assert!(t.select("#error:src"));
        t.collapse();
        assert_eq!(t.selected.as_deref(), Some("src"));
        assert_eq!(row_parent("#more:a/b"), "a/b");
        assert_eq!(row_parent("a/b"), "a");
    }
}
