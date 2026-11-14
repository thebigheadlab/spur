//! The `ui.*` commands: per-client UI state changes, run by the server itself (a
//! core command handler cannot reach a client's UI state). `State::run_as` sends
//! them here before any of its own bookkeeping.

use serde_json::{Value, json};
use spur_core::{Actor, CommandError, CommandId, Invocation, Scoped};
use spur_rpc::{ItemKind, ServerMsg};

use super::pack::StateValue;
use super::state::shown;
use crate::session::State;

/// Every `ui.*` command id.
pub const UI_COMMANDS: &[&str] = &[
    "ui.set",
    "ui.toggle",
    "ui.cycle",
    "ui.focus",
    "ui.list.move",
    "ui.list.page",
    "ui.list.activate",
    "ui.tree.expand",
    "ui.tree.collapse",
    "ui.quit",
];

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

impl State {
    /// Runs a `ui.*` command as `conn`.
    pub(crate) fn ui_command(
        &mut self,
        conn: u64,
        id: &str,
        args: &Value,
    ) -> Result<Value, CommandError> {
        if !UI_COMMANDS.contains(&id) {
            return Err(CommandError::unknown(id));
        }
        let has_ui = self.clients.get(&conn).is_some_and(|c| c.ui.is_some());
        if self.ux.is_none() || !has_ui {
            return Err(CommandError::failed("this client has no UI state"));
        }
        match id {
            "ui.set" | "ui.toggle" | "ui.cycle" => self.ui_set(conn, id, args)?,
            "ui.focus" => {
                let region = arg_str(args, "region")?;
                let known = self
                    .ux
                    .as_ref()
                    .is_some_and(|u| u.pack.regions.contains_key(region));
                if !known {
                    return Err(CommandError::bad_args(format!("no region {region:?}")));
                }
                self.set_focus(conn, region);
            }
            "ui.list.move" | "ui.list.page" => {
                let by = arg_int(args, "by")?;
                self.list_move(conn, by, id == "ui.list.page");
            }
            "ui.list.activate" => self.list_activate(conn, None),
            "ui.tree.expand" | "ui.tree.collapse" => {
                let expand = id == "ui.tree.expand";
                let (Some(ux), Some(c)) = (self.ux.as_mut(), self.clients.get_mut(&conn)) else {
                    return Ok(Value::Null);
                };
                if let Some(list) = focused_list(ux, c)
                    && let Some(t) = c.ui.as_mut().and_then(|u| u.trees.get_mut(&list))
                {
                    if expand {
                        t.expand(&mut ux.files);
                    } else {
                        t.collapse();
                    }
                }
            }
            "ui.quit" => self.quit(conn),
            _ => return Err(CommandError::unknown(id)),
        }
        if let Some(c) = self.clients.get_mut(&conn) {
            c.dirty = true;
        }
        self.check_focus(conn);
        Ok(Value::Null)
    }

    fn ui_set(&mut self, conn: u64, id: &str, args: &Value) -> Result<(), CommandError> {
        let (Some(ux), Some(c)) = (self.ux.as_ref(), self.clients.get_mut(&conn)) else {
            return Ok(());
        };
        let Some(ui) = c.ui.as_mut() else {
            return Ok(());
        };
        let key = arg_str(args, "key")?;
        let Some(current) = ui.values.get(key) else {
            return Err(CommandError::bad_args(format!(
                "{key} is not a state key of this pack"
            )));
        };
        let allowed = ux.pack.state_values.get(key);
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
                let i = (i64::try_from(at).unwrap_or(0) + by).rem_euclid(n);
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
        ui.values.insert(key.to_owned(), new);
        Ok(())
    }

    /// Moves focus to `region` (the focus check then runs as after any command).
    pub(crate) fn set_focus(&mut self, conn: u64, region: &str) {
        if let Some(c) = self.clients.get_mut(&conn)
            && let Some(ui) = c.ui.as_mut()
            && ui.focus != region
        {
            region.clone_into(&mut ui.focus);
            c.keymap.reset();
            c.dirty = true;
        }
    }

    /// Refreshes the client's scope and moves focus to the editor if its region is
    /// not shown.
    pub(crate) fn check_focus(&mut self, conn: u64) {
        self.refresh_client(conn);
        let (Some(ux), Some(c)) = (self.ux.as_ref(), self.clients.get_mut(&conn)) else {
            return;
        };
        let Some(ui) = c.ui.as_mut() else {
            return;
        };
        let ok =
            ux.pack.regions.get(&ui.focus).is_some_and(|r| {
                shown(&ux.pack, &r.node, &Scoped(&[self.core.context(), &c.scope]))
            });
        if !ok && ui.focus != "editor" {
            ui.focus = "editor".into();
            c.keymap.reset();
            c.dirty = true;
            self.refresh_client(conn);
        }
    }

