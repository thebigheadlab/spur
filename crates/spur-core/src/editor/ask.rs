//! Spur's own interaction pieces, shared by every frontend:
//!
//! - **Ask**: one card, opened at the caret, for everything that is not typing. Type a
//!   few letters of a command, a line number, or text to find. Agents join it later.
//! - **Find**: the Ask card in find form. Matches light up as you type; `enter` steps
//!   through them.
//! - **Grain**: grow a selection by meaning (word, line, block, whole file) and shrink it
//!   back, instead of learning motions.
//! - **Leave**: quitting with unsaved work asks on the card instead of failing.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use spur_text::{SelRange, Selection, Snapshot};

use super::commands::{C, after_selection_change, ev, focus, save, sync_context};
use super::{DocId, Editor, ModeSpec, ViewId, motion};
use crate::command::{CommandError, CommandMeta, parse_args};
use crate::core::{Core, RegisterError};
use crate::name::{CommandId, events};

/// Mode the keymap uses while the card is open.
pub const ASK_MODE: &str = "ask";

/// Most items the card lists.
const MAX_ITEMS: usize = 8;

/// What the card is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AskKind {
    /// Commands, line numbers, text to find.
    Ask,
    /// Find in the file.
    Find,
    /// A question with fixed answers (leaving with unsaved work).
    Confirm,
}

/// One row on the card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskItem {
    /// What it does, in words.
    pub title: String,
    /// Key that does the same without the card, if any.
    pub hint: String,
    /// Command run on `enter`.
    pub command: CommandId,
    /// Its arguments.
    pub args: Value,
}

/// The open card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    /// What it is for.
    pub kind: AskKind,
    /// Heading shown on the card's border.
    pub title: String,
    /// Text typed.
    pub text: String,
    /// Rows matching the text.
    pub items: Vec<AskItem>,
    /// Highlighted row.
    pub selected: usize,
    /// A short status (`3 of 12`, `no matches`).
    pub note: Option<String>,
    /// Mode to return to.
    pub return_mode: Arc<str>,
    /// Selection when the card opened (find starts here; `esc` on an empty find returns).
    pub origin: Selection,
    /// The text was filled in for you; typing replaces it.
    pub seeded: bool,
    /// The view it opened in.
    pub view: ViewId,
}

/// A command as the Ask card lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Command id.
    pub command: CommandId,
    /// Words to show and match.
    pub title: String,
    /// Key bound to it in the current keymap.
    pub hint: String,
    /// Arguments to run it with.
    pub args: Value,
}

/// A selection size, from small to large.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Grain {
    /// A word.
    Word,
    /// A line, without its ending.
    Line,
    /// A run of non-blank lines.
    Block,
    /// The whole file.
    All,
}

impl Grain {
    /// Name shown in the rail.
    pub fn name(self) -> &'static str {
        match self {
            Self::Word => "word",
            Self::Line => "line",
            Self::Block => "block",
            Self::All => "all",
        }
    }
}

/// Built-in titles for commands people look for by name. Others use their description.
const TITLES: &[(&str, &str)] = &[
    ("buffer.save", "Save"),
    ("editor.save_and_quit", "Save and leave"),
    ("editor.quit", "Leave Spur"),
    ("editor.undo", "Undo"),
    ("editor.redo", "Redo"),
    ("editor.copy", "Copy"),
    ("editor.delete_selection", "Cut"),
    ("editor.paste", "Paste"),
    ("editor.select_all", "Select everything"),
    (
        "editor.grow_selection",
        "Grow selection (word, line, block, all)",
    ),
    ("editor.shrink_selection", "Shrink selection"),
    ("editor.collapse_selection", "Drop selection"),
    ("editor.move_file_start", "Go to top"),
    ("editor.move_file_end", "Go to bottom"),
    ("ask.find", "Find in file"),
    ("editor.find_next", "Find next match"),
    ("editor.select_matches", "Select every match"),
    ("editor.goto_line", "Go to line"),
];

/// Commands shown before anything is typed, in order.
const FEATURED: &[&str] = &[
    "ask.find",
    "buffer.save",
    "editor.grow_selection",
    "editor.undo",
    "editor.redo",
    "editor.select_all",
    "editor.move_file_end",
    "editor.quit",
];

