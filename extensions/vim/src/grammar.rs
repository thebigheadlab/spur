//! The Vim grammar: operators over motions and text objects, visual selections,
//! find-char, `.` repeat, macros, the `:` command line and `/` search.

use std::ops::Range;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use spur_text::{SelRange, Snapshot};

use spur_core::editor::kit::{
    C, Kind, after_selection_change, apply_hunks, count, ev, focus, line_ending, move_ranges, open,
    save, span, sync_context,
};
use spur_core::editor::{Call, Doc, Editor, Register, motion};
use spur_core::{
    CommandError, CommandId, CommandMeta, Core, Cx, RegisterError, events, parse_args,
};

use crate::objects;
use crate::state::{Cmdline, PendingOp, pending, set_pending, vim};

const OPS: &[&str] = &[
    "delete",
    "change",
    "yank",
    "indent",
    "outdent",
    "lower",
    "upper",
    "toggle_case",
];

const INDENT: &str = "    ";

fn set_mode(cx: C<'_, '_>, mode: &Arc<str>) -> Result<(), CommandError> {
    let id = focus(cx);
    let grouping = cx.state.mode().groups_edits;
    let view = cx
        .state
        .views
        .get_mut(&id)
        .unwrap_or_else(|| unreachable!());
    if view.mode == *mode {
        return Ok(());
    }
    if grouping {
        view.bump_turn();
    }
    view.mode = mode.clone();
    cx.emit(
        ev(events::MODE_CHANGED),
        json!({ "view": id.0, "mode": &**mode }),
    )
}

/// Full-line span of lines `first..=last`, including the last line's ending.
fn line_span(s: &Snapshot, first: usize, last: usize) -> Range<usize> {
    let end = s.line_range(last).map_or(s.len(), |r| r.end);
    motion::line_start(s, first)..end
}

/// Stores text in registers the way Vim does: `"` always, `0` for yanks, `1`-`9` for
/// multi-line deletes, `-` for small deletes, nothing for `_`.
fn store(ed: &mut Editor, name: Option<char>, values: Vec<String>, linewise: bool, yank: bool) {
    if name == Some('_') || values.is_empty() {
        return;
    }
    let reg = Register { values, linewise };
    if let Some(c) = name.filter(|&c| c != '"') {
        ed.registers.insert(c, reg.clone());
    } else if yank {
        ed.registers.insert('0', reg.clone());
    } else if linewise || reg.values.iter().any(|v| v.contains('\n')) {
        for i in (1..9u8).rev() {
            let from = char::from(b'0' + i);
            if let Some(r) = ed.registers.remove(&from) {
                ed.registers.insert(char::from(b'1' + i), r);
            }
        }
        ed.registers.insert('1', reg.clone());
    } else {
        ed.registers.insert('-', reg.clone());
    }
    ed.registers.insert('"', reg);
}

