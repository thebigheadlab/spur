//! Optional Vim key preset. Spur's core and default keys know nothing about it: this
//! crate adds its modes, its operator grammar, its command line, `.` repeat and macros,
//! all as commands in the `vim.` namespace, plus a keymap layer that binds them.
//!
//! Load it with [`install`] (the `spur` binary does so for `--preset vim`).

mod grammar;
mod objects;
mod record;
mod state;

use spur_core::Source;
use spur_keymap::{Layer, Session};

pub use state::{Cmdline, PendingOp, VimState};

/// The preset's keymap.
pub const KEYMAP: &str = include_str!("../keymap.toml");

/// Adds the Vim preset to a session and starts it in normal mode.
///
/// # Panics
///
/// Only if the bundled keymap or command ids are invalid, which tests rule out.
pub fn install(session: &mut Session) {
    grammar::register(&mut session.core).unwrap_or_else(|e| unreachable!("{e}"));
    session.core.state.motion_hook = Some(grammar::motion_hook);
    let layer = Layer::from_toml("vim", Source::Extension { id: "vim".into() }, KEYMAP)
        .unwrap_or_else(|e| unreachable!("{e}"));
    session.load_layer(layer);
    session.add_observer(Box::new(record::Recorder::default()));
    session.set_default_mode("normal");
}

/// A session with Spur's keys and the Vim preset on top.
pub fn session() -> Session {
    let mut s = Session::new();
    install(&mut s);
    s
}
