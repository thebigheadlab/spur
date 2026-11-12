//! Input from clients: keys, paste, mouse and size.

use std::time::Instant;

use serde_json::json;
use spur_core::{CommandId, Invocation, Scoped};
use spur_keymap::{Dispatch, parse_keys};
use spur_rpc::{MouseAction, ServerMsg, Size, SurfaceSize};

use crate::frame::SURFACE;
use crate::session::{SCROLL_MARGIN, State};

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
        // `run_as` replaces the actor with the client's.
        let inv = Invocation::new(id, spur_core::Actor::User).args(json!({ "text": text }));
        if let Some(Err(e)) = self.run_as(conn, inv) {
            self.error(conn, e.message);
        }
        self.end_step(conn);
    }

    /// Ends the undo step of `conn`'s active view, so the next edit starts a new one.
    fn end_step(&mut self, conn: u64) {
        if let Some(c) = self.clients.get(&conn) {
            self.core.state.end_undo_step(c.active);
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
                if let Some(Err(e)) = self.run_as(conn, inv) {
                    self.error(conn, e.message);
                }
            }
            MouseAction::Scroll => {
                let Some(c) = self.clients.get_mut(&conn) else {
                    return;
                };
                self.core.state.scroll_by(c.active, lines as isize);
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
