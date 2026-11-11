//! Input from clients: keys, paste, mouse, size; and the server's own commands.

use std::time::Instant;

use serde::Deserialize;
use serde_json::{Value, json};
use spur_core::editor::{Editor, motion};
use spur_core::{
    CommandError, CommandId, CommandMeta, Core, EventName, Invocation, RegisterError, Scoped,
    events, parse_args,
};
use spur_keymap::{Dispatch, parse_keys};
use spur_rpc::{MouseAction, ServerMsg, Size, SurfaceSize};
use spur_text::Selection;

use crate::frame::SURFACE;
use crate::session::{SCROLL_MARGIN, State};

#[derive(Deserialize)]
struct MoveTo {
    line: usize,
    col: usize,
}

/// Registers `view.move_to`, which puts the focused view's cursor at a line and char
/// column (a mouse press). It ends the current undo step, like any cursor move.
///
/// # Errors
///
/// If the id is taken.
pub fn register_server_commands(core: &mut Core<Editor>) -> Result<(), RegisterError> {
    let meta = CommandMeta::new(
        CommandId::new("view.move_to").unwrap_or_else(|e| unreachable!("{e}")),
        "Put the cursor at a line and column, dropping other cursors.",
    )
    .args_schema(json!({
        "type": "object",
        "properties": {
            "line": { "type": "integer", "minimum": 0 },
            "col": { "type": "integer", "minimum": 0 }
        },
        "required": ["line", "col"]
    }));
    core.register_fn(meta, |cx, args| {
        let a: MoveTo = parse_args(args)?;
        let id = cx.state.focus;
        let snap = cx.state.snapshot();
        let line = a.line.min(snap.line_count().saturating_sub(1));
        let at = motion::offset_at_col(&snap, line, a.col);
        let depth = cx.depth();
        let view = cx
            .state
            .views
            .get_mut(&id)
            .ok_or_else(|| CommandError::failed("no view"))?;
        view.selection = Selection::cursor(at);
        view.goals = vec![None];
        if depth == 0 {
            view.turn += 1;
        }
        let name =
            EventName::new(events::SELECTION_CHANGED).unwrap_or_else(|e| unreachable!("{e}"));
        cx.emit(name, json!({ "view": id.0 }))?;
        Ok(Value::Null)
    })?;
    Ok(())
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
            let Some(c) = self.clients.get_mut(&conn) else {
                return;
            };
            c.refresh_scope(&self.core.state);
            let mode = self
                .core
                .state
                .views
                .get(&c.active)
                .map_or_else(|| self.core.state.default_mode.clone(), |v| v.mode.clone());
            let lookup = Scoped(&[self.core.context(), &c.scope]);
            let feed = c.keymap.feed(key, &mode, &lookup, now);
            for d in feed.run {
                self.dispatch(conn, &d);
            }
        }
    }

    pub(crate) fn dispatch(&mut self, conn: u64, d: &Dispatch) {
        let Some(c) = self.clients.get(&conn) else {
            return;
        };
        for inv in d.invocations(&c.actor) {
            if self.run_as(conn, inv).is_none() {
                return;
            }
        }
    }

    /// Inserts `text` as one undo step of its own, never read as keys.
    pub(crate) fn paste(&mut self, conn: u64, text: &str) {
        self.end_step(conn);
        let id = CommandId::new("editor.insert_text").unwrap_or_else(|e| unreachable!("{e}"));
        let inv = Invocation::new(id, spur_core::Actor::User).args(json!({ "text": text }));
        if let Some(Err(e)) = self.run_as(conn, inv) {
            self.error(conn, e.message);
        }
        self.end_step(conn);
    }

    /// Ends the undo step of `conn`'s active view, so the next edit starts a new one.
    fn end_step(&mut self, conn: u64) {
        let Some(c) = self.clients.get(&conn) else {
            return;
        };
        if let Some(v) = self.core.state.views.get_mut(&c.active) {
            v.turn += 1;
        }
    }

    pub(crate) fn mouse(
        &mut self,
        conn: u64,
        node: &str,
        action: MouseAction,
        line: u32,
        col: u32,
        lines: i32,
    ) {
        if node != SURFACE {
            return;
        }
        match action {
            MouseAction::Press => {
                let id = CommandId::new("view.move_to").unwrap_or_else(|e| unreachable!("{e}"));
                let inv = Invocation::new(id, spur_core::Actor::User)
                    .args(json!({ "line": line, "col": col }));
                self.run_as(conn, inv);
            }
            MouseAction::Scroll => {
                let Some(c) = self.clients.get_mut(&conn) else {
                    return;
                };
                let ed = &mut self.core.state;
                let Some(v) = ed.views.get_mut(&c.active) else {
                    return;
                };
                let count = ed.docs[&v.doc].buffer.snapshot().line_count();
                let top = v.top_line.saturating_add_signed(lines as isize);
                v.top_line = top.min(count.saturating_sub(1));
                c.dirty = true;
            }
            MouseAction::Unknown => {}
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
    }

    pub(crate) fn layout(&mut self, conn: u64, surfaces: &[SurfaceSize]) {
        let Some(c) = self.clients.get_mut(&conn) else {
            return;
        };
        if let Some(s) = surfaces.iter().find(|s| s.node == SURFACE) {
            c.surface = Some((s.rows, s.cols));
        }
        let (view, rows) = (c.active, c.rows());
        c.dirty = true;
        self.core.state.scroll_view(view, rows, SCROLL_MARGIN);
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
