//! The `ui.*` commands: per-client UI state changes. They are core commands (in
//! `Core::commands()`, audited, run through middleware), but their work needs the
//! calling client's UI state, which the core cannot see. `State::run_as` passes a
//! [`UiHost`] for the client into the call; the shared handler takes it and runs
//! the command here. Work that changes the session itself (checking focus, dropping
//! a client) is returned as [`Effect`]s, applied after the call.

use serde_json::{Value, json};
use spur_core::editor::Editor;
use spur_core::{
    ClientHost, CommandError, CommandId, CommandMeta, Context, Core, Cx, EventName, RegisterError,
    RemoteSide, Scoped, events,
};
use spur_rpc::ServerMsg;

use super::client_ui::{close_palette, open_palette, palette_rows, query_changed, set_scope};
use super::field::Step as FieldStep;
use super::files::Activate;
use super::pack::{PALETTE_SCOPES, PALETTE_SOURCE, StateValue};
use super::palette::Run;
use super::state::{first_list, focused_input};
use super::{EDITOR_REGION, Ux};
use crate::client::Client;

/// Every `ui.*` command id, with its description.
pub const UI_COMMANDS: &[(&str, &str)] = &[
    ("ui.set", "Set a UI state key of the pack."),
    ("ui.toggle", "Flip a boolean UI state key."),
    (
        "ui.cycle",
        "Step a UI state key through its allowed values.",
    ),
    ("ui.focus", "Move keyboard focus to a region."),
    ("ui.list.move", "Move the selection of the focused list."),
    (
        "ui.list.page",
        "Move the selection of the focused list by pages.",
    ),
    ("ui.list.activate", "Activate the selected item of a list."),
    (
        "ui.tree.expand",
        "Expand the selected folder of the focused tree.",
    ),
    (
        "ui.tree.collapse",
        "Collapse the selected folder of the focused tree.",
    ),
    ("ui.quit", "Quit, asking first if a document is modified."),
    (
        "ui.palette.open",
        "Open the palette in a scope (all, files, recent, actions, line), or switch scope.",
    ),
    ("ui.palette.close", "Close the palette."),
    ("ui.input.insert", "Type text into the focused input."),
    (
        "ui.input.delete_backward",
        "Delete the selection or the char before the cursor in the focused input.",
    ),
    (
        "ui.input.delete_forward",
        "Delete the selection or the char after the cursor in the focused input.",
    ),
    (
        "ui.input.move",
        "Move the cursor of the focused input (by char, word, start or end).",
    ),
    ("ui.input.clear", "Empty the focused input."),
    ("ui.input.focus", "Focus an input of the focused region."),
];

/// Session work a `ui.*` command asks for; applied by `run_as` after the call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    /// Refresh the client's scope and move focus to the editor if its region
    /// is hidden.
    CheckFocus,
    /// Drop the client (after a confirmed quit).
    Quit,
    /// Run a pack action as a key press in the focused region (a palette row).
    RunAction(String),
}

/// A client's UI side of one call.
pub(crate) struct UiHost<'a> {
    pub client: &'a mut Client,
    pub ux: &'a mut Ux,
    pub effects: Vec<Effect>,
}

fn cmd(id: &str) -> CommandId {
    CommandId::new(id).unwrap_or_else(|e| unreachable!("{e}"))
}

/// Registers every `ui.*` command with the core.
///
/// # Errors
///
/// A duplicate id.
pub fn register_ui_commands(core: &mut Core<Editor>) -> Result<(), RegisterError> {
    for (id, description) in UI_COMMANDS {
        let mut meta = CommandMeta::new(cmd(id), *description);
        meta.remote_side = RemoteSide::Ui;
        let command = cmd(id);
        core.register_fn(meta, move |cx, args| {
            let Some(host) = cx.take_host() else {
                return Err(CommandError::failed("this client has no UI state"));
            };
            let res = host.call(cx, &command, args);
            cx.put_host(host);
            res
        })?;
    }
    Ok(())
}

