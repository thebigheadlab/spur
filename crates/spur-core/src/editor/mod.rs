//! Editor state: documents, views, modes and clips, plus the preset-agnostic
//! `editor.*`, `buffer.*` and `ask.*` commands that operate on them.

pub mod ask;
mod commands;
pub mod motion;

use std::any::{Any, TypeId};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use spur_text::{Buffer, Point, SelRange, Selection, Snapshot, Version};

pub use ask::{Ask, AskItem, AskKind, Entry, Grain};
pub use commands::register_editor_commands;

/// Building blocks for preset extensions that add their own editing commands.
pub mod kit {
    pub use super::commands::{
        C, Kind, after_selection_change, apply_hunks, count, ev, focus, line_ending, move_ranges,
        open, save, span, sync_context,
    };
}

/// A recorded command call, replayed by presets that repeat changes or play macros.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    /// Command.
    pub command: crate::CommandId,
    /// Args.
    pub args: Arc<serde_json::Value>,
    /// Count.
    pub count: Option<u32>,
    /// Register.
    pub register: Option<char>,
}

/// Lets a preset take over a motion: called before every motion command moves the
/// selections, with the motion's kind and target function. Returning `Some` replaces
/// the move (a preset applying an operator to the motion's range, for example).
pub type MotionHook = fn(
    &mut crate::Cx<'_, Editor>,
    commands::Kind,
    &dyn Fn(&Snapshot, usize, usize) -> usize,
) -> Option<Result<serde_json::Value, crate::CommandError>>;

/// State that extensions keep on the editor, one value per type.
#[derive(Default)]
pub struct Extensions(HashMap<TypeId, Box<dyn Any + Send + Sync>>);

impl Extensions {
    /// The value of type `T`, if set.
    pub fn get<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.0.get(&TypeId::of::<T>())?.downcast_ref()
    }

    /// The value of type `T`, created with `Default` if missing.
    pub fn get_mut<T: Any + Send + Sync + Default>(&mut self) -> &mut T {
        self.0
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(T::default()))
            .downcast_mut()
            .unwrap_or_else(|| unreachable!("keyed by type"))
    }
}

/// The one mode Spur needs: typing edits text.
pub const EDIT_MODE: &str = "edit";

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
    /// While > 0, edits do not start new undo steps (replays by presets).
    pub turn_lock: u32,
    /// Grow history: (selection after, selection before, grain), newest last.
    pub grain: Vec<(Selection, Selection, Grain)>,
}

impl View {
    /// Starts a new undo step unless replay holds the current one.
    pub fn bump_turn(&mut self) {
        if self.turn_lock == 0 {
            self.turn += 1;
        }
    }
}

/// A mode, as data. Presets add their own.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ModeSpec {
    /// Name, e.g. `normal`.
    pub name: Arc<str>,
    /// Edits made in this mode join one undo step until the mode is left or the cursor
    /// moves (insert-like modes). Otherwise each top-level edit is its own step.
    pub groups_edits: bool,
    /// The cursor sits on a char and cannot rest after the last one.
    pub block_cursor: bool,
    /// Selections include the char under the cursor.
    pub inclusive_selection: bool,
    /// Selections cover whole lines.
    pub linewise_selection: bool,
}

/// Copied text.
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
    /// Copied text by name; `"` is the clipboard copy and paste use by default.
    pub registers: HashMap<char, Register>,
    /// Mode new views start in.
    pub default_mode: Arc<str>,
    /// A one-line prompt a preset is showing (its own command line), with its prefix.
    pub line_prompt: Option<String>,
    /// A preset's motion hook, see [`MotionHook`].
    pub motion_hook: Option<MotionHook>,
    /// Extension state.
    pub ext: Extensions,
    /// A message to show briefly.
    pub message: Option<Message>,
    /// Set by `editor.quit`; the frontend exits.
    pub quit: bool,
    /// The Ask card, while open.
    pub ask: Option<Ask>,
    /// Commands the Ask card can list, filled by the frontend from the registry.
    pub entries: Vec<Entry>,
    /// Text of the last find.
    pub find_text: Option<String>,
    next_id: u64,
}