/// Applies the pending operator (taken from the view) to `ranges` (range index, span,
/// linewise). Linewise spans cover whole lines.
#[allow(clippy::too_many_lines)]
pub(crate) fn apply_op(
    cx: C<'_, '_>,
    op: &PendingOp,
    ranges: Vec<(usize, Range<usize>, bool)>,
) -> Result<Value, CommandError> {
    let id = focus(cx);
    set_pending(cx.state, None);
    set_mode(cx, &op.return_mode)?;
    let snap = cx.state.snapshot();
    let le = line_ending(&snap);
    let linewise = ranges.iter().any(|r| r.2);
    let values: Vec<String> = ranges
        .iter()
        .filter_map(|(_, r, lw)| {
            let mut t = snap.slice(r.clone())?;
            if *lw && !t.ends_with('\n') && !t.ends_with('\r') {
                t.push_str(le);
            }
            Some(t)
        })
        .collect();
    let op_name = &*op.op;
    match op_name {
        "yank" => {
            store(cx.state, op.register, values, linewise, true);
            let view = cx
                .state
                .views
                .get_mut(&id)
                .unwrap_or_else(|| unreachable!());
            for (i, r, lw) in &ranges {
                if let Some(slot) = view.selection.ranges.get_mut(*i) {
                    let at = if *lw {
                        let line = motion::line_of(&snap, r.start);
                        if line < motion::line_of(&snap, slot.head) {
                            motion::offset_at_col(&snap, line, motion::char_col(&snap, slot.head))
                        } else {
                            slot.head
                        }
                    } else {
                        r.start
                    };
                    *slot = SelRange::cursor(at);
                }
            }
            after_selection_change(cx)?;
            Ok(Value::Null)
        }
        "delete" | "change" => {
            store(cx.state, op.register, values, linewise, false);
            let change = op_name == "change";
            let hunks = ranges
                .into_iter()
                .map(|(i, r, lw)| {
                    let r = if !lw {
                        r
                    } else if change {
                        // `cc`: keep one empty line.
                        let first = motion::line_of(&snap, r.start);
                        let last = motion::line_of(&snap, r.end.saturating_sub(1).max(r.start));
                        motion::line_start(&snap, first)..motion::line_end(&snap, last)
                    } else if r.end == snap.len()
                        && r.start > 0
                        && !snap.slice(r.clone()).is_some_and(|t| t.ends_with('\n'))
                    {
                        // Last line without an ending: take the previous line break.
                        let first = motion::line_of(&snap, r.start);
                        motion::line_end(&snap, first - 1)..r.end
                    } else {
                        r
                    };
                    (i, r, String::new())
                })
                .collect();
            let result = apply_hunks(cx, &snap, hunks, |start, _, _, _| start)?;
            if linewise && !change {
                let s = cx.state.snapshot();
                let view = cx
                    .state
                    .views
                    .get_mut(&id)
                    .unwrap_or_else(|| unreachable!());
                for r in &mut view.selection.ranges {
                    *r = SelRange::cursor(motion::first_nonblank(&s, motion::line_of(&s, r.head)));
                }
            }
            if change {
                set_mode(cx, &Arc::from("insert"))?;
            }
            Ok(result)
        }
        "indent" | "outdent" => {
            let mut hunks = Vec::new();
            for (i, r, _) in &ranges {
                let first = motion::line_of(&snap, r.start);
                let last = motion::line_of(&snap, r.end.saturating_sub(1).max(r.start));
                for line in first..=last {
                    let start = motion::line_start(&snap, line);
                    let text = motion::line_text(&snap, line);
                    if op_name == "indent" {
                        if !text.is_empty() {
                            hunks.push((*i, start..start, INDENT.to_owned()));
                        }
                    } else {
                        let n = if text.starts_with('\t') {
                            1
                        } else {
                            text.chars()
                                .take(INDENT.len())
                                .take_while(|&c| c == ' ')
                                .count()
                        };
                        if n > 0 {
                            hunks.push((*i, start..start + n, String::new()));
                        }
                    }
                }
            }
            let result = apply_hunks(cx, &snap, hunks, |start, _, _, _| start)?;
            let s = cx.state.snapshot();
            let first_lines: Vec<usize> = ranges
                .iter()
                .map(|(_, r, _)| motion::line_of(&snap, r.start))
                .collect();
            let view = cx
                .state
                .views
                .get_mut(&id)
                .unwrap_or_else(|| unreachable!());
            for (r, line) in view.selection.ranges.iter_mut().zip(first_lines) {
                *r = SelRange::cursor(motion::first_nonblank(&s, line));
            }
            Ok(result)
        }
        "lower" | "upper" | "toggle_case" => {
            let hunks = ranges
                .iter()
                .filter_map(|(i, r, _)| {
                    let t = snap.slice(r.clone())?;
                    let new: String = match op_name {
                        "lower" => t.to_lowercase(),
                        "upper" => t.to_uppercase(),
                        _ => t
                            .chars()
                            .flat_map(|c| {
                                if c.is_lowercase() {
                                    c.to_uppercase().collect::<Vec<_>>()
                                } else {
                                    c.to_lowercase().collect()
                                }
                            })
                            .collect(),
                    };
                    (new != t).then(|| (*i, r.clone(), new))
                })
                .collect();
            apply_hunks(cx, &snap, hunks, |start, _, _, _| start)
        }
        other => Err(CommandError::bad_args(format!("unknown operator {other}"))),
    }
}