/// Commands shown first when text is selected.
const FEATURED_SELECTION: &[&str] = &[
    "editor.copy",
    "editor.delete_selection",
    "editor.grow_selection",
    "editor.shrink_selection",
    "editor.paste",
    "ask.find",
    "buffer.save",
    "editor.collapse_selection",
];

/// Commands never listed: card plumbing and raw building blocks that keys already
/// cover. Spur's own commands are listed when they have a title; extension commands
/// are listed by their description.
fn hidden(id: &str) -> bool {
    let core = matches!(
        id.split('.').next(),
        Some("editor" | "buffer" | "ask" | "view" | "core" | "bus" | "command")
    );
    core && !TITLES.iter().any(|(i, _)| *i == id)
}

/// The title for a command.
pub fn title_of(id: &str, description: &str) -> String {
    TITLES.iter().find(|(i, _)| *i == id).map_or_else(
        || description.trim_end_matches('.').to_owned(),
        |t| t.1.to_owned(),
    )
}

/// Builds the card's command list from the registered commands and the keys bound to
/// them. Frontends call this after loading keymaps.
pub fn index<'a>(
    metas: impl IntoIterator<Item = &'a CommandMeta>,
    hint: impl Fn(&str) -> Option<String>,
) -> Vec<Entry> {
    let mut out: Vec<Entry> = metas
        .into_iter()
        .filter(|m| !hidden(m.id.as_str()))
        .map(|m| Entry {
            command: m.id.clone(),
            title: title_of(m.id.as_str(), &m.description),
            hint: hint(m.id.as_str()).unwrap_or_default(),
            args: Value::Null,
        })
        .collect();
    out.sort_by(|a, b| a.title.cmp(&b.title));
    out
}

/// Fuzzy score of `query` against `text`: every query char in order, case-insensitive,
/// starting at a word start. Higher is better; word starts and runs score more.
fn score(query: &str, text: &str) -> Option<i32> {
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let word_start = |i: usize| i == 0 || !text[i - 1].is_alphanumeric();
    let mut s = 0;
    let mut at = 0;
    let mut prev: Option<usize> = None;
    for (n, q) in query
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .enumerate()
    {
        let i = if n == 0 {
            (at..text.len()).find(|&i| text[i] == q && word_start(i))?
        } else if prev.is_some_and(|p| text.get(p + 1) == Some(&q)) {
            prev.map_or(at, |p| p + 1)
        } else {
            // Prefer the next word start, else any later char.
            (at..text.len())
                .find(|&i| text[i] == q && word_start(i))
                .or_else(|| (at..text.len()).find(|&i| text[i] == q))?
        };
        s += if word_start(i) { 8 } else { 1 };
        if prev == Some(i.wrapping_sub(1)) {
            s += 4;
        }
        s -= i32::try_from(i - at).unwrap_or(i32::MAX).min(3);
        prev = Some(i);
        at = i + 1;
    }
    // Weak matches (scattered letters) are noise.
    let n = i32::try_from(query.chars().filter(|c| !c.is_whitespace()).count()).unwrap_or(0);
    (s >= n * 4).then_some(s)
}

fn item(e: &Entry) -> AskItem {
    AskItem {
        title: e.title.clone(),
        hint: e.hint.clone(),
        command: e.command.clone(),
        args: e.args.clone(),
    }
}

fn id(s: &str) -> CommandId {
    CommandId::new(s).unwrap_or_else(|e| unreachable!("{e}"))
}

