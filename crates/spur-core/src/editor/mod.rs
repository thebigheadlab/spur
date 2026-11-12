//! Editor state: documents, views, modes and registers, plus the preset-agnostic
//! `editor.*` and `buffer.*` commands that operate on them.

mod commands;
pub mod motion;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use spur_text::{Buffer, Point, SelRange, Selection, Snapshot, Version};

use crate::{ClientId, CtxValue};

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
    /// Text as last saved or loaded, so undoing back to it clears `modified`.
    pub saved_text: Snapshot,
}

impl Doc {
    /// True if the text changed since it was loaded or saved.
    pub fn modified(&self) -> bool {
        self.buffer.version() != self.saved
    }

    /// Marks the doc saved again if its text equals the saved text (after an undo, say).
    /// Cheap unless the lengths match.
    pub fn recheck_saved(&mut self) {
        if !self.modified() || self.buffer.len() != self.saved_text.len() {
            return;
        }
        let now = self.buffer.snapshot();
        let same = now
            .chunks(0..now.len())
            .into_iter()
            .flatten()
            .flat_map(str::bytes)
            .eq(self
                .saved_text
                .chunks(0..self.saved_text.len())
                .into_iter()
                .flatten()
                .flat_map(str::bytes));
        if same {
            self.saved = self.buffer.version();
        }
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
    /// Undo turn for non-agent edits made in this view. Each view numbers its turns
    /// from `id << 32`, so two views never share a turn and their edits never join
    /// one undo step; [`turn_view`] recovers the view from a turn.
    pub turn: u64,
    /// First visible line.
    pub top_line: usize,
    /// Client that owns this view, if any. `editor.open` reuses only views owned by the
    /// calling client, so clients never share a view.
    pub owner: Option<ClientId>,
}

/// The view whose turn numbering `turn` comes from (see [`View::turn`]).
pub fn turn_view(turn: u64) -> ViewId {
    ViewId(turn >> 32)
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
        let saved_text = buffer.snapshot();
        self.docs.insert(
            id,
            Doc {
                buffer,
                path,
                saved,
                saved_text,
            },
        );
        id
    }

    /// Adds a view of `doc` with a cursor at the start, in the default mode.
    pub fn add_view(&mut self, doc: DocId) -> ViewId {
        self.add_view_for(doc, None)
    }

    /// Adds a view of `doc` owned by `owner`.
    pub fn add_view_for(&mut self, doc: DocId, owner: Option<ClientId>) -> ViewId {
        let id = ViewId(self.next());
        self.views.insert(
            id,
            View {
                doc,
                selection: Selection::cursor(0),
                goals: vec![None],
                mode: self.default_mode.clone(),
                turn: id.0 << 32,
                top_line: 0,
                owner,
            },
        );
        id
    }

    /// Removes a view. Refuses (returns `false`) for the focused view or an unknown id.
    pub fn remove_view(&mut self, view: ViewId) -> bool {
        view != self.focus && self.views.remove(&view).is_some()
    }