/// Applies the pending operator to the range a motion covers.
pub(crate) fn operate_motion(
    cx: C<'_, '_>,
    kind: Kind,
    f: &dyn Fn(&Snapshot, usize, usize) -> usize,
) -> Result<Value, CommandError> {
    let Some(op) = pending(cx.state) else {
        return Ok(Value::Null);
    };
    let snap = cx.state.snapshot();
    let times = op.count.unwrap_or(1).max(1);
    let ranges: Vec<(usize, Range<usize>, bool)> = cx
        .state
        .view()
        .selection
        .ranges
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let head = r.head;
            let goal = motion::char_col(&snap, head);
            let mut target = (0..times).fold(head, |h, _| f(&snap, h, goal));
            let head_line = motion::line_of(&snap, head);
            match kind {
                Kind::Linewise => {
                    let tl = motion::line_of(&snap, target);
                    Some((
                        i,
                        line_span(&snap, head_line.min(tl), head_line.max(tl)),
                        true,
                    ))
                }
                Kind::Inclusive => {
                    let (a, b) = (head.min(target), head.max(target));
                    let b = snap.next_grapheme_boundary(b).unwrap_or(b);
                    (a < b).then_some((i, a..b, false))
                }
                Kind::Exclusive => {
                    // An exclusive motion ending at the start of a later line stops at the
                    // end of the line before it (`dw` on a line's last word).
                    if target > head {
                        let tl = motion::line_of(&snap, target);
                        if tl > head_line && target <= motion::first_nonblank(&snap, tl) {
                            target = motion::line_end(&snap, tl - 1);
                        }
                    }
                    let (a, b) = (head.min(target), head.max(target));
                    (a < b).then_some((i, a..b, false))
                }
            }
        })
        .collect();
    if ranges.is_empty() {
        set_pending(cx.state, None);
        set_mode(cx, &op.return_mode)?;
        return Ok(Value::Null);
    }
    apply_op(cx, &op, ranges)
}

#[derive(Deserialize)]
struct OpArgs {
    op: String,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    linewise: bool,
}

#[derive(Deserialize)]
struct CharArgs {
    char: char,
    #[serde(default)]
    inner: bool,
    #[serde(default)]
    till: bool,
    #[serde(default)]
    backward: bool,
}

#[derive(Deserialize, Default)]
#[allow(clippy::struct_excessive_bools)]
struct FlagArgs {
    #[serde(default)]
    reverse: bool,
    #[serde(default)]
    above: bool,
    #[serde(default)]
    extend: bool,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    prefix: Option<char>,
    #[serde(default)]
    text: Option<String>,
}

fn check_op(op: &str) -> Result<Arc<str>, CommandError> {
    if OPS.contains(&op) {
        Ok(op.into())
    } else {
        Err(CommandError::bad_args(format!("unknown operator {op}")))
    }
}

#[allow(clippy::fn_params_excessive_bools)]
fn find_motion(
    cx: C<'_, '_>,
    c: char,
    till: bool,
    backward: bool,
    repeat: bool,
    extend: bool,
) -> Result<Value, CommandError> {
    let n = count(cx);
    let kind = if backward {
        Kind::Exclusive
    } else {
        Kind::Inclusive
    };
    let start = |s: &Snapshot, h: usize| {
        if repeat && till {
            if backward {
                motion::left(s, h, false)
            } else {
                motion::right(s, h, false)
            }
        } else {
            h
        }
    };
    if let Some(r) = without_target(cx, |s, h| {
        objects::find_char(s, start(s, h), c, n, till, backward)
    }) {
        return r;
    }
    move_ranges(cx, extend, false, kind, move |s, h, _| {
        // A repeated `t` starts one char further so it does not stick before its target.
        let from = if repeat && till {
            if backward {
                motion::left(s, h, false)
            } else {
                motion::right(s, h, false)
            }
        } else {
            h
        };
        objects::find_char(s, from, c, n, till, backward).unwrap_or(h)
    })
}

/// A motion that finds no target for any cursor does nothing, and cancels a pending
/// operator instead of applying it to the char under the cursor.
fn without_target(
    cx: C<'_, '_>,
    target: impl Fn(&Snapshot, usize) -> Option<usize>,
) -> Option<Result<Value, CommandError>> {
    let snap = cx.state.snapshot();
    let none = cx
        .state
        .view()
        .selection
        .ranges
        .iter()
        .all(|r| target(&snap, r.head).is_none());
    if !none {
        return None;
    }
    Some(match pending(cx.state) {
        Some(op) => cancel(cx, &op),
        None => Ok(Value::Null),
    })
}

