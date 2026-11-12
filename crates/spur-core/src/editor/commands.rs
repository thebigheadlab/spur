//! Preset-agnostic editing commands. Every command acts on the focused view and honours
//! the invocation count.

use std::io::Write as _;
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};
use spur_text::{
    Applied, Bias, Buffer, Change, ChangeSet, EditError, SelRange, Selection, Snapshot, Transaction,
};

use super::{Editor, Register, ViewId, motion, turn_view};
use crate::command::{CommandError, CommandMeta, ErrorCode, RemoteSide, parse_args};
use crate::core::{Core, Cx, RegisterError};
use crate::name::{CommandId, EventName, events};

type C<'a, 'b> = &'a mut Cx<'b, Editor>;

fn ev(name: &str) -> EventName {
    EventName::new(name).unwrap_or_else(|e| unreachable!("{e}"))
}

/// Largest count a command honours; bigger ones are clamped so `999999999x` cannot hang.
const MAX_COUNT: usize = 100_000;
/// Largest text one command may insert.
const MAX_INSERT: usize = 64 << 20;
/// Largest file `editor.open` reads. Reading blocks the editor, so bigger files wait for
/// the large-file support of ADR 0004.
const MAX_OPEN: u64 = 64 << 20;

fn count(cx: &Cx<'_, Editor>) -> usize {
    cx.count().map_or(1, |c| (c.max(1) as usize).min(MAX_COUNT))
}

/// `text` repeated `n` times, refusing results over [`MAX_INSERT`].
fn repeat(text: &str, n: usize) -> Result<String, CommandError> {
    repeat_check(text.len(), n)?;
    Ok(text.repeat(n))
}

fn repeat_check(len: usize, n: usize) -> Result<(), CommandError> {
    if len.saturating_mul(n) > MAX_INSERT {
        return Err(CommandError::failed("count too large"));
    }
    Ok(())
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
struct MoveArgs {
    #[serde(default)]
    extend: bool,
    #[serde(default = "default_true")]
    wrap: bool,
    #[serde(default)]
    big: bool,
}

#[derive(Deserialize)]
struct MoveToArgs {
    line: usize,
    col: usize,
}

#[derive(Deserialize)]
struct TextArgs {
    text: String,
}

#[derive(Deserialize, Default)]
struct RegArgs {
    register: Option<char>,
    #[serde(default)]
    before: Option<bool>,
}

#[derive(Deserialize)]
struct ModeArgs {
    mode: String,
}

#[derive(Deserialize)]
struct PathArgs {
    path: Option<PathBuf>,
}

fn focus(cx: &Cx<'_, Editor>) -> ViewId {
    cx.state.focus
}

/// Publishes context keys that `when` clauses and statuslines read.
fn sync_context(cx: C<'_, '_>) {
    let Some(keys) = cx.state.view_keys(cx.state.focus) else {
        return;
    };
    for (key, value) in keys {
        // Keys are valid names, so `set` cannot fail.
        let _ = cx.context.set(key, value);
    }
}

fn after_selection_change(cx: C<'_, '_>) -> Result<(), CommandError> {
    let view = focus(cx);
    cx.emit(ev(events::SELECTION_CHANGED), json!({ "view": view.0 }))
}

/// Moves every range with `f(snapshot, head, goal)`. Non-vertical motions clear the goal.
fn move_ranges(
    cx: C<'_, '_>,
    extend: bool,
    vertical: bool,
    f: impl Fn(&Snapshot, usize, usize) -> usize,
) -> Result<Value, CommandError> {
    let snap = cx.state.snapshot();
    let depth = cx.depth();
    let id = focus(cx);
    let view = cx
        .state
        .views
        .get_mut(&id)
        .unwrap_or_else(|| unreachable!());
    view.goals.resize(view.selection.ranges.len(), None);
    for (r, goal) in view.selection.ranges.iter_mut().zip(view.goals.iter_mut()) {
        let col = goal.unwrap_or_else(|| motion::char_col(&snap, r.head));
        r.head = f(&snap, r.head, col);
        if !extend {
            r.anchor = r.head;
        }
        *goal = vertical.then_some(col);
    }
    merge_cursors(&mut view.selection, &mut view.goals);
    if depth == 0 {
        // Moving the cursor ends the current undo step.
        view.turn += 1;
    }
    after_selection_change(cx)?;
    Ok(Value::Null)
}

/// Removes duplicate cursors (same anchor and head), keeping the primary.
fn merge_cursors(sel: &mut Selection, goals: &mut Vec<Option<usize>>) {
    let primary = sel.ranges[sel.primary];
    let mut seen = std::collections::HashSet::new();
    let mut ranges = Vec::new();
    let mut new_goals = Vec::new();
    for (r, g) in sel.ranges.iter().zip(goals.iter()) {
        if seen.insert((r.anchor, r.head)) {
            ranges.push(*r);
            new_goals.push(*g);
        }
    }
    sel.primary = ranges.iter().position(|r| *r == primary).unwrap_or(0);
    sel.ranges = ranges;
    *goals = new_goals;
}

fn span(r: SelRange) -> Range<usize> {
    r.anchor.min(r.head)..r.anchor.max(r.head)
}

fn edit_error(e: &EditError) -> CommandError {
    let code = match e {
        EditError::Stale { .. } | EditError::TooOld { .. } => ErrorCode::Stale,
        _ => ErrorCode::Failed,
    };
    CommandError::new(code, e.to_string())
}

/// Applies one hunk per range (or none). Cursors end after the inserted text.
fn edit(
    cx: C<'_, '_>,
    mut per_range: impl FnMut(&Snapshot, SelRange) -> Option<(Range<usize>, String)>,
) -> Result<Value, CommandError> {
    let snap = cx.state.snapshot();
    let id = focus(cx);
    let before = cx.state.views[&id].selection.clone();
    let mut hunks: Vec<(usize, Range<usize>, String)> = before
        .ranges
        .iter()
        .enumerate()
        .filter_map(|(i, r)| per_range(&snap, *r).map(|(range, text)| (i, range, text)))
        .collect();
    // Longest first among hunks with the same start, so a selection's edit wins over a
    // cursor at its start.
    hunks.sort_by_key(|(_, r, _)| (r.start, std::cmp::Reverse(r.end)));
    // Two cursors touching the same text: keep the first hunk.
    let mut last_end = None;
    hunks.retain(|(_, r, _)| {
        let ok = last_end.is_none_or(|e| r.start >= e);
        if ok {
            last_end = Some(r.end);
        }
        ok
    });
    if hunks.is_empty() {
        return Ok(Value::Null);
    }
    // One copy per cursor, so bound the total, not each piece.
    let total = hunks
        .iter()
        .map(|(_, _, t)| t.len())
        .fold(0, usize::saturating_add);
    repeat_check(total, 1)?;
    let changes = ChangeSet::new(
        snap.len(),
        hunks
            .iter()
            .map(|(_, r, t)| Change::new(r.clone(), t.as_str())),
    )
    .map_err(|e| CommandError::failed(e.to_string()))?;
    let mut after = before.map(&changes);
    // Hunks are sorted and disjoint, so each one's new position is its old start plus
    // the size change of the hunks before it.
    let mut delta: isize = 0;
    for (i, r, t) in &hunks {
        let at = r.start.saturating_add_signed(delta) + t.len();
        after.ranges[*i] = SelRange::cursor(at);
        delta += t.len().cast_signed() - r.len().cast_signed();
    }
    let mut goals = vec![None; after.ranges.len()];
    merge_cursors(&mut after, &mut goals);

    let groups = cx.state.mode().groups_edits;
    let depth = cx.depth();
    let author = cx.actor().author();
    let explicit_turn = cx.turn();
    let view = cx
        .state
        .views
        .get_mut(&id)
        .unwrap_or_else(|| unreachable!());
    if !groups && depth == 0 {
        view.turn += 1;
    }
    let turn = if explicit_turn == 0 {
        view.turn
    } else {
        explicit_turn
    };
    let group = cx.actor().group_key(turn);
    let doc_id = cx.state.views[&id].doc;
    let txn = Transaction::new(author, snap.version(), changes)
        .with_selections(before, after.clone())
        .with_group(group);
    let applied = cx
        .state
        .docs
        .get_mut(&doc_id)
        .unwrap_or_else(|| unreachable!())
        .buffer
        .apply(txn)
        .map_err(|e| edit_error(&e))?;
    let view = cx
        .state
        .views
        .get_mut(&id)
        .unwrap_or_else(|| unreachable!());
    view.selection = after;
    view.goals = goals;
    committed(cx, id, &snap, &applied)
}

/// Maps other views of the same document and emits `buffer.changed`. `old` is the text
/// before the edit.
fn committed(
    cx: C<'_, '_>,
    id: ViewId,
    old: &Snapshot,
    applied: &Applied,
) -> Result<Value, CommandError> {
    let doc = cx.state.views[&id].doc;
    let new = cx.state.docs[&doc].buffer.snapshot();
    for (vid, v) in &mut cx.state.views {
        if *vid != id && v.doc == doc {
            v.selection = v.selection.map(&applied.changes);
            // Keep the first visible line on the same text, so a peer's screen does
            // not jump when lines are added or removed above it.
            let start = motion::line_start(old, v.top_line);
            let mapped = applied.changes.map_offset(start, Bias::Left);
            v.top_line = motion::line_of(&new, mapped);
        }
    }
    cx.emit(
        ev(events::BUFFER_CHANGED),
        json!({ "doc": doc.0, "version": applied.version.0 }),
    )?;
    after_selection_change(cx)?;
    Ok(json!(applied.version.0))
}

fn line_ending(s: &Snapshot) -> &'static str {
    let first = s.line_range(0).unwrap_or(0..0);
    if motion::ending_len(s, first) == 2 {
        "\r\n"
    } else {
        "\n"
    }
}

