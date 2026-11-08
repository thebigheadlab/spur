//! Editor state: documents, views, modes and registers, plus the preset-agnostic
//! `editor.*` and `buffer.*` commands that operate on them.

mod commands;
pub mod motion;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use spur_text::{Buffer, Point, SelRange, Selection, Snapshot, Version};

pub use commands::register_editor_commands;

/// Identifies a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
pub struct DocId(pub u64);

/// Identifies a view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
pub struct ViewId(pub u64);

/// An open document: a text buffer plus where it lives on disk.
pub struct Doc {
    /// The text.
    pub buffer: Buffer,
    /// File path, if any.
    pub path: Option<PathBuf>,
    /// Version last saved or loaded.
    pub saved: Version,
}

impl Doc {
    /// True if the text changed since it was loaded or saved.
    pub fn modified(&self) -> bool {
        self.buffer.version() != self.saved
    }
}

/// A view onto a document: selections, mode and scroll position.
#[derive(Clone, Debug)]
pub struct View {
    /// The document shown.
    pub doc: DocId,
    /// Selections; one range per cursor.
    pub selection: Selection,
    /// Goal char column per range, for vertical motion.
    pub goals: Vec<Option<usize>>,
    /// Current mode name.
    pub mode: Arc<str>,
    /// Undo turn for non-agent edits made in this view.
    pub turn: u64,
    /// First visible line.
    pub top_line: usize,
}

/// A mode, as data. Presets add their own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeSpec {
    /// Name, e.g. `normal`.
    pub name: Arc<str>,
    /// Edits made in this mode join one undo step until the mode is left or the cursor
    /// moves (insert-like modes). Otherwise each top-level edit is its own step.
    pub groups_edits: bool,
}

/// Text held in a register.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Register {
    /// One entry per cursor.
    pub values: Vec<String>,
    /// Whole lines.
    pub linewise: bool,
}

/// The session state used as `S` in [`crate::Core`].
pub struct Editor {
    /// Open documents.
    pub docs: BTreeMap<DocId, Doc>,
    /// Views.
    pub views: BTreeMap<ViewId, View>,
    /// The view commands act on.
    pub focus: ViewId,
    /// Known modes.
    pub modes: HashMap<Arc<str>, ModeSpec>,
    /// Registers; `"` is the unnamed one.
    pub registers: HashMap<char, Register>,
    /// Mode new views start in.
    pub default_mode: Arc<str>,
    next_id: u64,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    /// An editor with one empty scratch document, starting in `insert` mode (no preset).
    pub fn new() -> Self {
        let mut modes = HashMap::new();
        for (name, groups_edits) in [("normal", false), ("insert", true)] {
            modes.insert(
                Arc::from(name),
                ModeSpec {
                    name: name.into(),
                    groups_edits,
                },
            );
        }
        let mut ed = Self {
            docs: BTreeMap::new(),
            views: BTreeMap::new(),
            focus: ViewId(0),
            modes,
            registers: HashMap::new(),
            default_mode: "insert".into(),
            next_id: 1,
        };
        let doc = ed.add_doc(Buffer::new(""), None);
        ed.focus = ed.add_view(doc);
        ed
    }

    fn next(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Adds a document.
    pub fn add_doc(&mut self, buffer: Buffer, path: Option<PathBuf>) -> DocId {
        let id = DocId(self.next());
        let saved = buffer.version();
        self.docs.insert(
            id,
            Doc {
                buffer,
                path,
                saved,
            },
        );
        id
    }

    /// Adds a view of `doc` with a cursor at the start, in the default mode.
    pub fn add_view(&mut self, doc: DocId) -> ViewId {
        let id = ViewId(self.next());
        self.views.insert(
            id,
            View {
                doc,
                selection: Selection::cursor(0),
                goals: vec![None],
                mode: self.default_mode.clone(),
                turn: 0,
                top_line: 0,
            },
        );
        id
    }

    /// The focused view.
    pub fn view(&self) -> &View {
        &self.views[&self.focus]
    }

    /// The focused view's document.
    pub fn doc(&self) -> &Doc {
        &self.docs[&self.view().doc]
    }

    /// Snapshot of the focused document.
    pub fn snapshot(&self) -> Snapshot {
        self.doc().buffer.snapshot()
    }

    /// The focused view's mode.
    pub fn mode(&self) -> &ModeSpec {
        let v = self.view();
        self.modes
            .get(&v.mode)
            .unwrap_or_else(|| &self.modes[&self.default_mode])
    }

    /// Scrolls the focused view so the primary cursor is visible with `margin` lines.
    pub fn scroll_to_cursor(&mut self, rows: usize, margin: usize) {
        if rows == 0 {
            return;
        }
        let snap = self.snapshot();
        let view = self
            .views
            .get_mut(&self.focus)
            .unwrap_or_else(|| unreachable!());
        let head = view.selection.ranges[view.selection.primary].head;
        let line = motion::line_of(&snap, head);
        let margin = margin.min(rows.saturating_sub(1) / 2);
        if line < view.top_line + margin {
            view.top_line = line.saturating_sub(margin);
        } else if line + margin >= view.top_line + rows {
            view.top_line = line + margin + 1 - rows;
        }
        let max_top = snap.line_count().saturating_sub(1);
        view.top_line = view.top_line.min(max_top);
    }

    /// What a frontend needs to draw `rows` lines of the focused view. Cost is O(rows).
    pub fn viewport(&self, rows: usize) -> Viewport {
        let view = self.view();
        let doc = self.doc();
        let snap = doc.buffer.snapshot();
        let end = (view.top_line + rows).min(snap.line_count());
        let lines = (view.top_line..end)
            .map(|l| motion::line_text(&snap, l))
            .collect();
        let point = |off: usize| Point {
            line: motion::line_of(&snap, off),
            column: motion::char_col(&snap, off),
        };
        let mut cursors = Vec::new();
        let mut selections = Vec::new();
        for (i, r) in view.selection.ranges.iter().enumerate() {
            cursors.push((point(r.head), i == view.selection.primary));
            if r.anchor != r.head {
                let (a, b) = (r.anchor.min(r.head), r.anchor.max(r.head));
                selections.push((point(a), point(b)));
            }
        }
        Viewport {
            top_line: view.top_line,
            line_count: snap.line_count(),
            lines,
            cursors,
            selections,
            mode: view.mode.clone(),
            path: doc.path.clone(),
            modified: doc.modified(),
        }
    }

    /// The focused view's primary range.
    pub fn primary(&self) -> SelRange {
        let s = &self.view().selection;
        s.ranges[s.primary]
    }
}

/// A frontend's view of the visible lines. Columns are in chars; the frontend handles
/// tabs and wide chars.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Viewport {
    /// First line shown.
    pub top_line: usize,
    /// Lines in the document.
    pub line_count: usize,
    /// Visible lines, without line endings.
    pub lines: Vec<String>,
    /// Cursor positions (line, char column) and whether each is the primary one.
    pub cursors: Vec<(Point, bool)>,
    /// Non-empty selections, start..end.
    pub selections: Vec<(Point, Point)>,
    /// Mode name.
    pub mode: Arc<str>,
    /// File path.
    pub path: Option<PathBuf>,
    /// Unsaved changes.
    pub modified: bool,
}
