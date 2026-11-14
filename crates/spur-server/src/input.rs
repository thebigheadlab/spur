//! Input from clients: keys, paste, mouse and size.

use std::sync::Arc;
use std::time::Instant;

use serde_json::json;
use spur_core::{Actor, CommandId, Invocation, Scoped, Target};
use spur_keymap::{Dispatch, parse_keys};
use spur_rpc::{MouseAction, NodeKind, ServerMsg, Size, SurfaceSize};

use crate::client::MAX_ROWS;
use crate::session::{SCROLL_MARGIN, State, has_node};
use crate::ux::EDITOR_REGION;

/// The command id of an invocation, for notices.
fn command_name(inv: &Invocation) -> String {
    match &inv.command {
        Target::Id(id) => id.to_string(),
        Target::Handle(_) => "command".into(),
    }
}

impl State {
    pub(crate) fn key(&mut self, conn: u64, notation: &str) {
        let keys = match parse_keys(notation) {
            Ok(k) => k,
            Err(e) => {
                self.error(conn, format!("bad key {notation:?}: {e}"));
                return;
            }
        };
        let now = Instant::now();
        for key in keys {
            self.input_arrived(conn);
            self.check_focus(conn);
            let Some(c) = self.clients.get_mut(&conn) else {
                return;
            };
            // The focused region's mode, or the view's own for the editor.
            let region_mode = c.ui.as_ref().and_then(|ui| {
                let r = self.ux.as_ref()?.pack.regions.get(&ui.focus)?;
                r.mode.as_deref().map(Arc::<str>::from)
            });
            let mode = region_mode.unwrap_or_else(|| {
                self.core
                    .state
                    .views
                    .get(&c.active)
                    .map_or_else(|| self.core.state.default_mode.clone(), |v| v.mode.clone())
            });
            let lookup = Scoped(&[self.core.context(), &c.scope]);
            let feed = c.keymap.feed(key, &mode, &lookup, now);
            for d in feed.run {
                self.dispatch(conn, &d);
            }
            self.input_done(conn);
        }
    }

    /// The quit prompt counts as confirmed only during the input that saw it.
    fn input_done(&mut self, conn: u64) {
        if let Some(ui) = self.clients.get_mut(&conn).and_then(|c| c.ui.as_mut()) {
            ui.quit_armed = false;
        }
    }

    /// Clears the client's prompt and status notices; the quit prompt counts as
    /// confirmed for this input (a second quit key).
    fn input_arrived(&mut self, conn: u64) {
        if let Some(c) = self.clients.get_mut(&conn)
            && let Some(ui) = c.ui.as_mut()
        {
            ui.quit_armed = ui.prompt.is_some();
            if ui.prompt.take().is_some() | ui.status.take().is_some() {
                c.dirty = true;
            }
        }
    }

    pub(crate) fn dispatch(&mut self, conn: u64, d: &Dispatch) {
        let Some(c) = self.clients.get(&conn) else {
            return;
        };
        for inv in d.invocations(&c.actor) {
            let name = command_name(&inv);
            match self.run_as(conn, inv) {
                None => return,
                // Later steps assumed this one worked.
                Some(Err(e)) => {
                    self.set_status(conn, &name, &e.message);
                    return;
                }
                Some(Ok(_)) => {}
            }
        }
    }

    /// Inserts `text` as one undo step of its own, never read as keys. Ignored while
    /// focus is not on the editor, so it never edits a document out of sight.
    pub(crate) fn paste(&mut self, conn: u64, text: &str) {
        self.input_arrived(conn);
        let elsewhere = self
            .clients
            .get(&conn)
            .and_then(|c| c.ui.as_ref())
            .is_some_and(|u| u.focus != EDITOR_REGION);
        if elsewhere {
            self.input_done(conn);
            return;
        }
        self.end_step(conn);
        let id = CommandId::new("editor.insert_text").unwrap_or_else(|e| unreachable!("{e}"));
        // `run_as` replaces the actor with the client's.
        let inv = Invocation::new(id, Actor::User).args(json!({ "text": text }));
        if let Some(Err(e)) = self.run_as(conn, inv) {
            self.input_failed(conn, "editor.insert_text", e.message);
        }
        self.end_step(conn);
        self.input_done(conn);
    }

    /// A step the person triggered failed: a status notice where the client has UI
    /// state, else an `error` message.
    fn input_failed(&mut self, conn: u64, command: &str, message: String) {
        let has_ui = self.clients.get(&conn).is_some_and(|c| c.ui.is_some());
        if self.ux.is_some() && has_ui {
            self.set_status(conn, command, &message);
        } else {
            self.error(conn, message);
        }
    }

    /// Ends the undo step of `conn`'s active view, so the next edit starts a new one.
    fn end_step(&mut self, conn: u64) {
        if let Some(c) = self.clients.get(&conn) {
            self.core.state.end_undo_step(c.active);
        }
    }