/// A short message for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// Text.
    pub text: String,
    /// An error.
    pub error: bool,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    /// An editor with one empty document, in the single `edit` mode. Typing types; there
    /// is nothing else to learn. Presets may add modes of their own.
    pub fn new() -> Self {
        let mut modes = HashMap::new();
        modes.insert(
            Arc::from(EDIT_MODE),
            ModeSpec {
                name: EDIT_MODE.into(),
                groups_edits: true,
                block_cursor: false,
                inclusive_selection: false,
                linewise_selection: false,
            },
        );
        let mut ed = Self {
            docs: BTreeMap::new(),
            views: BTreeMap::new(),
            focus: ViewId(0),
            modes,
            registers: HashMap::new(),
            default_mode: EDIT_MODE.into(),
            line_prompt: None,
            motion_hook: None,
            ext: Extensions::default(),
            message: None,
            quit: false,
            ask: None,
            entries: Vec::new(),
            find_text: None,
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
                turn_lock: 0,
                grain: Vec::new(),
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
            .or_else(|| self.modes.get(&self.default_mode))
            .or_else(|| self.modes.get(EDIT_MODE))
            .unwrap_or_else(|| unreachable!("the edit mode is never removed"))
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
        let inclusive = self.mode().inclusive_selection;
        let linewise = self.mode().linewise_selection;
        let mut cursors = Vec::new();
        let mut selections = Vec::new();
        for (i, r) in view.selection.ranges.iter().enumerate() {
            cursors.push((point(r.head), i == view.selection.primary));
            let (mut a, mut b) = (r.anchor.min(r.head), r.anchor.max(r.head));
            if linewise {
                a = motion::line_start(&snap, motion::line_of(&snap, a));
                b = motion::line_end(&snap, motion::line_of(&snap, b));
            } else if inclusive {
                b = snap.next_grapheme_boundary(b).unwrap_or(b);
            }
            if a != b {
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
            line_prompt: self.line_prompt.clone(),
            message: self.message.clone(),
            ask: self.ask.clone(),
            grain: view
                .grain
                .last()
                .filter(|(after, _, _)| *after == view.selection)
                .map(|g| g.2),
            highlights: self.highlights(&snap, view.top_line, end),
        }
    }

    /// Other visible matches of the open find, for highlighting.
    fn highlights(&self, snap: &Snapshot, top: usize, end: usize) -> Vec<(Point, Point)> {
        let Some(a) = self.ask.as_ref().filter(|a| a.kind == AskKind::Find) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for line in top..end {
            let start = motion::line_start(snap, line);
            let text = motion::line_text(snap, line);
            for m in ask::matches(&text, &a.text) {
                let col = |b: usize| text[..b].chars().count();
                out.push((
                    Point {
                        line,
                        column: col(m.start),
                    },
                    Point {
                        line,
                        column: col(m.end),
                    },
                ));
            }
            let _ = start;
        }
        out
    }

    /// Moves cursors off line endings in block-cursor modes.
    pub fn clamp_cursors(&mut self) {
        if !self.mode().block_cursor {
            return;
        }
        let snap = self.snapshot();
        let view = self
            .views
            .get_mut(&self.focus)
            .unwrap_or_else(|| unreachable!());
        for r in &mut view.selection.ranges {
            let line = motion::line_of(&snap, r.head);
            let end = motion::line_end(&snap, line);
            if r.head >= end && end > motion::line_start(&snap, line) {
                let collapsed = r.anchor == r.head;
                r.head = motion::left(&snap, end, false);
                if collapsed {
                    r.anchor = r.head;
                }
            }
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
    /// A preset's one-line prompt, while open.
    pub line_prompt: Option<String>,
    /// Message to show briefly.
    pub message: Option<Message>,
    /// The Ask card, while open.
    pub ask: Option<Ask>,
    /// Size of the selection after a grow, while it stands.
    pub grain: Option<Grain>,
    /// Find matches on the visible lines.
    pub highlights: Vec<(Point, Point)>,
}