fn arg<'a>(args: &'a Value, name: &str) -> Result<&'a Value, CommandError> {
    args.get(name)
        .ok_or_else(|| CommandError::bad_args(format!("missing {name}")))
}

fn arg_str<'a>(args: &'a Value, name: &str) -> Result<&'a str, CommandError> {
    arg(args, name)?
        .as_str()
        .ok_or_else(|| CommandError::bad_args(format!("{name} must be a string")))
}

fn arg_int(args: &Value, name: &str) -> Result<i64, CommandError> {
    arg(args, name)?
        .as_i64()
        .ok_or_else(|| CommandError::bad_args(format!("{name} must be an integer")))
}

fn emit(cx: &mut Cx<'_, Editor>, name: &str, payload: Value) {
    if let Ok(n) = EventName::new(name) {
        let _ = cx.emit(n, payload);
    }
}

impl ClientHost<Editor> for UiHost<'_> {
    #[allow(clippy::too_many_lines)]
    fn call(
        &mut self,
        cx: &mut Cx<'_, Editor>,
        id: &CommandId,
        args: &Value,
    ) -> Result<Value, CommandError> {
        if self.client.ui.is_none() {
            return Err(CommandError::failed("this client has no UI state"));
        }
        match id.as_str() {
            "ui.set" | "ui.toggle" | "ui.cycle" => {
                let (key, value) = self.ui_set(id.as_str(), args)?;
                emit(
                    cx,
                    events::UI_CHANGED,
                    json!({ "client": self.client.id.0, "key": key, "value": value }),
                );
            }
            "ui.focus" => {
                let region = arg_str(args, "region")?;
                if !self.ux.pack.regions.contains_key(region) {
                    return Err(CommandError::bad_args(format!("no region {region:?}")));
                }
                if set_focus(self.client, region) {
                    emit(
                        cx,
                        events::UI_FOCUSED,
                        json!({ "client": self.client.id.0, "region": region }),
                    );
                }
            }
            "ui.list.move" | "ui.list.page" => {
                let by = arg_int(args, "by")?;
                self.list_move(cx, by, id.as_str() == "ui.list.page");
            }
            "ui.list.activate" => {
                let list = match args.get("list") {
                    None | Some(Value::Null) => None,
                    Some(_) => Some(arg_str(args, "list")?.to_owned()),
                };
                self.list_activate(cx, list);
            }
            "ui.tree.expand" | "ui.tree.collapse" => {
                let expand = id.as_str() == "ui.tree.expand";
                let (ux, c) = (&mut *self.ux, &mut *self.client);
                if let Some(list) = focused_list(ux, c, cx.context)
                    && let Some(t) = c.ui.as_mut().and_then(|u| u.trees.get_mut(&list))
                {
                    if expand {
                        t.expand(&mut ux.files);
                    } else {
                        t.collapse();
                    }
                }
            }
            "ui.quit" => self.quit(cx.state),
            "ui.palette.open" => {
                let scope = arg_str(args, "scope")?;
                if !PALETTE_SCOPES.contains(&scope) {
                    return Err(CommandError::bad_args(format!(
                        "unknown scope {scope:?} ({})",
                        PALETTE_SCOPES.join(", ")
                    )));
                }
                let query = match args.get("query") {
                    None | Some(Value::Null) => None,
                    Some(_) => Some(arg_str(args, "query")?),
                };
                open_palette(self.ux, self.client, scope, query).map_err(CommandError::failed)?;
            }
            "ui.palette.close" => close_palette(self.ux, self.client, cx.context),
            "ui.input.insert" => {
                let text = arg_str(args, "text")?;
                self.input_edit(cx.context, |f| f.insert(text), Some(text), false);
            }
            "ui.input.delete_backward" => {
                self.input_edit(cx.context, super::field::Field::delete_backward, None, true);
            }
            "ui.input.delete_forward" => {
                self.input_edit(cx.context, super::field::Field::delete_forward, None, false);
            }
            "ui.input.clear" => {
                self.input_edit(cx.context, super::field::Field::clear, None, false);
            }
            "ui.input.move" => {
                let step = match arg_str(args, "by")? {
                    "char" => FieldStep::Char,
                    "word" => FieldStep::Word,
                    "start" => FieldStep::Start,
                    "end" => FieldStep::End,
                    other => {
                        return Err(CommandError::bad_args(format!(
                            "by: {other:?} is not char, word, start or end"
                        )));
                    }
                };
                let back = args.get("back").and_then(Value::as_bool).unwrap_or(false);
                let extend = args.get("extend").and_then(Value::as_bool).unwrap_or(false);
                self.input_edit(cx.context, |f| f.move_by(step, !back, extend), None, false);
            }
            "ui.input.focus" => {
                let node = arg_str(args, "node")?;
                let ok = self
                    .ux
                    .pack
                    .nodes
                    .get(node)
                    .is_some_and(|n| n.kind == spur_rpc::NodeKind::Input)
                    && self.client.ui.as_ref().is_some_and(|u| {
                        self.ux
                            .pack
                            .regions
                            .get(&u.focus)
                            .is_some_and(|r| self.ux.pack.within(node, &r.node))
                    });
                if !ok {
                    return Err(CommandError::bad_args(format!(
                        "{node:?} is not an input of the focused region"
                    )));
                }
                if let Some(ui) = self.client.ui.as_mut() {
                    ui.input = Some(node.to_owned());
                    self.client.keymap.reset();
                }
            }
            other => return Err(CommandError::unknown(other)),
        }
        self.client.dirty = true;
        self.effects.push(Effect::CheckFocus);
        Ok(Value::Null)
    }
}