fn search(
    cx: C<'_, '_>,
    pattern: &str,
    backward: bool,
    extend: bool,
) -> Result<Value, CommandError> {
    let re = regex::Regex::new(pattern).map_err(|e| CommandError::bad_args(e.to_string()))?;
    let snap = cx.state.snapshot();
    let text = snap.slice(0..snap.len()).unwrap_or_default();
    let starts: Vec<usize> = re.find_iter(&text).map(|m| m.start()).collect();
    if starts.is_empty() {
        return Err(CommandError::failed(format!(
            "Pattern not found: {pattern}"
        )));
    }
    let n = count(cx);
    move_ranges(cx, extend, false, Kind::Exclusive, move |_, h, _| {
        (0..n).fold(h, |at, _| {
            if backward {
                starts
                    .iter()
                    .rev()
                    .find(|&&s| s < at)
                    .or(starts.last())
                    .copied()
                    .unwrap_or(at)
            } else {
                starts
                    .iter()
                    .find(|&&s| s > at)
                    .or(starts.first())
                    .copied()
                    .unwrap_or(at)
            }
        })
    })
}

fn replay(cx: C<'_, '_>, calls: &[Call], times: usize) -> Result<Value, CommandError> {
    let id = focus(cx);
    {
        let view = cx
            .state
            .views
            .get_mut(&id)
            .unwrap_or_else(|| unreachable!());
        view.bump_turn();
        view.turn_lock += 1;
    }
    let mut result = Ok(Value::Null);
    'outer: for _ in 0..times {
        for c in calls {
            result = cx.execute_with(&c.command, &c.args, c.count, c.register);
            if result.is_err() {
                break 'outer;
            }
        }
    }
    let view = cx
        .state
        .views
        .get_mut(&id)
        .unwrap_or_else(|| unreachable!());
    view.turn_lock -= 1;
    view.bump_turn();
    result
}

fn ex(cx: C<'_, '_>, line: &str) -> Result<Value, CommandError> {
    let line = line.trim();
    let (cmd, arg) = line
        .split_once(char::is_whitespace)
        .map_or((line, ""), |(c, a)| (c, a.trim()));
    let path = (!arg.is_empty()).then(|| std::path::PathBuf::from(arg));
    let quit = |cx: C<'_, '_>, force: bool| -> Result<Value, CommandError> {
        if !force && cx.state.docs.values().any(Doc::modified) {
            return Err(CommandError::failed(
                "No write since last change (add ! to override)",
            ));
        }
        cx.state.quit = true;
        Ok(Value::Null)
    };
    match cmd {
        "" => Ok(Value::Null),
        "w" | "write" => save(cx, path),
        "q" | "quit" | "qa" | "qall" => quit(cx, false),
        "q!" | "quit!" | "qa!" | "qall!" => quit(cx, true),
        "wq" | "x" | "xit" => {
            save(cx, path)?;
            quit(cx, false)
        }
        "e" | "edit" => match path {
            Some(p) => open(cx, &p),
            None => Err(CommandError::bad_args("E32: No file name")),
        },
        _ if cmd.chars().all(|c| c.is_ascii_digit()) => {
            let line: u32 = cmd.parse().unwrap_or(1);
            cx.execute_with(
                &CommandId::new("editor.move_doc_start").unwrap_or_else(|e| unreachable!("{e}")),
                &Value::Null,
                Some(line.max(1)),
                None,
            )
        }
        _ => {
            // Any command by id, with optional JSON args: `:editor.move_down {"extend":true}`.
            let id = CommandId::new(cmd)
                .map_err(|_| CommandError::failed(format!("Not an editor command: {line}")))?;
            let args: Value = if arg.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(arg).map_err(|e| CommandError::bad_args(e.to_string()))?
            };
            cx.execute(&id, &args)
        }
    }
}

fn close_cmdline(cx: C<'_, '_>) -> Result<Option<Cmdline>, CommandError> {
    let Some(c) = vim(cx.state).cmdline.take() else {
        return Ok(None);
    };
    let ret = c.return_mode.clone();
    let id = focus(cx);
    let view = cx
        .state
        .views
        .get_mut(&id)
        .unwrap_or_else(|| unreachable!());
    view.mode = ret.clone();
    cx.emit(
        ev(events::MODE_CHANGED),
        json!({ "view": id.0, "mode": &*ret }),
    )?;
    Ok(Some(c))
}