fn yank(cx: C<'_, '_>, register: Option<char>) -> usize {
    let snap = cx.state.snapshot();
    let values: Vec<String> = cx
        .state
        .view()
        .selection
        .ranges
        .iter()
        .filter(|r| r.anchor != r.head)
        .filter_map(|r| snap.slice(span(*r)))
        .collect();
    let n = values.len();
    if n > 0 {
        let reg = Register {
            values,
            linewise: false,
        };
        let name = register.unwrap_or('"');
        if name != '"' {
            cx.state.registers.insert('"', reg.clone());
        }
        cx.state.registers.insert(name, reg);
    }
    n
}

fn undo_redo(cx: C<'_, '_>, redo: bool) -> Result<Value, CommandError> {
    let author = cx.actor().author();
    let id = focus(cx);
    let doc_id = cx.state.views[&id].doc;
    let mut last = None;
    for _ in 0..count(cx) {
        let buffer = &mut cx
            .state
            .docs
            .get_mut(&doc_id)
            .unwrap_or_else(|| unreachable!())
            .buffer;
        let old = buffer.snapshot();
        let r = if redo {
            buffer.redo(&author)
        } else {
            buffer.undo(&author)
        };
        let r = match r {
            Ok(r) => r,
            // Keep the steps already undone; report the failure only if none were.
            Err(_) if last.is_some() => break,
            Err(e) => return Err(CommandError::new(ErrorCode::Stale, e.to_string())),
        };
        match r {
            None => break,
            Some(applied) => {
                let view = cx
                    .state
                    .views
                    .get_mut(&id)
                    .unwrap_or_else(|| unreachable!());
                // Restore the recorded selection only for an edit made in this view;
                // another view's edit (another client's) just shifts this cursor.
                let ours = applied.group.as_ref().is_none_or(|g| {
                    g.actor != spur_text::AuthorId::User || turn_view(g.turn) == id
                });
                view.selection = match &applied.selection {
                    Some(sel) if ours => sel.clone(),
                    _ => view.selection.map(&applied.changes),
                };
                let doc = cx
                    .state
                    .docs
                    .get_mut(&doc_id)
                    .unwrap_or_else(|| unreachable!());
                doc.recheck_saved();
                let snap = doc.buffer.snapshot();
                let view = cx
                    .state
                    .views
                    .get_mut(&id)
                    .unwrap_or_else(|| unreachable!());
                for r in &mut view.selection.ranges {
                    r.anchor = motion::snap_char(&snap, r.anchor);
                    r.head = motion::snap_char(&snap, r.head);
                }
                view.goals = vec![None; view.selection.ranges.len()];
                view.turn += 1;
                last = Some(committed(cx, id, &old, &applied)?);
            }
        }
    }
    Ok(last.unwrap_or(Value::Bool(false)))
}

/// `path` made absolute against the current directory, so documents have one
/// spelling and clients elsewhere can place them. The file need not exist.
fn absolute(path: &Path) -> Result<PathBuf, CommandError> {
    std::path::absolute(path).map_err(|e| CommandError::failed(format!("{}: {e}", path.display())))
}