    /// A press or wheel on `node`; `at` is (line, col, lines).
    pub(crate) fn mouse(
        &mut self,
        conn: u64,
        node: &str,
        action: MouseAction,
        at: (u32, u32, i32),
        item: Option<&str>,
    ) {
        let (line, col, lines) = at;
        // Only what the client was last shown can be pressed.
        let in_tree = self.clients.get(&conn).is_none_or(|c| {
            c.ui.as_ref()
                .is_none_or(|u| u.sent_ui.as_ref().is_some_and(|(n, _)| has_node(n, node)))
        });
        if !in_tree {
            return;
        }
        let surface = node == self.surface_id();
        let (region, kind) = match self.ux.as_ref() {
            Some(ux) => (
                ux.pack.region_of(node).map(str::to_owned),
                ux.pack.nodes.get(node).map(|n| n.kind),
            ),
            None => (None, None),
        };
        match action {
            MouseAction::Press => {
                self.input_arrived(conn);
                if let Some(r) = &region {
                    self.set_focus(conn, r);
                }
                self.check_focus(conn);
                if surface {
                    let id = CommandId::new("view.move_to").unwrap_or_else(|e| unreachable!("{e}"));
                    let inv =
                        Invocation::new(id, Actor::User).args(json!({ "line": line, "col": col }));
                    if let Some(Err(e)) = self.run_as(conn, inv) {
                        self.input_failed(conn, "view.move_to", e.message);
                    }
                } else if kind == Some(NodeKind::List) {
                    if let Some(item) = item
                        && self.list_select(conn, node, item)
                    {
                        let id = CommandId::new("ui.list.activate")
                            .unwrap_or_else(|e| unreachable!("{e}"));
                        let inv = Invocation::new(id, Actor::User).args(json!({ "list": node }));
                        if let Some(Err(e)) = self.run_as(conn, inv) {
                            self.input_failed(conn, "ui.list.activate", e.message);
                        }
                    }
                } else {
                    self.click(conn, node);
                }
            }
            MouseAction::Scroll if surface => {
                let Some(c) = self.clients.get_mut(&conn) else {
                    return;
                };
                self.core.state.scroll_by(c.active, lines as isize);
                c.dirty = true;
            }
            MouseAction::Scroll if kind == Some(NodeKind::List) => {
                let (Some(ux), Some(c)) = (self.ux.as_mut(), self.clients.get_mut(&conn)) else {
                    return;
                };
                let hint = c.rows_hint();
                if let Some(ui) = c.ui.as_mut()
                    && let Some(t) = ui.trees.get_mut(node)
                {
                    let height = ui.list_rows.get(node).copied().unwrap_or(hint);
                    t.rows(&mut ux.files);
                    t.scroll(lines, height);
                    c.dirty = true;
                }
            }
            MouseAction::Scroll | MouseAction::Unknown => {}
        }
        self.input_done(conn);
        self.check_focus(conn);
    }

    /// Selects `item` in list `node`. False if the item is not a visible row.
    fn list_select(&mut self, conn: u64, node: &str, item: &str) -> bool {
        let (Some(ux), Some(c)) = (self.ux.as_mut(), self.clients.get_mut(&conn)) else {
            return false;
        };
        let Some(t) = c.ui.as_mut().and_then(|u| u.trees.get_mut(node)) else {
            return false;
        };
        t.rows(&mut ux.files);
        t.select(item)
    }

    /// Runs the last `on_click` entry of `node` whose `when` holds.
    fn click(&mut self, conn: u64, node: &str) {
        let steps = {
            let (Some(ux), Some(c)) = (self.ux.as_ref(), self.clients.get(&conn)) else {
                return;
            };
            let look = Scoped(&[self.core.context(), &c.scope]);
            let Some(spec) = ux.pack.nodes.get(node) else {
                return;
            };
            let Some(entry) = spec
                .on_click
                .iter()
                .rev()
                .find(|e| e.when.as_ref().is_none_or(|w| w.eval(&look)))
            else {
                return;
            };
            entry.steps.clone()
        };
        for step in steps {
            let name = step.command.to_string();
            let inv = Invocation::new(step.command, Actor::User).args(step.args);
            match self.run_as(conn, inv) {
                None => return,
                Some(Err(e)) => {
                    self.set_status(conn, &name, &e.message);
                    return;
                }
                Some(Ok(_)) => {}
            }
        }
    }

    pub(crate) fn resize(&mut self, conn: u64, size: Size) {
        let Some(c) = self.clients.get_mut(&conn) else {
            return;
        };
        c.size = size;
        let (view, rows) = (c.active, c.rows());
        c.dirty = true;
        self.core.state.scroll_view(view, rows, SCROLL_MARGIN);
        self.check_focus(conn);
    }

    /// Sizes the client's layout gave its surface and list nodes.
    pub(crate) fn layout(&mut self, conn: u64, surfaces: &[SurfaceSize]) {
        let surface = self.surface_id().to_owned();
        let ux = self.ux.as_ref();
        let Some(c) = self.clients.get_mut(&conn) else {
            return;
        };
        if let Some(s) = surfaces.iter().find(|s| s.node == surface) {
            c.surface = Some((s.rows, s.cols));
        }
        if let (Some(ux), Some(ui)) = (ux, c.ui.as_mut()) {
            for s in surfaces {
                let list = ux
                    .pack
                    .nodes
                    .get(&s.node)
                    .is_some_and(|n| n.kind == NodeKind::List);
                if list {
                    ui.list_rows.insert(s.node.clone(), s.rows.min(MAX_ROWS));
                }
            }
        }
        let (view, rows) = (c.active, c.rows());
        c.dirty = true;
        self.core.state.scroll_view(view, rows, SCROLL_MARGIN);
        self.check_focus(conn);
    }

    pub(crate) fn error(&mut self, conn: u64, message: String) {
        if let Some(c) = self.clients.get_mut(&conn) {
            c.batch.push(ServerMsg::Error { message });
        }
    }

    /// The earliest keymap timeout over all clients.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.clients
            .values()
            .filter_map(|c| c.keymap.deadline())
            .min()
    }

    /// Runs bindings whose timeout passed.
    pub(crate) fn tick(&mut self, now: Instant) {
        let due: Vec<(u64, Dispatch)> = self
            .clients
            .iter_mut()
            .filter_map(|(k, c)| c.keymap.tick(now).map(|d| (*k, d)))
            .collect();
        for (conn, d) in due {
            self.dispatch(conn, &d);
        }
    }
}