/// Recomputes the card's rows for its text.
fn refresh(ed: &mut Editor) {
    let has_selection = ed
        .view()
        .selection
        .ranges
        .iter()
        .any(|r| r.anchor != r.head);
    let Some(ask) = ed.ask.as_mut() else {
        return;
    };
    if ask.kind != AskKind::Ask {
        return;
    }
    let text = ask.text.trim();
    let mut items = Vec::new();
    if text.is_empty() {
        let featured = if has_selection {
            FEATURED_SELECTION
        } else {
            FEATURED
        };
        for f in featured {
            if let Some(e) = ed.entries.iter().find(|e| e.command.as_str() == *f) {
                items.push(item(e));
            }
        }
    } else if let Ok(line) = text.parse::<usize>() {
        items.push(AskItem {
            title: format!("Go to line {line}"),
            hint: String::new(),
            command: id("editor.goto_line"),
            args: json!({ "line": line }),
        });
    } else {
        let mut scored: Vec<(i32, &Entry)> = ed
            .entries
            .iter()
            .filter_map(|e| {
                let s = score(text, &e.title)
                    .or_else(|| score(text, e.command.as_str()).map(|s| s - 20))?;
                Some((s, e))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));
        items.extend(scored.iter().take(MAX_ITEMS - 1).map(|(_, e)| item(e)));
        items.push(AskItem {
            title: format!("Find \u{201c}{text}\u{201d}"),
            hint: String::new(),
            command: id("ask.find"),
            args: json!({ "text": text }),
        });
    }
    items.truncate(MAX_ITEMS);
    ask.selected = ask.selected.min(items.len().saturating_sub(1));
    ask.items = items;
}

fn ensure_mode(ed: &mut Editor) {
    ed.modes
        .entry(Arc::from(ASK_MODE))
        .or_insert_with(|| ModeSpec {
            name: ASK_MODE.into(),
            groups_edits: false,
            block_cursor: false,
            inclusive_selection: false,
            linewise_selection: false,
        });
}

fn switch_mode(cx: C<'_, '_>, id: ViewId, mode: &Arc<str>) -> Result<(), CommandError> {
    let Some(view) = cx.state.views.get_mut(&id) else {
        return Ok(());
    };
    if view.mode == *mode {
        return Ok(());
    }
    view.bump_turn();
    view.mode = mode.clone();
    cx.emit(
        ev(events::MODE_CHANGED),
        json!({ "view": id.0, "mode": &**mode }),
    )
}

fn open(
    cx: C<'_, '_>,
    kind: AskKind,
    title: &str,
    text: String,
    items: Vec<AskItem>,
) -> Result<(), CommandError> {
    ensure_mode(cx.state);
    let return_mode = match &cx.state.ask {
        Some(a) => a.return_mode.clone(),
        None => cx.state.view().mode.clone(),
    };
    let origin = cx.state.view().selection.clone();
    let view = focus(cx);
    cx.state.ask = Some(Ask {
        view,
        kind,
        title: title.into(),
        text,
        items,
        selected: 0,
        note: None,
        return_mode,
        origin,
        seeded: false,
    });
    refresh(cx.state);
    switch_mode(cx, view, &ASK_MODE.into())
}

fn close(cx: C<'_, '_>) -> Result<Option<Ask>, CommandError> {
    let Some(a) = cx.state.ask.take() else {
        return Ok(None);
    };
    switch_mode(cx, a.view, &a.return_mode)?;
    Ok(Some(a))
}

fn set_selection(cx: C<'_, '_>, sel: Selection) -> Result<(), CommandError> {
    let id = focus(cx);
    let view = cx
        .state
        .views
        .get_mut(&id)
        .unwrap_or_else(|| unreachable!());
    view.goals = vec![None; sel.ranges.len()];
    view.selection = sel;
    after_selection_change(cx)
}

/// Saves every modified document that has a path, through a view that shows it.
fn save_all(cx: C<'_, '_>) -> Result<(), CommandError> {
    let focus_before = focus(cx);
    let docs: Vec<DocId> = cx
        .state
        .docs
        .iter()
        .filter(|(_, d)| d.modified() && d.path.is_some())
        .map(|(id, _)| *id)
        .collect();
    let mut result = Ok(());
    for doc in docs {
        let Some(view) = cx
            .state
            .views
            .iter()
            .find(|(_, v)| v.doc == doc)
            .map(|(id, _)| *id)
        else {
            continue;
        };
        cx.state.focus = view;
        if let Err(e) = save(cx, None) {
            result = Err(e);
            break;
        }
    }
    cx.state.focus = focus_before;
    result
}