impl UiHost<'_> {
    /// Sets a state key; returns the key and its new value.
    fn ui_set(&mut self, id: &str, args: &Value) -> Result<(String, Value), CommandError> {
        let Some(ui) = self.client.ui.as_mut() else {
            return Err(CommandError::failed("this client has no UI state"));
        };
        let key = arg_str(args, "key")?;
        let Some(current) = ui.values.get(key) else {
            return Err(CommandError::bad_args(format!(
                "{key} is not a state key of this pack"
            )));
        };
        let allowed = self.ux.pack.state_values.get(key);
        let new = match id {
            "ui.toggle" => match current {
                StateValue::Bool(b) => StateValue::Bool(!b),
                _ => return Err(CommandError::bad_args(format!("{key} is not a boolean"))),
            },
            "ui.cycle" => {
                let (Some(list), StateValue::Str(s)) = (allowed, current) else {
                    return Err(CommandError::bad_args(format!(
                        "{key} has no list of values"
                    )));
                };
                let by = arg_int(args, "by")?;
                let at = list.iter().position(|v| v == s).unwrap_or(0);
                let n = i64::try_from(list.len()).unwrap_or(1);
                let i = (i64::try_from(at).unwrap_or(0) + by.rem_euclid(n)).rem_euclid(n);
                StateValue::Str(list[usize::try_from(i).unwrap_or(0)].clone())
            }
            _ => {
                let v = StateValue::from_json(arg(args, "value")?)
                    .filter(|v| v.same_type(current))
                    .ok_or_else(|| CommandError::bad_args(format!("wrong type for {key}")))?;
                if let (StateValue::Str(s), Some(list)) = (&v, allowed)
                    && !list.contains(s)
                {
                    return Err(CommandError::bad_args(format!(
                        "{s:?} is not one of {}",
                        list.join(", ")
                    )));
                }
                v
            }
        };
        let value = match &new {
            StateValue::Bool(b) => json!(b),
            StateValue::Int(n) => json!(n),
            StateValue::Str(s) => json!(s),
        };
        ui.values.insert(key.to_owned(), new);
        Ok((key.to_owned(), value))
    }

    /// Edits the focused input. In the palette's empty input, a scope prefix
    /// typed switches scope instead, and backspace (`back`) returns to the scope it
    /// opened with.
    fn input_edit(
        &mut self,
        global: &Context,
        f: impl FnOnce(&mut super::field::Field),
        typed: Option<&str>,
        back: bool,
    ) {
        let (ux, c) = (&mut *self.ux, &mut *self.client);
        let Some(ui) = c.ui.as_ref() else { return };
        let Some(input) = focused_input(&ux.pack, ui, &Scoped(&[global, &c.scope])) else {
            return;
        };
        let palette = ux.pack.palette.as_ref().is_some_and(|p| p.input == input);
        let Some(ui) = c.ui.as_mut() else { return };
        let before = ui.fields.get(&input).cloned().unwrap_or_default();
        if palette && before.text.is_empty() {
            let prefix = typed.and_then(|t| ux.pack.prefixes.iter().find(|(p, _)| p == t));
            let scope = match prefix {
                Some((_, scope)) => Some(scope.clone()),
                None if back && ui.palette.scope != ui.palette.open_scope => {
                    Some(ui.palette.open_scope.clone())
                }
                None => None,
            };
            if let Some(scope) = scope {
                set_scope(&mut ui.palette, &scope);
                c.dirty = true;
                query_changed(ux, c);
                return;
            }
        }
        let mut after = before.clone();
        f(&mut after);
        let changed = after.text != before.text;
        ui.fields.insert(input, after);
        c.dirty = true;
        if palette && changed {
            query_changed(ux, c);
        }
    }

    fn list_move(&mut self, cx: &Cx<'_, Editor>, by: i64, page: bool) {
        let global: &Context = cx.context;
        let (ux, c) = (&mut *self.ux, &mut *self.client);
        let Some(list) = focused_list(ux, c, global) else {
            return;
        };
        let rows = c.rows_hint();
        if is_palette(ux, &list) {
            let Some(r) = palette_rows(ux, c, cx.state, global) else {
                return;
            };
            let Some(ui) = c.ui.as_mut() else { return };
            let height = ui.list_rows.get(&list).copied().unwrap_or(rows);
            let step = if page {
                by.saturating_mul(i64::from(height.max(1)))
            } else {
                by
            };
            let m = ux.matchers.get(&c.id.0);
            r.settle(&mut ui.palette, m, ux.index.root());
            r.move_by(&mut ui.palette, step, height, m, ux.index.root());
            return;
        }
        let Some(ui) = c.ui.as_mut() else { return };
        let height = ui.list_rows.get(&list).copied().unwrap_or(rows);
        if let Some(t) = ui.trees.get_mut(&list) {
            let step = if page {
                by.saturating_mul(i64::from(height.max(1)))
            } else {
                by
            };
            t.move_by(&mut ux.files, step, height);
        }
    }

    /// Activates the selected item of the focused list, or of `list` (a click).
    /// Opening a file that fails shows the status notice; the call still succeeds.
    fn list_activate(&mut self, cx: &mut Cx<'_, Editor>, list: Option<String>) {
        let (ux, c) = (&mut *self.ux, &mut *self.client);
        let Some(list) = list.or_else(|| focused_list(ux, c, cx.context)) else {
            return;
        };
        if is_palette(ux, &list) {
            self.palette_run(cx);
            return;
        }
        let Some(t) = c.ui.as_mut().and_then(|u| u.trees.get_mut(&list)) else {
            return;
        };
        if let Activate::Open(path) = t.activate(&mut ux.files) {
            match cx.execute(&cmd("editor.open"), &json!({ "path": path })) {
                Ok(_) => {
                    set_focus(c, EDITOR_REGION);
                }
                Err(e) => set_status(ux, c, "editor.open", &e.message),
            }
        }
    }

    /// Runs the selected palette row: closes the palette, then opens, moves or
    /// runs the action (after the call, as a key press in the restored region).
    fn palette_run(&mut self, cx: &mut Cx<'_, Editor>) {
        let (ux, c) = (&mut *self.ux, &mut *self.client);
        let Some(rows) = palette_rows(ux, c, cx.state, cx.context) else {
            return;
        };
        let Some(ui) = c.ui.as_mut() else { return };
        let m = ux.matchers.get(&c.id.0);
        rows.settle(&mut ui.palette, m, ux.index.root());
        let Some(run) = rows.run(&ui.palette, m, ux.index.root()) else {
            return;
        };
        close_palette(ux, c, cx.context);
        match run {
            Run::Open(path) => {
                set_focus(c, EDITOR_REGION);
                if let Err(e) = cx.execute(&cmd("editor.open"), &json!({ "path": path })) {
                    set_status(ux, c, "editor.open", &e.message);
                }
            }
            Run::Line(line, col) => {
                set_focus(c, EDITOR_REGION);
                let args = match col {
                    Some(col) => json!({ "line": line, "col": col }),
                    None => json!({ "line": line }),
                };
                if let Err(e) = cx.execute(&cmd("view.move_to"), &args) {
                    set_status(ux, c, "view.move_to", &e.message);
                }
            }
            Run::Action(id) => self.effects.push(Effect::RunAction(id)),
        }
    }

    fn quit(&mut self, ed: &Editor) {
        let mut modified: Vec<_> = ed
            .docs
            .iter()
            .filter(|(_, d)| d.modified())
            .map(|(id, d)| (*id, d.path.clone()))
            .collect();
        modified.sort_by_key(|(id, _)| *id);
        let armed = self.client.ui.as_ref().is_some_and(|u| u.quit_armed);
        if modified.is_empty() || armed {
            self.client.batch.push(ServerMsg::Bye {
                reason: "quit".into(),
            });
            self.effects.push(Effect::Quit);
            return;
        }
        let name = modified[0]
            .1
            .as_deref()
            .and_then(|p| p.file_name())
            .map_or_else(
                || self.ux.pack.message("doc.untitled", &[]),
                |n| n.to_string_lossy().into_owned(),
            );
        let text = self.ux.pack.message("quit.unsaved", &[("doc", &name)]);
        if let Some(ui) = self.client.ui.as_mut() {
            ui.prompt = Some(text);
        }
    }
}