fn open(cx: C<'_, '_>, path: &Path) -> Result<Value, CommandError> {
    let path = &absolute(path)?;
    let existing = cx
        .state
        .docs
        .iter()
        .find(|(_, d)| d.path.as_deref() == Some(path))
        .map(|(id, _)| *id);
    let doc = if let Some(doc) = existing {
        doc
    } else {
        match std::fs::metadata(path) {
            Ok(m) if m.is_dir() => {
                return Err(CommandError::failed(format!(
                    "{} is a folder",
                    path.display()
                )));
            }
            Ok(m) if m.len() > MAX_OPEN => {
                return Err(CommandError::failed(format!(
                    "{} is larger than {} MiB",
                    path.display(),
                    MAX_OPEN >> 20
                )));
            }
            _ => {}
        }
        let text = match std::fs::read(path) {
            Ok(bytes) => String::from_utf8(bytes).map_err(|_| {
                CommandError::failed(format!("{} is not valid UTF-8", path.display()))
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(CommandError::failed(format!("{}: {e}", path.display()))),
        };
        cx.state.add_doc(Buffer::new(&text), Some(path.clone()))
    };
    // Reuse a view only if the caller owns it, so two clients never share one.
    let owner = cx.client();
    let view = cx
        .state
        .views
        .iter()
        .find(|(_, v)| v.doc == doc && v.owner == owner)
        .map(|(id, _)| *id);
    let view = match view {
        Some(v) => v,
        None => cx.state.add_view_for(doc, owner),
    };
    cx.state.focus = view;
    cx.emit(
        ev(events::VIEW_OPENED),
        json!({ "view": view.0, "doc": doc.0 }),
    )?;
    Ok(json!({ "view": view.0, "doc": doc.0 }))
}

/// Follows `path` through symlinks (also dangling ones) to the file a save should
/// replace.
fn resolve_link(path: &Path) -> PathBuf {
    let mut p = path.to_path_buf();
    // Bounded, like the OS, so a link cycle cannot loop forever.
    for _ in 0..40 {
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => match std::fs::read_link(&p) {
                Ok(next) if next.is_absolute() => p = next,
                Ok(next) => p = p.parent().unwrap_or(Path::new("")).join(next),
                Err(_) => break,
            },
            _ => break,
        }
    }
    p
}

/// Writes `text` to a new temp file next to `path`, then renames it over `path`. A
/// symlink is followed (the link stays a link) and the file's permissions are kept. The
/// temp file is created exclusively, so a planted file or link there is never written
/// through.
fn write_atomic(path: &Path, snap: &Snapshot) -> std::io::Result<()> {
    let target = resolve_link(path);
    let perms = std::fs::metadata(&target).ok().map(|m| m.permissions());
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (mut f, tmp) = {
        let mut attempt = 0u32;
        loop {
            let tmp =
                target.with_file_name(format!(".{name}.spur-{}-{attempt}.tmp", std::process::id()));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
            {
                Ok(f) => break (f, tmp),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 100 => {
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    };
    let write = || -> std::io::Result<()> {
        for chunk in snap.chunks(0..snap.len()).into_iter().flatten() {
            f.write_all(chunk.as_bytes())?;
        }
        f.sync_all()?;
        if let Some(p) = perms {
            std::fs::set_permissions(&tmp, p)?;
        }
        std::fs::rename(&tmp, &target)
    };
    write().inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn save(cx: C<'_, '_>, path: Option<PathBuf>) -> Result<Value, CommandError> {
    let doc_id = cx.state.view().doc;
    let doc = &cx.state.docs[&doc_id];
    let path = path
        .or_else(|| doc.path.clone())
        .ok_or_else(|| CommandError::failed("no file name"))?;
    let path = absolute(&path)?;
    let snap = doc.buffer.snapshot();
    write_atomic(&path, &snap)
        .map_err(|e| CommandError::failed(format!("{}: {e}", path.display())))?;
    let doc = cx
        .state
        .docs
        .get_mut(&doc_id)
        .unwrap_or_else(|| unreachable!());
    doc.saved = snap.version();
    doc.saved_text = snap.clone();
    doc.path = Some(path.clone());
    // Saving ends the current undo step, so one undo returns to the saved text.
    let id = focus(cx);
    if let Some(v) = cx.state.views.get_mut(&id) {
        v.turn += 1;
    }
    let shown = path.display().to_string();
    cx.emit(
        ev(events::BUFFER_SAVED),
        json!({ "doc": doc_id.0, "path": shown }),
    )?;
    Ok(json!({ "path": shown, "bytes": snap.len() }))
}

fn paste(cx: C<'_, '_>, args: &RegArgs) -> Result<Value, CommandError> {
    let name = args.register.unwrap_or('"');
    let Some(reg) = cx.state.registers.get(&name).cloned() else {
        return Ok(Value::Null);
    };
    let before = args.before.unwrap_or(true);
    let times = count(cx);
    let n = reg.values.len().max(1);
    let longest = reg.values.iter().map(String::len).max().unwrap_or(0);
    repeat_check(longest, times)?;
    let mut i = 0;
    edit(cx, |snap, r| {
        let mut text = reg.values.get(i % n).cloned().unwrap_or_default();
        text = text.repeat(times);
        i += 1;
        let at = if reg.linewise {
            let line = motion::line_of(snap, r.head);
            if before {
                motion::line_start(snap, line)
            } else if line + 1 < snap.line_count() {
                motion::line_start(snap, line + 1)
            } else {
                let le = line_ending(snap);
                text = format!("{le}{}", text.strip_suffix(le).unwrap_or(&text));
                snap.len()
            }
        } else if before {
            r.head
        } else {
            motion::right(snap, r.head, false)
        };
        Some((at..at, text))
    })
}

/// Registers the `editor.*` and `buffer.*` commands.
///
/// # Errors
///
/// Returns [`RegisterError`] if any id is already registered.
#[allow(clippy::too_many_lines)]
pub fn register_editor_commands(core: &mut Core<Editor>) -> Result<(), RegisterError> {
    let mut reg = |id: &str,
                   desc: &str,
                   tweak: fn(CommandMeta) -> CommandMeta,
                   f: fn(C<'_, '_>, &Value) -> Result<Value, CommandError>|
     -> Result<(), RegisterError> {
        let meta = tweak(CommandMeta::new(
            CommandId::new(id).unwrap_or_else(|e| unreachable!("{e}")),
            desc,
        ));
        core.register_fn(meta, move |cx, args| {
            let r = f(cx, args);
            sync_context(cx);
            r
        })
        .map(|_| ())
    };
    let plain: fn(CommandMeta) -> CommandMeta = |m| m;
    let rep: fn(CommandMeta) -> CommandMeta = |m| {
        m.repeatable().args_schema(json!({
            "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"]
        }))
    };
    let mv: fn(CommandMeta) -> CommandMeta = |m| {
        m.read_only().args_schema(json!({
            "type": "object",
            "properties": { "extend": { "type": "boolean" }, "wrap": { "type": "boolean" }, "big": { "type": "boolean" } }
        }))
    };
    let edit_meta: fn(CommandMeta) -> CommandMeta = CommandMeta::repeatable;

    reg(
        "editor.insert_text",
        "Insert text at every cursor, replacing selections.",
        rep,
        |cx, a| {
            let t: TextArgs = parse_args(a)?;
            let text = repeat(&t.text, count(cx))?;
            edit(cx, |_, r| Some((span(r), text.clone())))
        },
    )?;
    reg(
        "editor.insert_newline",
        "Insert a line break at every cursor.",
        edit_meta,
        |cx, _| {
            let n = count(cx);
            edit(cx, |s, r| {
                Some((span(r), line_ending(s).repeat(n.min(MAX_COUNT))))
            })
        },
    )?;
    reg(
        "editor.delete_backward",
        "Delete the selection or the grapheme before each cursor.",
        edit_meta,
        |cx, _| {
            let n = count(cx);
            edit(cx, |s, r| {
                if r.anchor != r.head {
                    return Some((span(r), String::new()));
                }
                let start = (0..n).fold(r.head, |at, _| motion::left(s, at, true));
                (start < r.head).then(|| (start..r.head, String::new()))
            })
        },
    )?;
    reg(
        "editor.delete_forward",
        "Delete the selection or the grapheme after each cursor.",
        edit_meta,
        |cx, _| {
            let n = count(cx);
            edit(cx, |s, r| {
                if r.anchor != r.head {
                    return Some((span(r), String::new()));
                }
                let end = (0..n).fold(r.head, |at, _| motion::right(s, at, true));
                (end > r.head).then(|| (r.head..end, String::new()))
            })
        },
    )?;
    reg(
        "editor.delete_selection",
        "Cut every selection into a register.",
        edit_meta,
        |cx, a| {
            let args: RegArgs = parse_args(a)?;
            yank(cx, args.register);
            edit(cx, |_, r| {
                (r.anchor != r.head).then(|| (span(r), String::new()))
            })
        },
    )?;
    reg(
        "editor.yank",
        "Copy every selection into a register.",
        CommandMeta::read_only,
        |cx, a| {
            let args: RegArgs = parse_args(a)?;
            Ok(json!(yank(cx, args.register)))
        },
    )?;
    reg(
        "editor.paste",
        "Paste a register at every cursor (before = at the cursor).",
        edit_meta,
        |cx, a| {
            let args: RegArgs = parse_args(a)?;
            paste(cx, &args)
        },
    )?;

    reg(
        "view.move_to",
        "Put the cursor at a line and char column, dropping other cursors.",
        |m| {
            m.read_only().args_schema(json!({
                "type": "object",
                "properties": {
                    "line": { "type": "integer", "minimum": 0 },
                    "col": { "type": "integer", "minimum": 0 }
                },
                "required": ["line", "col"]
            }))
        },
        |cx, a| {
            let m: MoveToArgs = parse_args(a)?;
            let snap = cx.state.snapshot();
            let line = m.line.min(snap.line_count().saturating_sub(1));
            let at = motion::offset_at_col(&snap, line, m.col);
            let depth = cx.depth();
            let id = focus(cx);
            let view = cx
                .state
                .views
                .get_mut(&id)
                .unwrap_or_else(|| unreachable!());
            view.selection = Selection::cursor(at);
            view.goals = vec![None];
            if depth == 0 {
                // Moving the cursor ends the current undo step.
                view.turn += 1;
            }
            after_selection_change(cx)?;
            Ok(Value::Null)
        },
    )?;
    reg(
        "editor.move_left",
        "Move left by graphemes.",
        mv,
        |cx, a| {
            let m: MoveArgs = parse_args(a)?;
            let n = count(cx);
            move_ranges(cx, m.extend, false, |s, h, _| {
                (0..n).fold(h, |at, _| motion::left(s, at, m.wrap))
            })
        },
    )?;
    reg(
        "editor.move_right",
        "Move right by graphemes.",
        mv,
        |cx, a| {
            let m: MoveArgs = parse_args(a)?;
            let n = count(cx);
            move_ranges(cx, m.extend, false, |s, h, _| {
                (0..n).fold(h, |at, _| motion::right(s, at, m.wrap))
            })
        },
    )?;
    reg(
        "editor.move_up",
        "Move up by lines, keeping the column.",
        mv,
        |cx, a| {
            let m: MoveArgs = parse_args(a)?;
            let n = isize::try_from(count(cx)).unwrap_or(isize::MAX);
            move_ranges(cx, m.extend, true, |s, h, g| motion::vertical(s, h, -n, g))
        },
    )?;
    reg(
        "editor.move_down",
        "Move down by lines, keeping the column.",
        mv,
        |cx, a| {
            let m: MoveArgs = parse_args(a)?;
            let n = isize::try_from(count(cx)).unwrap_or(isize::MAX);
            move_ranges(cx, m.extend, true, |s, h, g| motion::vertical(s, h, n, g))
        },
    )?;
    reg(
        "editor.move_line_start",
        "Move to the start of the line.",
        mv,
        |cx, a| {
            let m: MoveArgs = parse_args(a)?;
            move_ranges(cx, m.extend, false, |s, h, _| {
                motion::line_start(s, motion::line_of(s, h))
            })
        },
    )?;
    reg(
        "editor.move_line_end",
        "Move to the end of the line.",
        mv,
        |cx, a| {
            let m: MoveArgs = parse_args(a)?;
            move_ranges(cx, m.extend, false, |s, h, _| {
                motion::line_end(s, motion::line_of(s, h))
            })
        },
    )?;
    reg(
        "editor.move_first_nonblank",
        "Move to the first non-blank char of the line.",
        mv,
        |cx, a| {
            let m: MoveArgs = parse_args(a)?;
            move_ranges(cx, m.extend, false, |s, h, _| {
                motion::first_nonblank(s, motion::line_of(s, h))
            })
        },
    )?;
    reg(
        "editor.move_doc_start",
        "Move to the start of the document (count = line).",
        mv,
        |cx, a| {
            let m: MoveArgs = parse_args(a)?;
            let line = cx.count().map(|c| (c as usize).saturating_sub(1));
            move_ranges(cx, m.extend, false, |s, _, _| {
                line.map_or(0, |l| motion::first_nonblank(s, l.min(s.line_count() - 1)))
            })
        },
    )?;
    reg(
        "editor.move_doc_end",
        "Move to the end of the document (count = line).",
        mv,
        |cx, a| {
            let m: MoveArgs = parse_args(a)?;
            let line = cx.count().map(|c| (c as usize).saturating_sub(1));
            move_ranges(cx, m.extend, false, |s, _, _| {
                let l = line.unwrap_or(usize::MAX).min(s.line_count() - 1);
                motion::first_nonblank(s, l)
            })
        },
    )?;
    reg(
        "editor.move_word_next",
        "Move to the start of the next word.",
        mv,
        |cx, a| {
            let m: MoveArgs = parse_args(a)?;
            let n = count(cx);
            move_ranges(cx, m.extend, false, |s, h, _| {
                (0..n).fold(h, |at, _| motion::word_next(s, at, m.big))
            })
        },
    )?;
    reg(
        "editor.move_word_prev",
        "Move to the start of the previous word.",
        mv,
        |cx, a| {
            let m: MoveArgs = parse_args(a)?;
            let n = count(cx);
            move_ranges(cx, m.extend, false, |s, h, _| {
                (0..n).fold(h, |at, _| motion::word_prev(s, at, m.big))
            })
        },
    )?;
    reg(
        "editor.move_word_end",
        "Move to the end of the word.",
        mv,
        |cx, a| {
            let m: MoveArgs = parse_args(a)?;
            let n = count(cx);
            move_ranges(cx, m.extend, false, |s, h, _| {
                (0..n).fold(h, |at, _| motion::word_end(s, at, m.big))
            })
        },
    )?;
    reg(
        "editor.collapse_selection",
        "Reduce every selection to its cursor.",
        CommandMeta::read_only,
        |cx, _| move_ranges(cx, false, false, |_, h, _| h),
    )?;

    reg("editor.undo", "Undo your last change.", plain, |cx, _| {
        undo_redo(cx, false)
    })?;
    reg(
        "editor.redo",
        "Redo your last undone change.",
        plain,
        |cx, _| undo_redo(cx, true),
    )?;
    reg(
        "editor.set_mode",
        "Switch the focused view's mode.",
        |m| {
            m.remote_side(RemoteSide::Workspace).args_schema(json!({
            "type": "object", "properties": { "mode": { "type": "string" } }, "required": ["mode"]
        }))
        },
        |cx, a| {
            let m: ModeArgs = parse_args(a)?;
            let Some(spec) = cx.state.modes.get(m.mode.as_str()).cloned() else {
                return Err(CommandError::bad_args(format!("unknown mode {}", m.mode)));
            };
            let was_grouping = cx.state.mode().groups_edits;
            let id = focus(cx);
            let view = cx
                .state
                .views
                .get_mut(&id)
                .unwrap_or_else(|| unreachable!());
            if view.mode == spec.name {
                return Ok(Value::Null);
            }
            if was_grouping {
                view.turn += 1;
            }
            view.mode = spec.name.clone();
            cx.emit(
                ev(events::MODE_CHANGED),
                json!({ "view": id.0, "mode": &*spec.name }),
            )?;
            Ok(Value::Null)
        },
    )?;
    reg(
        "editor.open",
        "Open a file in a new view (an empty buffer if it does not exist).",
        |m| {
            m.args_schema(json!({
            "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"]
        }))
        },
        |cx, a| {
            let p: PathArgs = parse_args(a)?;
            let path = p
                .path
                .ok_or_else(|| CommandError::bad_args("path is required"))?;
            open(cx, &path)
        },
    )?;
    reg(
        "buffer.save",
        "Write the focused document to disk (optionally to a new path).",
        |m| {
            m.args_schema(
                json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
            )
        },
        |cx, a| {
            let p: PathArgs = parse_args(a)?;
            save(cx, p.path)
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Actor, Invocation};

    fn core(text: &str, mode: &str) -> Core<Editor> {
        let mut ed = Editor::new();
        let doc = ed.add_doc(Buffer::new(text), None);
        ed.default_mode = mode.into();
        ed.focus = ed.add_view(doc);
        let mut c = Core::new(ed);
        register_editor_commands(&mut c).unwrap();
        c
    }

    fn run(c: &mut Core<Editor>, id: &str, args: Value) -> Value {
        run_n(c, id, args, None)
    }

    fn run_n(c: &mut Core<Editor>, id: &str, args: Value, n: Option<u32>) -> Value {
        let mut inv = Invocation::new(CommandId::new(id).unwrap(), Actor::User).args(args);
        inv.count = n;
        c.execute(inv).unwrap_or_else(|e| panic!("{id}: {e}"))
    }

    fn text(c: &Core<Editor>) -> String {
        let s = c.state.snapshot();
        s.slice(0..s.len()).unwrap()
    }

    fn head(c: &Core<Editor>) -> usize {
        c.state.primary().head
    }

    fn set_cursors(c: &mut Core<Editor>, at: &[usize]) {
        let id = c.state.focus;
        let v = c.state.views.get_mut(&id).unwrap();
        v.selection = Selection {
            ranges: at.iter().map(|&o| SelRange::cursor(o)).collect(),
            primary: 0,
        };
        v.goals = vec![None; at.len()];
    }

    #[test]
    fn touching_hunks_keep_both_cursors() {
        let mut c = core("abcd", "insert");
        let id = c.state.focus;
        let v = c.state.views.get_mut(&id).unwrap();
        v.selection = Selection {
            ranges: vec![SelRange { anchor: 1, head: 3 }, SelRange::cursor(3)],
            primary: 0,
        };
        v.goals = vec![None; 2];
        run(&mut c, "editor.insert_text", json!({"text": "X"}));
        assert_eq!(text(&c), "aXXd");
        let heads: Vec<_> = c
            .state
            .view()
            .selection
            .ranges
            .iter()
            .map(|r| r.head)
            .collect();
        assert_eq!(heads, vec![2, 3]);
    }

    #[test]
    fn selection_edit_wins_over_cursor_at_its_start() {
        let mut c = core("abcd", "insert");
        let id = c.state.focus;
        let v = c.state.views.get_mut(&id).unwrap();
        v.selection = Selection {
            ranges: vec![SelRange { anchor: 0, head: 3 }, SelRange::cursor(0)],
            primary: 0,
        };
        v.goals = vec![None; 2];
        run(&mut c, "editor.delete_forward", Value::Null);
        assert_eq!(text(&c), "d");
    }

    #[test]
    fn insert_size_bound_counts_every_cursor() {
        let mut c = core(&"\n".repeat(99), "insert");
        set_cursors(&mut c, &(0..100).collect::<Vec<_>>());
        let mut inv = Invocation::new(CommandId::new("editor.insert_text").unwrap(), Actor::User)
            .args(json!({"text": "x".repeat(1 << 20)}));
        inv.count = Some(1);
        assert!(
            c.execute(inv).is_err(),
            "100 cursors x 1 MiB exceeds the cap"
        );
    }

    #[test]
    fn undo_back_to_saved_text_is_unmodified() {
        let mut c = core("ab", "insert");
        run(&mut c, "editor.insert_text", json!({"text": "X"}));
        assert!(c.state.doc().modified());
        run(&mut c, "editor.undo", Value::Null);
        assert!(!c.state.doc().modified());
        run(&mut c, "editor.redo", Value::Null);
        assert!(c.state.doc().modified());
    }

    #[test]
    fn save_ends_the_undo_step() {
        let dir = std::env::temp_dir().join(format!("spur-core-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.txt");
        let mut c = core("", "insert");
        run(&mut c, "editor.open", json!({"path": path}));
        run(&mut c, "editor.insert_text", json!({"text": "a"}));
        run(&mut c, "buffer.save", Value::Null);
        run(&mut c, "editor.insert_text", json!({"text": "b"}));
        run(&mut c, "editor.undo", Value::Null);
        assert_eq!(text(&c), "a");
        assert!(!c.state.doc().modified());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn huge_counts_are_bounded() {
        let mut c = core("ab", "insert");
        let mut inv = Invocation::new(CommandId::new("editor.insert_text").unwrap(), Actor::User)
            .args(json!({"text": "x".repeat(1000)}));
        inv.count = Some(u32::MAX);
        assert!(c.execute(inv).is_err());
        assert_eq!(text(&c), "ab");
        run_n(&mut c, "editor.move_right", Value::Null, Some(u32::MAX));
        assert_eq!(head(&c), 2);
    }

    #[test]
    fn insert_and_delete_multi_cursor() {
        let mut c = core("ab\ncd", "insert");
        set_cursors(&mut c, &[1, 4]);
        run(&mut c, "editor.insert_text", json!({"text": "XY"}));
        assert_eq!(text(&c), "aXYb\ncXYd");
        run_n(&mut c, "editor.delete_backward", Value::Null, Some(2));
        assert_eq!(text(&c), "ab\ncd");
        assert_eq!(c.state.view().selection.ranges.len(), 2);
        run(&mut c, "editor.delete_forward", Value::Null);
        assert_eq!(text(&c), "a\nc");
        assert_eq!(
            crate::ContextLookup::get(c.context(), "buffer.modified"),
            Some(&true.into())
        );
    }

    #[test]
    fn moves_with_count_and_goal() {
        let mut c = core("hello\nhi\nworld", "normal");
        run_n(&mut c, "editor.move_right", json!({"wrap": false}), Some(4));
        assert_eq!(head(&c), 4);
        run(&mut c, "editor.move_down", Value::Null);
        assert_eq!(head(&c), 8);
        run(&mut c, "editor.move_down", Value::Null);
        assert_eq!(head(&c), 13, "goal column 4 restored");
        run(&mut c, "editor.move_doc_start", Value::Null);
        assert_eq!(head(&c), 0);
        run_n(&mut c, "editor.move_doc_start", Value::Null, Some(2));
        assert_eq!(head(&c), 6);
        run(&mut c, "editor.move_doc_end", Value::Null);
        assert_eq!(head(&c), 9);
        run(&mut c, "editor.move_line_end", json!({"extend": true}));
        let r = c.state.primary();
        assert_eq!((r.anchor, r.head), (9, 14));
        run(&mut c, "editor.collapse_selection", Value::Null);
        assert_eq!(c.state.primary().anchor, 14);
    }

    #[test]
    fn undo_steps_follow_modes() {
        let mut c = core("abc", "normal");
        run(&mut c, "editor.delete_forward", Value::Null);
        run(&mut c, "editor.delete_forward", Value::Null);
        assert_eq!(text(&c), "c");
        run(&mut c, "editor.undo", Value::Null);
        assert_eq!(text(&c), "bc", "normal-mode edits are separate steps");
        run(&mut c, "editor.undo", Value::Null);

        // `cw`-like: delete in normal, then insert session = one step.
        run(&mut c, "editor.delete_forward", Value::Null);
        run(&mut c, "editor.set_mode", json!({"mode": "insert"}));
        run(&mut c, "editor.insert_text", json!({"text": "X"}));
        run(&mut c, "editor.insert_text", json!({"text": "Y"}));
        run(&mut c, "editor.set_mode", json!({"mode": "normal"}));
        assert_eq!(text(&c), "XYbc");
        run(&mut c, "editor.undo", Value::Null);
        assert_eq!(text(&c), "abc");
        run(&mut c, "editor.redo", Value::Null);
        assert_eq!(text(&c), "XYbc");
        assert_eq!(head(&c), 2);
    }

    #[test]
    fn moving_breaks_insert_undo_step() {
        let mut c = core("", "insert");
        run(&mut c, "editor.insert_text", json!({"text": "ab"}));
        run(&mut c, "editor.move_left", Value::Null);
        run(&mut c, "editor.insert_text", json!({"text": "X"}));
        assert_eq!(text(&c), "aXb");
        run(&mut c, "editor.undo", Value::Null);
        assert_eq!(text(&c), "ab");
    }

    #[test]
    fn agent_undo_leaves_user_edits() {
        let mut c = core("hello", "insert");
        run(&mut c, "editor.insert_text", json!({"text": "> "}));
        let agent = Actor::Agent {
            session_id: "s".into(),
            agent_kind: "t".into(),
            turn_id: 1,
            tool_call_id: None,
        };
        run(&mut c, "editor.move_line_end", Value::Null);
        let id = CommandId::new("editor.insert_text").unwrap();
        c.execute(Invocation::new(id, agent.clone()).args(json!({"text": "!"})))
            .unwrap();
        assert_eq!(text(&c), "> hello!");
        c.execute(Invocation::new(
            CommandId::new("editor.undo").unwrap(),
            agent,
        ))
        .unwrap();
        assert_eq!(text(&c), "> hello");
    }

    #[test]
    fn yank_and_paste() {
        let mut c = core("one two", "normal");
        run_n(
            &mut c,
            "editor.move_right",
            json!({"extend": true}),
            Some(3),
        );
        run(&mut c, "editor.yank", Value::Null);
        run(&mut c, "editor.move_line_end", Value::Null);
        run(&mut c, "editor.paste", Value::Null);
        assert_eq!(text(&c), "one twoone");
        c.state.registers.insert(
            'a',
            Register {
                values: vec!["L\n".into()],
                linewise: true,
            },
        );
        run(
            &mut c,
            "editor.paste",
            json!({"register": "a", "before": false}),
        );
        assert_eq!(text(&c), "one twoone\nL");
        run(&mut c, "editor.paste", json!({"register": "a"}));
        assert_eq!(text(&c), "one twoone\nL\nL");
    }

    #[test]
    fn newline_keeps_crlf() {
        let mut c = core("a\r\nb", "insert");
        run(&mut c, "editor.move_doc_end", Value::Null);
        run(&mut c, "editor.move_line_end", Value::Null);
        run(&mut c, "editor.insert_newline", Value::Null);
        assert_eq!(text(&c), "a\r\nb\r\n");
    }

    #[test]
    fn open_save_round_trip() {
        let dir = std::env::temp_dir().join(format!("spur-core-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.txt");
        std::fs::write(&path, "x\r\ny").unwrap();
        let mut c = core("", "insert");
        run(&mut c, "editor.open", json!({"path": path}));
        assert_eq!(text(&c), "x\r\ny");
        run(&mut c, "editor.insert_text", json!({"text": "0"}));
        assert!(c.state.doc().modified());
        run(&mut c, "buffer.save", Value::Null);
        assert!(!c.state.doc().modified());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "0x\r\ny");
        let views = c.state.views.len();
        run(&mut c, "editor.open", json!({"path": path}));
        assert_eq!(
            c.state.views.len(),
            views,
            "reopening focuses the existing view"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            // Saving through a symlink keeps the link and the target's permissions.
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            let link = dir.join("link.txt");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            let mut l = core("", "insert");
            run(&mut l, "editor.open", json!({"path": link}));
            run(&mut l, "editor.insert_text", json!({"text": "L"}));
            run(&mut l, "buffer.save", Value::Null);
            assert!(
                std::fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "L0x\r\ny");
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);

            // A planted temp-file link is never written through.
            let victim = dir.join("victim");
            std::fs::write(&victim, "precious").unwrap();
            for n in 0..3 {
                let planted = dir.join(format!(".f.txt.spur-{}-{n}.tmp", std::process::id()));
                std::os::unix::fs::symlink(&victim, planted).unwrap();
            }
            run(&mut l, "editor.insert_text", json!({"text": "M"}));
            run(&mut l, "buffer.save", Value::Null);
            assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "LM0x\r\ny");

            // A dangling link stays a link; the save creates its target.
            let dangling = dir.join("dangling.txt");
            std::os::unix::fs::symlink("not-here.txt", &dangling).unwrap();
            let mut d = core("", "insert");
            run(&mut d, "editor.open", json!({"path": dangling}));
            run(&mut d, "editor.insert_text", json!({"text": "D"}));
            run(&mut d, "buffer.save", Value::Null);
            assert!(
                std::fs::symlink_metadata(&dangling)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(
                std::fs::read_to_string(dir.join("not-here.txt")).unwrap(),
                "D"
            );
        }
        let missing = dir.join("new.txt");
        run(&mut c, "editor.open", json!({"path": missing}));
        assert_eq!(text(&c), "");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unknown_mode_is_bad_args() {
        let mut c = core("", "insert");
        let e = c
            .execute(
                Invocation::new(CommandId::new("editor.set_mode").unwrap(), Actor::User)
                    .args(json!({"mode": "nope"})),
            )
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::BadArgs);
    }

    #[test]
    fn viewport_and_scroll() {
        let text: String = (0..100).fold(String::new(), |mut s, i| {
            use std::fmt::Write as _;
            let _ = writeln!(s, "line {i}");
            s
        });
        let mut c = core(&text, "normal");
        run_n(&mut c, "editor.move_down", Value::Null, Some(50));
        c.state.scroll_to_cursor(10, 2);
        let vp = c.state.viewport(10);
        assert_eq!(vp.top_line, 43);
        assert_eq!(vp.lines.len(), 10);
        assert_eq!(vp.lines[7], "line 50");
        assert_eq!(vp.cursors[0].0.line, 50);
        assert_eq!(vp.mode.as_ref(), "normal");
        run(&mut c, "editor.move_doc_start", Value::Null);
        c.state.scroll_to_cursor(10, 2);
        assert_eq!(c.state.viewport(10).top_line, 0);
    }

    /// Runs `id` as client `client` in `view`, the way a server does.
    fn run_in(c: &mut Core<Editor>, view: ViewId, client: u64, id: &str, args: Value) {
        c.state.focus = view;
        let inv = Invocation::new(CommandId::new(id).unwrap(), Actor::User)
            .args(args)
            .client(crate::ClientId(client));
        c.execute(inv).unwrap_or_else(|e| panic!("{id}: {e}"));
    }

    #[test]
    fn two_views_keep_separate_undo_steps() {
        let mut c = core("\n\n\n", "insert");
        let a = c.state.focus;
        let doc = c.state.views[&a].doc;
        let b = c.state.add_view(doc);
        run_in(&mut c, b, 2, "editor.move_down", Value::Null);
        run_in(&mut c, b, 2, "editor.move_down", Value::Null);
        // Interleaved typing in insert mode, where each view groups its own edits.
        for (wa, wb) in ["he", "ll", "o"].iter().zip(["wo", "rl", "d"]) {
            run_in(&mut c, a, 1, "editor.insert_text", json!({ "text": wa }));
            run_in(&mut c, b, 2, "editor.insert_text", json!({ "text": wb }));
        }
        assert_eq!(text(&c), "hello\n\nworld\n");
        // Steps never merge across views: six steps, undone newest first. Undo is per
        // person and document (as in other editors' split views), so A's undo takes
        // B's newest step, but A's cursor only shifts; it never jumps to B's.
        let a_head = c.state.views[&a].selection.ranges[0].head;
        run_in(&mut c, a, 1, "editor.undo", Value::Null);
        assert_eq!(text(&c), "hello\n\nworl\n");
        assert_eq!(c.state.views[&a].selection.ranges[0].head, a_head);
        assert_eq!(c.state.views[&b].selection.ranges[0].head, 11);
        run_in(&mut c, a, 1, "editor.undo", Value::Null);
        assert_eq!(text(&c), "hell\n\nworl\n");
        assert_eq!(c.state.views[&a].selection.ranges[0].head, 4);
        assert_eq!(c.state.views[&b].selection.ranges[0].head, 10);
        assert_ne!(c.state.views[&a].turn >> 32, c.state.views[&b].turn >> 32);
    }

    #[test]
    fn open_reuses_only_the_callers_view() {
        let dir = std::env::temp_dir().join(format!("spur-open-owner-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.txt");
        std::fs::write(&path, "x\n").unwrap();
        let mut c = core("", "insert");
        let start = c.state.focus;
        let p = json!({ "path": path });
        run_in(&mut c, start, 1, "editor.open", p.clone());
        let va = c.state.focus;
        run_in(&mut c, start, 2, "editor.open", p.clone());
        let vb = c.state.focus;
        assert_ne!(va, vb);
        assert_eq!(c.state.views[&va].owner, Some(crate::ClientId(1)));
        assert_eq!(c.state.views[&vb].owner, Some(crate::ClientId(2)));
        run_in(&mut c, start, 1, "editor.open", p);
        assert_eq!(c.state.focus, va);
        let inv = Invocation::new(CommandId::new("editor.open").unwrap(), Actor::User)
            .args(json!({ "path": dir }));
        assert!(c.execute(inv).is_err(), "a folder is refused");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lines_added_above_keep_other_views_on_their_text() {
        let body = (0..40).fold(String::new(), |s, i| s + &format!("line {i}\n"));
        let mut c = core(&body, "insert");
        let a = c.state.focus;
        let doc = c.state.views[&a].doc;
        let b = c.state.add_view(doc);
        c.state.views.get_mut(&b).unwrap().top_line = 20;
        run_in(
            &mut c,
            a,
            1,
            "editor.insert_text",
            json!({ "text": "x\ny\nz\n" }),
        );
        assert_eq!(c.state.views[&b].top_line, 23);
        assert_eq!(
            c.state.viewport_of(b, 1, usize::MAX).unwrap().lines,
            vec!["line 20"]
        );
        // An edit below the top line does not scroll it.
        c.state.views.get_mut(&a).unwrap().selection = Selection::cursor(c.state.snapshot().len());
        run_in(
            &mut c,
            a,
            1,
            "editor.insert_text",
            json!({ "text": "tail\n" }),
        );
        assert_eq!(c.state.views[&b].top_line, 23);
    }

    #[test]
    fn view_helpers_address_any_view() {
        let mut c = core("a\nb\nc\n", "insert");
        let a = c.state.focus;
        let doc = c.state.views[&a].doc;
        let b = c.state.add_view(doc);
        assert_eq!(
            c.state.viewport_of(a, 2, usize::MAX),
            Some(c.state.viewport(2))
        );
        assert!(!c.state.remove_view(a), "focused view stays");
        let keys = c.state.view_keys(b).unwrap();
        assert_eq!(
            keys[0],
            ("editor.mode", crate::CtxValue::Str("insert".into()))
        );
        assert!(c.state.remove_view(b));
        assert!(c.state.viewport_of(b, 2, usize::MAX).is_none());
        assert!(!c.state.remove_view(b));
    }

    #[test]
    fn move_to_scroll_and_clipped_viewports() {
        let mut c = core("ab\ncdéf\ng\n", "insert");
        let v = c.state.focus;
        run(&mut c, "view.move_to", json!({ "line": 1, "col": 3 }));
        let p = c.state.primary_point(v).unwrap();
        assert_eq!((p.line, p.column), (1, 3));
        // Past the end clamps to the last line and its end.
        run(&mut c, "view.move_to", json!({ "line": 99, "col": 99 }));
        assert_eq!(c.state.primary_point(v).unwrap().line, 3);
        let vp = c.state.viewport_of(v, 3, 3).unwrap();
        assert_eq!(vp.lines, vec!["ab", "cdé", "g"]);
        assert_eq!(vp.clipped, vec![false, true, false]);
        c.state.scroll_by(v, 2);
        assert_eq!(c.state.views[&v].top_line, 2);
        c.state.scroll_by(v, 100);
        assert_eq!(c.state.views[&v].top_line, 3);
        c.state.scroll_by(v, -100);
        assert_eq!(c.state.views[&v].top_line, 0);
        let turn = c.state.views[&v].turn;
        c.state.end_undo_step(v);
        assert_eq!(c.state.views[&v].turn, turn + 1);
    }

    #[test]
    fn open_stores_an_absolute_path() {
        let mut c = core("", "insert");
        let name = format!("spur-not-a-file-{}.txt", std::process::id());
        run(&mut c, "editor.open", json!({ "path": name }));
        let doc = c.state.view().doc;
        let path = c.state.docs[&doc].path.clone().unwrap();
        assert!(path.is_absolute(), "{}", path.display());
        assert!(path.ends_with(&name));
        // Another spelling of the same path finds the same document.
        run(
            &mut c,
            "editor.open",
            json!({ "path": format!("./{name}") }),
        );
        assert_eq!(c.state.view().doc, doc);
    }

    #[test]
    fn viewports_split_every_line_ending_and_cut_long_lines() {
        let c = core("ab\r\ncd\re\n\r\nxyzw\nlast\n", "insert");
        let v = c.state.focus;
        let vp = c.state.viewport_of(v, 10, 4).unwrap();
        assert_eq!(vp.lines, ["ab", "cd", "e", "", "xyzw", "last", ""]);
        assert_eq!(vp.clipped, [false; 7]);
        let vp = c.state.viewport_of(v, 10, 3).unwrap();
        assert_eq!(vp.lines[4], "xyz");
        assert!(vp.clipped[4]);
        // An empty document has one empty line.
        let c = core("", "insert");
        let vp = c.state.viewport_of(c.state.focus, 5, 3).unwrap();
        assert_eq!(vp.lines, [""]);
        // A line too long to copy whole is read only up to the cut.
        let long = format!("{}\nshort\n", "é".repeat(100_000));
        let c = core(&long, "insert");
        let vp = c.state.viewport_of(c.state.focus, 3, 5).unwrap();
        assert_eq!(vp.lines, ["ééééé", "short", ""]);
        assert_eq!(vp.clipped, [true, false, false]);
    }
}