    /// Context keys that describe `view`: its mode and its document's state. The core
    /// publishes them for the focused view; a server publishes them per client.
    pub fn view_keys(&self, view: ViewId) -> Option<[(&'static str, CtxValue); 3]> {
        let v = self.views.get(&view)?;
        let doc = self.docs.get(&v.doc)?;
        Some([
            ("editor.mode", CtxValue::Str(v.mode.clone())),
            ("buffer.modified", CtxValue::Bool(doc.modified())),
            ("buffer.has_path", CtxValue::Bool(doc.path.is_some())),
        ])
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
        self.scroll_view(self.focus, rows, margin);
    }

    /// Scrolls `view` so its primary cursor is visible with `margin` lines.
    pub fn scroll_view(&mut self, view: ViewId, rows: usize, margin: usize) {
        if rows == 0 {
            return;
        }
        let Some(doc) = self.views.get(&view).map(|v| v.doc) else {
            return;
        };
        let snap = self.docs[&doc].buffer.snapshot();
        let view = self.views.get_mut(&view).unwrap_or_else(|| unreachable!());
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

    /// Ends `view`'s current undo step, so its next edit starts a new one.
    pub fn end_undo_step(&mut self, view: ViewId) {
        if let Some(v) = self.views.get_mut(&view) {
            v.turn += 1;
        }
    }

    /// Moves `view`'s first visible line by `lines`, staying within the document.
    pub fn scroll_by(&mut self, view: ViewId, lines: isize) {
        let Some(v) = self.views.get_mut(&view) else {
            return;
        };
        let count = self.docs[&v.doc].buffer.snapshot().line_count();
        v.top_line = v
            .top_line
            .saturating_add_signed(lines)
            .min(count.saturating_sub(1));
    }

    /// Line and char column of `view`'s primary cursor.
    pub fn primary_point(&self, view: ViewId) -> Option<Point> {
        let v = self.views.get(&view)?;
        let snap = self.docs.get(&v.doc)?.buffer.snapshot();
        let head = v.selection.ranges[v.selection.primary].head;
        Some(Point {
            line: motion::line_of(&snap, head),
            column: motion::char_col(&snap, head),
        })
    }

    /// What a frontend needs to draw `rows` lines of the focused view. Cost is O(rows).
    pub fn viewport(&self, rows: usize) -> Viewport {
        self.viewport_of(self.focus, rows, usize::MAX)
            .unwrap_or_else(|| unreachable!("the focused view exists"))
    }

    /// What a frontend needs to draw `rows` lines of `view`, each cut to `max_cols`
    /// chars, or `None` for an unknown view. Cost is O(rows × `max_cols`), never the
    /// length of a long line.
    pub fn viewport_of(&self, view: ViewId, rows: usize, max_cols: usize) -> Option<Viewport> {
        let view = self.views.get(&view)?;
        let doc = self.docs.get(&view.doc)?;
        let snap = doc.buffer.snapshot();
        let end = (view.top_line + rows).min(snap.line_count());
        let (lines, clipped) = visible_lines(&snap, view.top_line..end, max_cols);
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
        Some(Viewport {
            top_line: view.top_line,
            line_count: snap.line_count(),
            lines,
            clipped,
            cursors,
            selections,
            mode: view.mode.clone(),
            path: doc.path.clone(),
            modified: doc.modified(),
        })
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
    /// Per visible line: it was cut to the requested width.
    pub clipped: Vec<bool>,
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

/// Lines `range` without their endings, each cut to `max_cols` chars. Copies the
/// block once and splits it, unless a line is long enough that copying it whole
/// would cost more than the cut lines.
fn visible_lines(
    s: &Snapshot,
    range: std::ops::Range<usize>,
    max_cols: usize,
) -> (Vec<String>, Vec<bool>) {
    let (Some(first), Some(last)) = (
        s.line_range(range.start),
        range.end.checked_sub(1).and_then(|l| s.line_range(l)),
    ) else {
        return (Vec::new(), Vec::new());
    };
    let rows = range.end - range.start;
    let limit = rows.saturating_mul(max_cols.min(1 << 20)).saturating_mul(4);
    let block = (last.end - first.start <= limit)
        .then(|| s.slice(first.start..last.end))
        .flatten();
    let Some(block) = block else {
        return range.map(|l| clipped_line(s, l, max_cols)).unzip();
    };
    let mut lines = Vec::with_capacity(rows);
    let mut clipped = Vec::with_capacity(rows);
    let mut rest = block.as_str();
    for _ in 0..rows {
        // Line breaks are `\n`, `\r\n` and `\r` (the rope's line metric).
        let brk = memchr::memchr2(b'\n', b'\r', rest.as_bytes());
        let (line, next) = match brk {
            Some(i) if rest[i..].starts_with("\r\n") => (&rest[..i], &rest[i + 2..]),
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        rest = next;
        let cut = if line.len() > max_cols {
            line.char_indices().nth(max_cols).map(|(i, _)| i)
        } else {
            None
        };
        lines.push(cut.map_or(line, |i| &line[..i]).to_owned());
        clipped.push(cut.is_some());
    }
    (lines, clipped)
}

/// Line `line` without its ending, cut to `max_cols` chars, and whether it was cut.
fn clipped_line(s: &Snapshot, line: usize, max_cols: usize) -> (String, bool) {
    // One range lookup and borrowed chunks: no per-line char conversions, and never
    // more than `max_cols` chars read from a long line.
    let Some(range) = s.line_range(line) else {
        return (String::new(), false);
    };
    let cap = (range.end - range.start).min(max_cols.saturating_mul(4));
    take_cols(s.chunks(range).into_iter().flatten(), max_cols, cap)
}

/// The text of `chunks` (one line with its ending) up to `max_cols` chars, without
/// the ending, and whether chars were left out.
fn take_cols<'a>(
    chunks: impl Iterator<Item = &'a str>,
    max_cols: usize,
    cap: usize,
) -> (String, bool) {
    let mut text = String::with_capacity(cap);
    let mut chars = 0usize;
    'chunks: for chunk in chunks {
        // A char is at least one byte, so a chunk this short cannot pass the limit.
        if chars + chunk.len() <= max_cols {
            text.push_str(chunk);
            chars += chunk.chars().count();
            continue;
        }
        for c in chunk.chars() {
            if chars == max_cols {
                // Only the line ending may follow a full line.
                if matches!(c, '\n' | '\r') {
                    break 'chunks;
                }
                return (text, true);
            }
            text.push(c);
            chars += 1;
        }
    }
    // Drop the line ending: `\r\n`, `\n` or `\r`.
    if text.ends_with('\n') {
        text.pop();
    }
    if text.ends_with('\r') {
        text.pop();
    }
    (text, false)
}

#[cfg(test)]
mod tests {
    use super::take_cols;

    #[test]
    fn a_cut_is_seen_across_chunk_boundaries() {
        let cut = |chunks: &[&str], max| take_cols(chunks.iter().copied(), max, 0);
        // The limit falls exactly at the end of a chunk.
        assert_eq!(cut(&["éé", "é\n"], 2), ("éé".into(), true));
        assert_eq!(cut(&["éé", "\r\n"], 2), ("éé".into(), false));
        assert_eq!(cut(&["éé", "\n"], 2), ("éé".into(), false));
        assert_eq!(cut(&["éé"], 2), ("éé".into(), false));
        assert_eq!(cut(&["ab", "c"], 3), ("abc".into(), false));
        assert_eq!(cut(&["ab", "cd"], 3), ("abc".into(), true));
        assert_eq!(cut(&["ab\r", "\n"], 2), ("ab".into(), false));
    }
}