    fn list_move(&mut self, conn: u64, by: i64, page: bool) {
        let (Some(ux), Some(c)) = (self.ux.as_mut(), self.clients.get_mut(&conn)) else {
            return;
        };
        let Some(list) = focused_list(ux, c) else {
            return;
        };
        let rows = c.rows_hint();
        let Some(ui) = c.ui.as_mut() else { return };
        let height = ui.list_rows.get(&list).copied().unwrap_or(rows);
        if let Some(t) = ui.trees.get_mut(&list) {
            let step = if page {
                by * i64::from(height.max(1))
            } else {
                by
            };
            t.move_by(&mut ux.files, step, height);
        }
    }

    /// Activates the selected item of the focused list, or of `list` (a click).
    pub(crate) fn list_activate(&mut self, conn: u64, list: Option<String>) {
        let (Some(ux), Some(c)) = (self.ux.as_mut(), self.clients.get_mut(&conn)) else {
            return;
        };
        let Some(list) = list.or_else(|| focused_list(ux, c)) else {
            return;
        };
        let Some(t) = c.ui.as_mut().and_then(|u| u.trees.get_mut(&list)) else {
            return;
        };
        c.dirty = true;
        t.rows(&mut ux.files);
        let Some(row) = t.selected_row().cloned() else {
            return;
        };
        match row.kind {
            ItemKind::Dir if row.open => t.collapse(),
            ItemKind::Dir => {
                t.expand(&mut ux.files);
            }
            ItemKind::Entry if row.openable => {
                let path = ux.files.root().join(&row.id);
                let inv = Invocation::new(
                    CommandId::new("editor.open").unwrap_or_else(|e| unreachable!("{e}")),
                    Actor::User,
                )
                .args(json!({ "path": path }));
                match self.run_as(conn, inv) {
                    Some(Ok(_)) => self.set_focus(conn, "editor"),
                    Some(Err(e)) => self.set_status(conn, "editor.open", &e.message),
                    None => {}
                }
            }
            _ => {}
        }
    }

    /// Shows a failed command in the client's status notice.
    pub(crate) fn set_status(&mut self, conn: u64, command: &str, reason: &str) {
        let Some(ux) = self.ux.as_ref() else {
            return;
        };
        let text = ux.pack.message(
            "command.failed",
            &[("command", command), ("reason", reason)],
        );
        if let Some(c) = self.clients.get_mut(&conn)
            && let Some(ui) = c.ui.as_mut()
        {
            ui.status = Some(text);
            c.dirty = true;
        }
    }

    fn quit(&mut self, conn: u64) {
        let Some(ux) = self.ux.as_ref() else {
            return;
        };
        let mut modified: Vec<_> = self
            .core
            .state
            .docs
            .iter()
            .filter(|(_, d)| d.modified())
            .map(|(id, d)| (*id, d.path.clone()))
            .collect();
        modified.sort_by_key(|(id, _)| *id);
        let armed = self
            .clients
            .get(&conn)
            .and_then(|c| c.ui.as_ref())
            .is_some_and(|u| u.quit_armed);
        if modified.is_empty() || armed {
            if let Some(c) = self.clients.get_mut(&conn) {
                c.batch.push(ServerMsg::Bye {
                    reason: "quit".into(),
                });
            }
            self.drop_client(conn);
            return;
        }
        let name = modified[0]
            .1
            .as_deref()
            .and_then(|p| p.file_name())
            .map_or_else(
                || ux.pack.message("doc.untitled", &[]),
                |n| n.to_string_lossy().into_owned(),
            );
        let text = ux.pack.message("quit.unsaved", &[("doc", &name)]);
        if let Some(c) = self.clients.get_mut(&conn)
            && let Some(ui) = c.ui.as_mut()
        {
            ui.prompt = Some(text);
        }
    }
}

/// The first shown list node inside the focused region.
fn focused_list(ux: &super::Ux, c: &crate::client::Client) -> Option<String> {
    let ui = c.ui.as_ref()?;
    let region = ux.pack.regions.get(&ui.focus)?;
    ui.shown_lists
        .iter()
        .find(|l| ux.pack.within(l, &region.node))
        .cloned()
}

/// True if `inv` is a `ui.*` command; its id.
pub(crate) fn ui_id(inv: &Invocation) -> Option<&CommandId> {
    match &inv.command {
        spur_core::Target::Id(id) if id.namespace() == "ui" => Some(id),
        _ => None,
    }
}