/// Publishes the preset's context keys and mirrors its command line for frontends.
fn sync(cx: C<'_, '_>) {
    let op = pending(cx.state).map(|o| o.op);
    let st = vim(cx.state);
    let recording = st.macro_recording.as_ref().map(|m| m.0.to_string());
    let line = st
        .cmdline
        .as_ref()
        .map(|c| format!("{}{}", c.prefix, c.text));
    cx.state.line_prompt = line;
    // Keys are valid names, so `set` cannot fail.
    match op {
        Some(op) => {
            let _ = cx.context.set("vim.operator", &*op);
        }
        None => {
            cx.context.remove("vim.operator");
        }
    }
    match recording {
        Some(r) => {
            let _ = cx.context.set("vim.recording", r);
        }
        None => {
            cx.context.remove("vim.recording");
        }
    }
}

/// With an operator pending, a motion applies it to the motion's range.
pub(crate) fn motion_hook(
    cx: &mut Cx<'_, Editor>,
    kind: Kind,
    f: &dyn Fn(&Snapshot, usize, usize) -> usize,
) -> Option<Result<Value, CommandError>> {
    pending(cx.state)?;
    let r = operate_motion(cx, kind, f);
    sync(cx);
    Some(r)
}

/// Registers the modal grammar commands.
///
/// # Errors
///
/// Returns [`RegisterError`] if any id is already registered.
#[allow(clippy::too_many_lines)]
pub(crate) fn register(core: &mut Core<Editor>) -> Result<(), RegisterError> {
    let mut reg = |id: &str,
                   desc: &str,
                   repeatable: bool,
                   f: fn(C<'_, '_>, &Value) -> Result<Value, CommandError>|
     -> Result<(), RegisterError> {
        let mut meta = CommandMeta::new(
            CommandId::new(id).unwrap_or_else(|e| unreachable!("{e}")),
            desc,
        );
        meta.repeatable = repeatable;
        core.register_fn(meta, move |cx, args| {
            let had_op = pending(cx.state).is_some();
            let r = f(cx, args);
            // A failed motion (no previous search, bad pattern) drops a pending operator.
            if r.is_err()
                && had_op
                && let Some(op) = pending(cx.state)
            {
                let _ = cancel(cx, &op);
            }
            sync_context(cx);
            sync(cx);
            r
        })
        .map(|_| ())
    };

    reg(
        "vim.begin_operator",
        "Start an operator (delete, change, yank, ...) that waits for a motion or text object.",
        true,
        |cx, a| {
            let args: OpArgs = parse_args(a)?;
            let op = check_op(&args.op)?;
            let mode: Arc<str> = args.mode.as_deref().unwrap_or("operator").into();
            if !cx.state.modes.contains_key(&mode) {
                return Err(CommandError::bad_args(format!("unknown mode {mode}")));
            }
            let pending = PendingOp {
                op,
                count: cx.count(),
                register: cx.register(),
                return_mode: cx.state.view().mode.clone(),
            };
            set_pending(cx.state, Some(pending));
            set_mode(cx, &mode)?;
            Ok(Value::Null)
        },
    )?;
    reg(
        "vim.cancel_operator",
        "Cancel a pending operator.",
        false,
        |cx, _| {
            let id = focus(cx);
            let Some(op) = vim(cx.state).ops.remove(&id) else {
                return Ok(Value::Null);
            };
            set_mode(cx, &op.return_mode)?;
            Ok(Value::Null)
        },
    )?;
    reg(
        "vim.operator_line",
        "Apply the pending operator to [count] whole lines (dd, cc, yy).",
        true,
        |cx, _| {
            let Some(op) = pending(cx.state) else {
                return Ok(Value::Null);
            };
            let n = op.count.unwrap_or(1).max(1) as usize * count(cx);
            let snap = cx.state.snapshot();
            let last = snap.line_count() - 1;
            let ranges = cx
                .state
                .view()
                .selection
                .ranges
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let first = motion::line_of(&snap, r.head);
                    (i, line_span(&snap, first, (first + n - 1).min(last)), true)
                })
                .collect();
            apply_op(cx, &op, ranges)
        },
    )?;
    reg(
        "vim.apply_operator",
        "Apply an operator to the selections (visual mode).",
        true,
        |cx, a| {
            let args: OpArgs = parse_args(a)?;
            let op = PendingOp {
                op: check_op(&args.op)?,
                count: None,
                register: cx.register(),
                return_mode: args.mode.as_deref().unwrap_or("normal").into(),
            };
            let snap = cx.state.snapshot();
            let mode = cx.state.mode().clone();
            let linewise = args.linewise || mode.linewise_selection;
            let ranges = cx
                .state
                .view()
                .selection
                .ranges
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let (a, b) = (r.anchor.min(r.head), r.anchor.max(r.head));
                    if linewise {
                        let (la, lb) = (motion::line_of(&snap, a), motion::line_of(&snap, b));
                        (i, line_span(&snap, la, lb), true)
                    } else {
                        let b = if mode.inclusive_selection {
                            snap.next_grapheme_boundary(b).unwrap_or(b)
                        } else {
                            b
                        };
                        (i, a..b, false)
                    }
                })
                .collect();
            apply_op(cx, &op, ranges)
        },
    )?;
    reg(
        "vim.text_object",
        "Select or operate on a text object (args: char = w W \" ' ( b { B [ < p, inner).",
        true,
        |cx, a| {
            let args: CharArgs = parse_args(a)?;
            let snap = cx.state.snapshot();
            let objects: Vec<_> = cx
                .state
                .view()
                .selection
                .ranges
                .iter()
                .map(|r| objects::text_object(&snap, r.head, args.char, args.inner))
                .collect();
            if let Some(op) = pending(cx.state) {
                let ranges = objects
                    .into_iter()
                    .enumerate()
                    .filter_map(|(i, o)| o.map(|o| (i, o.range, o.linewise)))
                    .collect::<Vec<_>>();
                if ranges.is_empty() {
                    return cancel(cx, &op);
                }
                return apply_op(cx, &op, ranges);
            }
            let inclusive = cx.state.mode().inclusive_selection;
            let id = focus(cx);
            let view = cx
                .state
                .views
                .get_mut(&id)
                .unwrap_or_else(|| unreachable!());
            for (r, o) in view.selection.ranges.iter_mut().zip(objects) {
                if let Some(o) = o.filter(|o| !o.range.is_empty()) {
                    let end = if inclusive {
                        snap.prev_grapheme_boundary(o.range.end)
                            .unwrap_or(o.range.start)
                    } else {
                        o.range.end
                    };
                    *r = SelRange {
                        anchor: o.range.start,
                        head: end,
                    };
                }
            }
            after_selection_change(cx)?;
            Ok(Value::Null)
        },
    )?;
    reg(
        "vim.find_char",
        "Move to the [count]th char on the line (f t F T).",
        true,
        |cx, a| {
            let args: CharArgs = parse_args(a)?;
            vim(cx.state).last_find = Some((args.char, args.till, args.backward));
            let extend = a.get("extend").and_then(Value::as_bool).unwrap_or(false);
            find_motion(cx, args.char, args.till, args.backward, false, extend)
        },
    )?;
    reg(
        "vim.repeat_find",
        "Repeat the last find-char (; and , with reverse).",
        true,
        |cx, a| {
            let args: FlagArgs = parse_args(a)?;
            let Some((c, till, backward)) = vim(cx.state).last_find else {
                return Ok(Value::Null);
            };
            find_motion(cx, c, till, backward != args.reverse, true, args.extend)
        },
    )?;
    reg(
        "vim.match_bracket",
        "Jump to the matching bracket (%).",
        true,
        |cx, a| {
            let args: FlagArgs = parse_args(a)?;
            if let Some(r) = without_target(cx, objects::match_bracket) {
                return r;
            }
            move_ranges(cx, args.extend, false, Kind::Inclusive, |s, h, _| {
                objects::match_bracket(s, h).unwrap_or(h)
            })
        },
    )?;
    reg(
        "vim.replace_char",
        "Replace [count] chars under the cursor with a char (r).",
        true,
        |cx, a| {
            let args: CharArgs = parse_args(a)?;
            let n = count(cx);
            let snap = cx.state.snapshot();
            let hunks = cx
                .state
                .view()
                .selection
                .ranges
                .iter()
                .enumerate()
                .filter_map(|(i, r)| {
                    let end_of_line = motion::line_end(&snap, motion::line_of(&snap, r.head));
                    let mut end = r.head;
                    for _ in 0..n {
                        let next = motion::right(&snap, end, false);
                        if next == end || next > end_of_line {
                            return None;
                        }
                        end = next;
                    }
                    Some((i, r.head..end, args.char.to_string().repeat(n)))
                })
                .collect();
            apply_hunks(cx, &snap, hunks, |start, _, _, t| {
                let end = start + t.len();
                end - t.chars().last().map_or(0, char::len_utf8)
            })
        },
    )?;
    reg(
        "vim.join_lines",
        "Join [count] lines with single spaces (J).",
        true,
        |cx, _| {
            let n = count(cx).max(2) - 1;
            let snap = cx.state.snapshot();
            let last = snap.line_count() - 1;
            let mut hunks = Vec::new();
            for (i, r) in cx.state.view().selection.ranges.iter().enumerate() {
                let line = motion::line_of(&snap, r.head);
                for l in line..(line + n).min(last) {
                    let end = motion::line_end(&snap, l);
                    let next = motion::first_nonblank(&snap, l + 1);
                    let next_text = snap
                        .slice(next..motion::line_end(&snap, l + 1))
                        .unwrap_or_default();
                    let trimmed_end =
                        motion::line_text(&snap, l).trim_end().len() + motion::line_start(&snap, l);
                    let sep = if next_text.is_empty() || next_text.starts_with(')') {
                        ""
                    } else {
                        " "
                    };
                    hunks.push((i, trimmed_end..next, sep.to_owned()));
                    let _ = end;
                }
            }
            apply_hunks(cx, &snap, hunks, |start, _, _, _| start)
        },
    )?;
    reg(
        "vim.open_line",
        "Open a new line below (or above) with the same indent.",
        true,
        |cx, a| {
            let args: FlagArgs = parse_args(a)?;
            let snap = cx.state.snapshot();
            let le = line_ending(&snap);
            let hunks = cx
                .state
                .view()
                .selection
                .ranges
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let line = motion::line_of(&snap, r.head);
                    let text = motion::line_text(&snap, line);
                    let indent: String = text
                        .chars()
                        .take_while(|c| matches!(c, ' ' | '\t'))
                        .collect();
                    if args.above {
                        let at = motion::line_start(&snap, line);
                        (i, at..at, format!("{indent}{le}"))
                    } else {
                        let at = motion::line_end(&snap, line);
                        (i, at..at, format!("{le}{indent}"))
                    }
                })
                .collect();
            let above = args.above;
            let le_len = le.len();
            apply_hunks(cx, &snap, hunks, move |start, _, _, t| {
                if above {
                    start + t.len() - le_len
                } else {
                    start + t.len()
                }
            })
        },
    )?;
    reg(
        "vim.flip_selection",
        "Swap each selection's anchor and cursor (visual o).",
        false,
        |cx, _| {
            let id = focus(cx);
            let view = cx
                .state
                .views
                .get_mut(&id)
                .unwrap_or_else(|| unreachable!());
            for r in &mut view.selection.ranges {
                std::mem::swap(&mut r.anchor, &mut r.head);
            }
            after_selection_change(cx)?;
            Ok(Value::Null)
        },
    )?;
    reg(
        "vim.repeat",
        "Repeat the last change (.), [count] replaces its count.",
        false,
        |cx, _| {
            let mut calls = vim(cx.state).last_change.clone();
            if let (Some(n), Some(first)) = (cx.count(), calls.first_mut()) {
                first.count = Some(n);
            }
            replay(cx, &calls, 1)
        },
    )?;
    reg(
        "vim.macro_record",
        "Start recording a macro into a register, or stop (q).",
        false,
        |cx, a| {
            if let Some((c, calls)) = vim(cx.state).macro_recording.take() {
                vim(cx.state).macros.insert(c, calls);
                return Ok(Value::Null);
            }
            let args: CharArgs = parse_args(a)?;
            vim(cx.state).macro_recording = Some((args.char, Vec::new()));
            Ok(Value::Null)
        },
    )?;
    reg(
        "vim.macro_play",
        "Play a macro [count] times (@x, @@ = last).",
        false,
        |cx, a| {
            let args: CharArgs = parse_args(a)?;
            let c = if args.char == '@' {
                vim(cx.state)
                    .macros
                    .get(&'@')
                    .and_then(|m| m.first())
                    .and_then(|c| c.register)
                    .unwrap_or('@')
            } else {
                args.char
            };
            let Some(calls) = vim(cx.state).macros.get(&c).cloned() else {
                return Err(CommandError::failed(format!("register {c} is empty")));
            };
            // Remember the last played macro for `@@`.
            vim(cx.state).macros.insert(
                '@',
                vec![Call {
                    command: CommandId::new("vim.macro_play")
                        .unwrap_or_else(|e| unreachable!("{e}")),
                    args: Arc::new(Value::Null),
                    count: None,
                    register: Some(c),
                }],
            );
            let n = count(cx);
            replay(cx, &calls, n)
        },
    )?;
    reg(
        "vim.cmdline_open",
        "Open the command line (: for commands, / or ? for search).",
        false,
        |cx, a| {
            let args: FlagArgs = parse_args(a)?;
            let mode: Arc<str> = args.mode.as_deref().unwrap_or("cmdline").into();
            if !cx.state.modes.contains_key(&mode) {
                return Err(CommandError::bad_args(format!("unknown mode {mode}")));
            }
            vim(cx.state).cmdline = Some(Cmdline {
                prefix: args.prefix.unwrap_or(':'),
                text: args.text.unwrap_or_default(),
                return_mode: cx.state.view().mode.clone(),
            });
            let id = focus(cx);
            cx.state
                .views
                .get_mut(&id)
                .unwrap_or_else(|| unreachable!())
                .mode = mode.clone();
            cx.emit(
                ev(events::MODE_CHANGED),
                json!({ "view": id.0, "mode": &*mode }),
            )?;
            Ok(Value::Null)
        },
    )?;
    reg(
        "vim.cmdline_insert",
        "Type into the command line.",
        false,
        |cx, a| {
            let args: FlagArgs = parse_args(a)?;
            if let (Some(c), Some(t)) = (vim(cx.state).cmdline.as_mut(), args.text) {
                c.text.push_str(&t);
            }
            Ok(Value::Null)
        },
    )?;
    reg(
        "vim.cmdline_backspace",
        "Delete the last char of the command line; close it if empty.",
        false,
        |cx, _| {
            match vim(cx.state).cmdline.as_mut() {
                Some(c) if !c.text.is_empty() => {
                    c.text.pop();
                }
                Some(_) => {
                    close_cmdline(cx)?;
                }
                None => {}
            }
            Ok(Value::Null)
        },
    )?;
    reg(
        "vim.cmdline_cancel",
        "Close the command line.",
        false,
        |cx, _| {
            close_cmdline(cx)?;
            Ok(Value::Null)
        },
    )?;
    reg(
        "vim.cmdline_submit",
        "Run the command line.",
        false,
        |cx, _| {
            let Some(c) = close_cmdline(cx)? else {
                return Ok(Value::Null);
            };
            match c.prefix {
                '/' | '?' => {
                    let backward = c.prefix == '?';
                    let pattern = if c.text.is_empty() {
                        vim(cx.state)
                            .last_search
                            .clone()
                            .map(|s| s.0)
                            .unwrap_or_default()
                    } else {
                        c.text
                    };
                    vim(cx.state).last_search = Some((pattern.clone(), backward));
                    search(cx, &pattern, backward, false)
                }
                _ => ex(cx, &c.text),
            }
        },
    )?;
    reg(
        "vim.search_next",
        "Jump to the next match of the last search (n; N with reverse).",
        false,
        |cx, a| {
            let args: FlagArgs = parse_args(a)?;
            let Some((pattern, backward)) = vim(cx.state).last_search.clone() else {
                return Err(CommandError::failed("no previous search"));
            };
            search(cx, &pattern, backward != args.reverse, args.extend)
        },
    )?;
    reg(
        "vim.ex",
        "Run an ex-style command line (w, q, wq, e path, 42, or a command id).",
        false,
        |cx, a| {
            let args: FlagArgs = parse_args(a)?;
            ex(cx, args.text.as_deref().unwrap_or_default())
        },
    )?;
    let _ = span;
    Ok(())
}

pub(crate) fn cancel(cx: C<'_, '_>, op: &PendingOp) -> Result<Value, CommandError> {
    set_pending(cx.state, None);
    set_mode(cx, &op.return_mode)?;
    Ok(Value::Null)
}