/// Moves the client's focus to `region`. True if it changed.
pub(crate) fn set_focus(c: &mut Client, region: &str) -> bool {
    if let Some(ui) = c.ui.as_mut()
        && ui.focus != region
    {
        region.clone_into(&mut ui.focus);
        c.keymap.reset();
        c.dirty = true;
        return true;
    }
    false
}

/// Shows a failed command in the client's status notice.
pub(crate) fn set_status(ux: &Ux, c: &mut Client, command: &str, reason: &str) {
    let text = ux.pack.message(
        "command.failed",
        &[("command", command), ("reason", reason)],
    );
    if let Some(ui) = c.ui.as_mut() {
        ui.status = Some(text);
        c.dirty = true;
    }
}

/// True if list node `list` shows the palette.
pub(crate) fn is_palette(ux: &Ux, list: &str) -> bool {
    ux.pack
        .nodes
        .get(list)
        .is_some_and(|n| n.source.as_deref() == Some(PALETTE_SOURCE))
}

/// The first shown list node inside the focused region, under the client's
/// current scope (keys handled before the next render see the new state).
fn focused_list(ux: &Ux, c: &Client, global: &Context) -> Option<String> {
    let ui = c.ui.as_ref()?;
    let region = ux.pack.regions.get(&ui.focus)?;
    first_list(&ux.pack, &region.node, &Scoped(&[global, &c.scope]))
}