/// Byte ranges of `needle` in `hay`, ignoring case unless the needle has capitals.
pub fn matches(hay: &str, needle: &str) -> Vec<std::ops::Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }
    let sensitive = needle.chars().any(char::is_uppercase);
    if sensitive {
        return hay
            .match_indices(needle)
            .map(|(i, m)| i..i + m.len())
            .collect();
    }
    // Lowercasing can change byte lengths; compare char by char instead.
    let n: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    let idx: Vec<(usize, char)> = hay.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < idx.len() {
        let mut k = 0;
        let mut j = i;
        while k < n.len() && j < idx.len() {
            let mut lc = idx[j].1.to_lowercase();
            if lc.len() != 1 || lc.next() != Some(n[k]) {
                break;
            }
            k += 1;
            j += 1;
        }
        if k == n.len() {
            let end = idx.get(j).map_or(hay.len(), |p| p.0);
            out.push(idx[i].0..end);
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// Selects the next (or previous) match of `text` from `from`, wrapping. Returns the
/// match number and the count.
fn find(
    cx: C<'_, '_>,
    text: &str,
    from: usize,
    backward: bool,
) -> Result<(usize, usize), CommandError> {
    let snap = cx.state.snapshot();
    let hay = snap.slice(0..snap.len()).unwrap_or_default();
    let all = matches(&hay, text);
    if all.is_empty() {
        return Ok((0, 0));
    }
    let i = if backward {
        all.iter()
            .rposition(|m| m.start < from)
            .unwrap_or(all.len() - 1)
    } else {
        all.iter().position(|m| m.start >= from).unwrap_or(0)
    };
    let m = all[i].clone();
    set_selection(
        cx,
        Selection {
            ranges: vec![SelRange {
                anchor: m.start,
                head: m.end,
            }],
            primary: 0,
        },
    )?;
    Ok((i + 1, all.len()))
}

fn find_note(found: (usize, usize)) -> String {
    match found {
        (_, 0) => "no matches".into(),
        (1, 1) => "1 match".into(),
        (i, n) => format!("{i} of {n}"),
    }
}

/// Live find while typing: search from where the card opened.
fn find_live(cx: C<'_, '_>) -> Result<(), CommandError> {
    let Some(a) = cx.state.ask.as_ref() else {
        return Ok(());
    };
    let text = a.text.clone();
    let origin = a.origin.clone();
    if text.is_empty() {
        set_selection(cx, origin)?;
        if let Some(a) = cx.state.ask.as_mut() {
            a.note = None;
        }
        return Ok(());
    }
    let from = origin.ranges[origin.primary]
        .anchor
        .min(origin.ranges[origin.primary].head);
    let found = find(cx, &text, from, false)?;
    if found.1 == 0 {
        set_selection(cx, origin)?;
    }
    if let Some(a) = cx.state.ask.as_mut() {
        a.note = Some(find_note(found));
    }
    Ok(())
}

/// Word chars for the word grain.
fn wordy(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The smallest grain range strictly larger than `r` (or containing a bare cursor).
#[allow(clippy::many_single_char_names)]
fn grow(s: &Snapshot, r: SelRange) -> (std::ops::Range<usize>, Grain) {
    let (a, b) = (r.anchor.min(r.head), r.anchor.max(r.head));
    let bigger = |x: &std::ops::Range<usize>| x.start <= a && b <= x.end && x.len() > b - a;
    // Word around the cursor.
    let line = motion::line_of(s, r.head);
    let ls = motion::line_start(s, line);
    let text = motion::line_text(s, line);
    let at = r.head.saturating_sub(ls).min(text.len());
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let pos = chars
        .iter()
        .position(|(i, _)| *i >= at)
        .unwrap_or(chars.len());
    let on = |i: usize| chars.get(i).is_some_and(|c| wordy(c.1));
    let pivot = if on(pos) {
        Some(pos)
    } else if pos > 0 && on(pos - 1) {
        Some(pos - 1)
    } else {
        None
    };
    if let Some(p) = pivot {
        let mut st = p;
        while st > 0 && on(st - 1) {
            st -= 1;
        }
        let mut en = p;
        while on(en) {
            en += 1;
        }
        let w = ls + chars[st].0..ls + chars.get(en).map_or(text.len(), |c| c.0);
        if bigger(&w) {
            return (w, Grain::Word);
        }
    }
    // Lines spanned.
    let (l0, l1) = (motion::line_of(s, a), motion::line_of(s, b));
    let lines = motion::line_start(s, l0)..motion::line_end(s, l1);
    if bigger(&lines) {
        return (lines, Grain::Line);
    }
    // Block: grow over neighbouring non-blank lines.
    let blank = |l: usize| motion::line_text(s, l).trim().is_empty();
    let last = s.line_count().saturating_sub(1);
    let (mut b0, mut b1) = (l0, l1);
    while b0 > 0 && !blank(b0 - 1) {
        b0 -= 1;
    }
    while b1 < last && !blank(b1 + 1) {
        b1 += 1;
    }
    let block = motion::line_start(s, b0)..motion::line_end(s, b1);
    // From a blank line there is no block of its own; go straight to the file.
    let on_blank = (l0..=l1).all(blank);
    if !on_blank && bigger(&block) {
        return (block, Grain::Block);
    }
    (0..s.len(), Grain::All)
}

#[derive(Deserialize, Default)]
struct OpenArgs {
    text: Option<String>,
}

#[derive(Deserialize)]
struct TypeArgs {
    text: String,
}

#[derive(Deserialize, Default)]
struct MoveArgs {
    #[serde(default)]
    by: i64,
}

#[derive(Deserialize)]
struct LineArgs {
    line: usize,
}

#[derive(Deserialize)]
struct FindArgs {
    text: String,
    #[serde(default)]
    backward: bool,
}

#[derive(Deserialize, Default)]
struct ExtendArgs {
    #[serde(default)]
    extend: bool,
}

fn to_edge(cx: C<'_, '_>, end: bool, extend: bool) -> Result<Value, CommandError> {
    let at = if end { cx.state.snapshot().len() } else { 0 };
    let p = cx.state.primary();
    let anchor = if extend { p.anchor } else { at };
    set_selection(
        cx,
        Selection {
            ranges: vec![SelRange { anchor, head: at }],
            primary: 0,
        },
    )?;
    Ok(Value::Null)
}

#[derive(Deserialize, Default)]
struct QuitArgs {
    #[serde(default)]
    force: bool,
}

/// Registers the Ask card, find, grain and leave commands.
///
/// # Errors
///
/// Returns [`RegisterError`] if any id is already registered.
#[allow(clippy::too_many_lines)]
pub(super) fn register(core: &mut Core<Editor>) -> Result<(), RegisterError> {
    let mut reg = |id: &str,
                   desc: &str,
                   f: fn(C<'_, '_>, &Value) -> Result<Value, CommandError>|
     -> Result<(), RegisterError> {
        let meta = CommandMeta::new(
            CommandId::new(id).unwrap_or_else(|e| unreachable!("{e}")),
            desc,
        );
        core.register_fn(meta, move |cx, args| {
            let r = f(cx, args);
            sync_context(cx);
            r
        })
        .map(|_| ())
    };

    reg(
        "ask.open",
        "Open the Ask card at the caret: commands, line numbers, text to find.",
        |cx, a| {
            let args: OpenArgs = parse_args(a)?;
            open(
                cx,
                AskKind::Ask,
                "ask",
                args.text.unwrap_or_default(),
                Vec::new(),
            )?;
            Ok(Value::Null)
        },
    )?;
    reg(
        "ask.find",
        "Find text in the file; matches light up as you type.",
        |cx, a| {
            let args: OpenArgs = parse_args(a)?;
            let seeded = args.text.is_none();
            let text = args.text.unwrap_or_else(|| {
                // Seed with a one-line selection, else the last search.
                let snap = cx.state.snapshot();
                let r = cx.state.primary();
                let sel = snap
                    .slice(r.anchor.min(r.head)..r.anchor.max(r.head))
                    .unwrap_or_default();
                if !sel.is_empty() && !sel.contains('\n') {
                    sel
                } else {
                    cx.state.find_text.clone().unwrap_or_default()
                }
            });
            // From the Ask card, find from where it was opened.
            let origin = cx.state.ask.as_ref().map(|a| a.origin.clone());
            open(cx, AskKind::Find, "find", text, Vec::new())?;
            if let Some(a) = cx.state.ask.as_mut() {
                if let Some(o) = origin {
                    a.origin = o;
                }
                a.seeded = seeded && !a.text.is_empty();
            }
            find_live(cx)?;
            Ok(Value::Null)
        },
    )?;
    reg("ask.type", "Type into the Ask card.", |cx, a| {
        let args: TypeArgs = parse_args(a)?;
        let Some(ask) = cx.state.ask.as_mut() else {
            return Ok(Value::Null);
        };
        if ask.kind == AskKind::Confirm {
            return Ok(Value::Null);
        }
        if ask.seeded {
            ask.text.clear();
            ask.seeded = false;
        }
        ask.text.push_str(&args.text);
        ask.selected = 0;
        let kind = ask.kind;
        refresh(cx.state);
        if kind == AskKind::Find {
            find_live(cx)?;
        }
        Ok(Value::Null)
    })?;
    reg(
        "ask.erase",
        "Delete the last char typed into the Ask card.",
        |cx, _| {
            let Some(ask) = cx.state.ask.as_mut() else {
                return Ok(Value::Null);
            };
            if ask.seeded {
                ask.text.clear();
                ask.seeded = false;
            } else {
                ask.text.pop();
            }
            ask.selected = 0;
            let kind = ask.kind;
            refresh(cx.state);
            if kind == AskKind::Find {
                find_live(cx)?;
            }
            Ok(Value::Null)
        },
    )?;
    reg(
        "ask.move",
        "Move the highlight on the Ask card (by = -1 up, 1 down).",
        |cx, a| {
            let args: MoveArgs = parse_args(a)?;
            if let Some(ask) = cx.state.ask.as_mut() {
                let n = i64::try_from(ask.items.len()).unwrap_or(0);
                if n > 0 {
                    let cur = i64::try_from(ask.selected).unwrap_or(0);
                    ask.selected = usize::try_from((cur + args.by).rem_euclid(n)).unwrap_or(0);
                }
            }
            Ok(Value::Null)
        },
    )?;
    reg(
        "ask.close",
        "Close the Ask card. Find keeps the match selected.",
        |cx, _| {
            if let Some(a) = close(cx)?
                && a.kind == AskKind::Find
            {
                if a.text.is_empty() || a.note.as_deref() == Some("no matches") {
                    set_selection(cx, a.origin)?;
                } else {
                    cx.state.find_text = Some(a.text);
                }
            }
            Ok(Value::Null)
        },
    )?;
    reg(
        "ask.accept",
        "Run the highlighted row of the Ask card (find: go to the next match).",
        |cx, a| {
            #[derive(Deserialize, Default)]
            struct Dir {
                #[serde(default)]
                backward: bool,
            }
            let args: Dir = parse_args(a)?;
            let Some(ask) = cx.state.ask.clone() else {
                return Ok(Value::Null);
            };
            if ask.kind == AskKind::Find {
                if ask.text.is_empty() {
                    return Ok(Value::Null);
                }
                let r = cx.state.primary();
                let from = if args.backward {
                    r.anchor.min(r.head)
                } else {
                    r.anchor.max(r.head).max(r.anchor.min(r.head) + 1)
                };
                let found = find(cx, &ask.text, from, args.backward)?;
                cx.state.find_text = Some(ask.text);
                if let Some(a) = cx.state.ask.as_mut() {
                    a.note = Some(find_note(found));
                }
                return Ok(Value::Null);
            }
            close(cx)?;
            let Some(it) = ask.items.get(ask.selected).cloned() else {
                return Ok(Value::Null);
            };
            cx.execute(&it.command, &it.args)
        },
    )?;
    reg(
        "editor.select_matches",
        "Select every match of the text on the Find card (or the last find), so typing changes them all.",
        |cx, _| {
            let typed = cx
                .state
                .ask
                .as_ref()
                .filter(|a| !a.text.is_empty())
                .map(|a| a.text.clone());
            let Some(text) = typed.or_else(|| cx.state.find_text.clone()) else {
                return Err(CommandError::failed("nothing to find yet"));
            };
            let snap = cx.state.snapshot();
            let hay = snap.slice(0..snap.len()).unwrap_or_default();
            let all = matches(&hay, &text);
            if all.is_empty() {
                return Err(CommandError::failed("no matches"));
            }
            // Keep the match the caret is on as the primary one.
            let r = cx.state.primary();
            let at = r.anchor.min(r.head);
            let primary = all.iter().position(|m| m.start >= at).unwrap_or(0);
            if cx.state.ask.is_some() {
                close(cx)?;
            }
            cx.state.find_text = Some(text);
            let n = all.len();
            set_selection(
                cx,
                Selection {
                    ranges: all
                        .into_iter()
                        .map(|m| SelRange {
                            anchor: m.start,
                            head: m.end,
                        })
                        .collect(),
                    primary,
                },
            )?;
            cx.state.message = Some(super::Message {
                text: format!("{n} selected: type to change them all"),
                error: false,
            });
            Ok(json!({ "matches": n }))
        },
    )?;
    reg(
        "editor.find",
        "Select the next match of text after the cursor (backward = before), wrapping.",
        |cx, a| {
            let args: FindArgs = parse_args(a)?;
            let r = cx.state.primary();
            let from = if args.backward {
                r.anchor.min(r.head)
            } else {
                r.anchor.max(r.head)
            };
            let (i, n) = find(cx, &args.text, from, args.backward)?;
            cx.state.find_text = Some(args.text);
            if n == 0 {
                return Err(CommandError::failed("no matches"));
            }
            Ok(json!({ "match": i, "matches": n }))
        },
    )?;
    reg(
        "editor.find_next",
        "Go to the next match of the last find (backward = previous).",
        |cx, a| {
            #[derive(Deserialize, Default)]
            struct Dir {
                #[serde(default)]
                backward: bool,
            }
            let d: Dir = parse_args(a)?;
            let Some(text) = cx.state.find_text.clone() else {
                return Err(CommandError::failed("nothing to find yet"));
            };
            let r = cx.state.primary();
            let from = if d.backward {
                r.anchor.min(r.head)
            } else {
                r.anchor.max(r.head)
            };
            let found = find(cx, &text, from, d.backward)?;
            if found.1 == 0 {
                return Err(CommandError::failed("no matches"));
            }
            cx.state.message = Some(super::Message {
                text: find_note(found),
                error: false,
            });
            Ok(Value::Null)
        },
    )?;
    reg(
        "editor.goto_line",
        "Put the cursor at the start of a line (1 = first).",
        |cx, a| {
            let args: LineArgs = parse_args(a)?;
            let snap = cx.state.snapshot();
            let line = args.line.max(1).min(snap.line_count()) - 1;
            let at = motion::first_nonblank(&snap, line);
            set_selection(cx, Selection::cursor(at))?;
            Ok(Value::Null)
        },
    )?;
    reg(
        "editor.move_file_start",
        "Put the cursor at the very start of the file (extend = select to there).",
        |cx, a| {
            let args: ExtendArgs = parse_args(a)?;
            to_edge(cx, false, args.extend)
        },
    )?;
    reg(
        "editor.move_file_end",
        "Put the cursor at the very end of the file (extend = select to there).",
        |cx, a| {
            let args: ExtendArgs = parse_args(a)?;
            to_edge(cx, true, args.extend)
        },
    )?;
    reg("editor.select_all", "Select the whole file.", |cx, _| {
        let len = cx.state.snapshot().len();
        set_selection(
            cx,
            Selection {
                ranges: vec![SelRange {
                    anchor: 0,
                    head: len,
                }],
                primary: 0,
            },
        )?;
        Ok(Value::Null)
    })?;
    reg(
        "editor.grow_selection",
        "Grow every selection to the next size: word, line, block, whole file.",
        |cx, _| {
            let snap = cx.state.snapshot();
            let view = cx.state.view();
            let before = view.selection.clone();
            let mut stack = view.grain.clone();
            // Any other selection change since the last grow starts over.
            if stack.last().is_none_or(|(after, _, _)| *after != before) {
                stack.clear();
            }
            let mut biggest = Grain::Word;
            let ranges = before
                .ranges
                .iter()
                .map(|r| {
                    let (g, grain) = grow(&snap, *r);
                    biggest = biggest.max(grain);
                    SelRange {
                        anchor: g.start,
                        head: g.end,
                    }
                })
                .collect();
            let mut after = Selection {
                ranges,
                primary: before.primary,
            };
            let mut goals = vec![None; after.ranges.len()];
            super::commands::merge_cursors(&mut after, &mut goals);
            stack.push((after.clone(), before, biggest));
            set_selection(cx, after)?;
            let id = focus(cx);
            if let Some(v) = cx.state.views.get_mut(&id) {
                v.grain = stack;
            }
            Ok(json!({ "grain": biggest.name() }))
        },
    )?;
    reg(
        "editor.shrink_selection",
        "Undo the last grow of the selection.",
        |cx, _| {
            let view = cx.state.view();
            let mut stack = view.grain.clone();
            if stack
                .last()
                .is_none_or(|(after, _, _)| *after != view.selection)
            {
                return Ok(Value::Null);
            }
            let Some((_, before, _)) = stack.pop() else {
                return Ok(Value::Null);
            };
            set_selection(cx, before)?;
            let id = focus(cx);
            if let Some(v) = cx.state.views.get_mut(&id) {
                v.grain = stack;
            }
            Ok(Value::Null)
        },
    )?;
    reg(
        "editor.quit",
        "Leave Spur. With unsaved work, asks first (force = leave anyway).",
        |cx, a| {
            let args: QuitArgs = parse_args(a)?;
            let unsaved: Vec<String> = cx
                .state
                .docs
                .values()
                .filter(|d| d.modified())
                .map(|d| {
                    d.path.as_ref().map_or_else(
                        || "the new file".to_owned(),
                        |p| {
                            p.file_name().map_or_else(
                                || p.display().to_string(),
                                |n| n.to_string_lossy().into(),
                            )
                        },
                    )
                })
                .collect();
            if args.force || unsaved.is_empty() {
                cx.state.quit = true;
                return Ok(Value::Null);
            }
            let all_have_paths = cx
                .state
                .docs
                .values()
                .filter(|d| d.modified())
                .all(|d| d.path.is_some());
            let mut items = Vec::new();
            if all_have_paths {
                items.push(AskItem {
                    title: "Save and leave".into(),
                    hint: String::new(),
                    command: id("editor.save_and_quit"),
                    args: Value::Null,
                });
            }
            items.push(AskItem {
                title: "Leave without saving".into(),
                hint: String::new(),
                command: id("editor.quit"),
                args: json!({ "force": true }),
            });
            items.push(AskItem {
                title: "Keep editing".into(),
                hint: "esc".into(),
                command: id("ask.close"),
                args: Value::Null,
            });
            let title = format!("{} has unsaved changes", unsaved.join(", "));
            open(cx, AskKind::Confirm, &title, String::new(), items)?;
            Ok(Value::Null)
        },
    )?;
    reg("editor.set_message", "Show a short message.", |cx, a| {
        let args: OpenArgs = parse_args(a)?;
        cx.state.message = args.text.map(|text| super::Message { text, error: false });
        Ok(Value::Null)
    })?;
    reg(
        "editor.save_and_quit",
        "Save every changed file, then leave Spur.",
        |cx, _| {
            save_all(cx)?;
            cx.state.quit = true;
            Ok(Value::Null)
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy() {
        assert!(score("sv", "Save").is_some());
        assert!(score("xz", "Save").is_none());
        assert!(score("sa", "Save").unwrap() > score("sa", "Select all").unwrap_or(-99) - 100);
        assert!(
            score("gro", "Grow selection").unwrap() > score("gro", "Go to bottom").unwrap_or(-99)
        );
    }

    #[test]
    fn matching() {
        assert_eq!(matches("Greet greet GREET", "greet").len(), 3);
        assert_eq!(matches("Greet greet GREET", "Greet"), vec![0..5]);
        assert_eq!(matches("é é", "é"), vec![0..2, 3..5]);
        assert!(matches("abc", "").is_empty());
    }
}
